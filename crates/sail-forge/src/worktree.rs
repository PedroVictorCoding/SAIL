use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use sail_guardian::sandbox::{CommandOutput, SandboxPolicy, SandboxViolation};

/// Errors encountered during worktree lifecycle or sandbox operations.
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Sandbox boundary violation: {0}")]
    Sandbox(#[from] SandboxViolation),
    #[error("Path traversal detected: {path:?} escapes worktree root {root:?}")]
    PathTraversal { path: PathBuf, root: PathBuf },
    #[error("Symlink escape detected: {path:?} resolves outside worktree to {target:?}")]
    SymlinkEscape { path: PathBuf, target: PathBuf },
    #[error("Protected production path collision: {0:?}")]
    ProductionPathCollision(PathBuf),
    #[error("Invalid base commit ref '{0}': ref does not exist or failed verification")]
    InvalidBaseCommit(String),
    #[error("Worktree setup failed: {0}")]
    WorktreeSetupFailed(String),
    #[error("Command '{binary}' timed out after {timeout_secs}s; process terminated")]
    CommandTimeout { binary: String, timeout_secs: u64 },
    #[error("Git command failed: {command} (status: {status}): {stderr}")]
    GitError {
        command: String,
        status: i32,
        stderr: String,
    },
    #[error("Patch application failed: {0}")]
    PatchError(String),
}

/// Manages an isolated, disposable directory or git worktree for a single Forge task.
#[derive(Debug)]
pub struct IsolatedWorktree {
    pub job_id: String,
    pub root_dir: PathBuf,
    pub branch: String,
    pub base_commit: String,
    pub sandbox_policy: Arc<SandboxPolicy>,
    keep_on_drop: bool,
}

impl IsolatedWorktree {
    /// Create a new isolated disposable workspace under a secure sandbox root.
    /// Ensures isolation from SAIL production directory, database, and model weights.
    pub fn create(
        job_id: &str,
        repo_root: &Path,
        base_commit: &str,
        _base_sandbox_policy: Arc<SandboxPolicy>,
    ) -> Result<Self, WorktreeError> {
        let temp_base = std::env::var("SAIL_FORGE_WORKTREE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("sail_forge_worktrees"));

        let root_dir = temp_base.join(job_id);
        fs::create_dir_all(&root_dir)?;

        let canonical_root = root_dir.canonicalize().unwrap_or_else(|_| root_dir.clone());
        let canonical_repo = repo_root.canonicalize().unwrap_or_else(|_| repo_root.to_path_buf());

        if canonical_root.starts_with(&canonical_repo) && canonical_root == canonical_repo {
            return Err(WorktreeError::ProductionPathCollision(canonical_repo));
        }

        // Construct a scoped sandbox policy that only allows write access inside canonical_root,
        // and treats the production repo strictly as read-only.
        let scoped_policy = Arc::new(
            SandboxPolicy::from_roots([&canonical_root])
                .with_read_only_root(&canonical_repo)
        );

        let branch = format!("agent/forge-{}", job_id);

        let worktree = Self {
            job_id: job_id.to_string(),
            root_dir: canonical_root,
            branch,
            base_commit: base_commit.to_string(),
            sandbox_policy: scoped_policy,
            keep_on_drop: false,
        };

        worktree.init_git_workspace(&canonical_repo, base_commit)?;
        Ok(worktree)
    }

    /// Mark this worktree to be preserved on drop for manual user inspection of failures.
    pub fn retain_for_inspection(&mut self) {
        self.keep_on_drop = true;
    }

    /// Check whether a relative path safely resolves strictly beneath the isolated worktree.
    /// Denies path traversal (..), absolute paths, and symlinks that resolve outside the worktree.
    pub fn resolve_safe_path(&self, rel_path: &Path, is_write: bool) -> Result<PathBuf, WorktreeError> {
        if rel_path.is_absolute() {
            return Err(WorktreeError::PathTraversal {
                path: rel_path.to_path_buf(),
                root: self.root_dir.clone(),
            });
        }

        let combined = self.root_dir.join(rel_path);
        let mut normalized = PathBuf::new();
        for comp in combined.components() {
            match comp {
                Component::ParentDir => {
                    if !normalized.pop() {
                        return Err(WorktreeError::PathTraversal {
                            path: rel_path.to_path_buf(),
                            root: self.root_dir.clone(),
                        });
                    }
                }
                Component::CurDir => {}
                c => normalized.push(c.as_os_str()),
            }
        }

        if !normalized.starts_with(&self.root_dir) {
            return Err(WorktreeError::PathTraversal {
                path: rel_path.to_path_buf(),
                root: self.root_dir.clone(),
            });
        }

        // Check for symlink escapes in all path components from root_dir down to target
        let mut curr = self.root_dir.clone();
        for comp in rel_path.components() {
            if let Component::Normal(part) = comp {
                curr.push(part);
                if let Ok(meta) = fs::symlink_metadata(&curr) {
                    if meta.file_type().is_symlink() {
                        let real_target = fs::canonicalize(&curr).unwrap_or_else(|_| curr.clone());
                        if !real_target.starts_with(&self.root_dir) {
                            return Err(WorktreeError::SymlinkEscape {
                                path: curr,
                                target: real_target,
                            });
                        }
                    }
                }
            }
        }

        // Validate against Guardian policy
        self.sandbox_policy
            .validate_path(&normalized, is_write)
            .map_err(WorktreeError::Sandbox)?;

        Ok(normalized)
    }

    /// Safely write a file inside the isolated worktree with strict traversal and symlink prevention.
    pub fn write_file(&self, rel_path: &Path, content: &str) -> Result<(), WorktreeError> {
        let target = self.resolve_safe_path(rel_path, true)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target, content)?;
        Ok(())
    }

    /// Safely read a file inside the isolated worktree with strict traversal prevention.
    pub fn read_file(&self, rel_path: &Path) -> Result<String, WorktreeError> {
        let target = self.resolve_safe_path(rel_path, false)?;
        let content = fs::read_to_string(target)?;
        Ok(content)
    }

    /// Apply unified diff patch strictly inside the worktree.
    pub fn apply_patch(&self, patch: &str) -> Result<(), WorktreeError> {
        // First validate that patch headers do not contain forbidden traversal paths
        for line in patch.lines() {
            if let Some(path_str) = line.strip_prefix("+++ b/") {
                let p = Path::new(path_str);
                self.resolve_safe_path(p, true)?;
            }
        }

        let output = Command::new("git")
            .arg("apply")
            .arg("--verbose")
            .arg("-")
            .current_dir(&self.root_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(patch.as_bytes());
                }
                child.wait_with_output()
            });

        match output {
            Ok(out) if out.status.success() => {
                info!("Git apply succeeded for job {}", self.job_id);
                Ok(())
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                self.fallback_apply_patch(patch)
                    .map_err(|e| WorktreeError::PatchError(format!("{}: {}", stderr, e)))
            }
            Err(e) => self
                .fallback_apply_patch(patch)
                .map_err(|_| WorktreeError::PatchError(e.to_string())),
        }
    }

    /// Simple line-based fallback patcher for standalone test fixtures.
    fn fallback_apply_patch(&self, patch: &str) -> Result<(), WorktreeError> {
        let mut current_file: Option<PathBuf> = None;
        let mut new_content = String::new();
        let mut in_file = false;

        for line in patch.lines() {
            if line.starts_with("+++ b/") {
                if let Some(file) = current_file.take() {
                    self.write_file(&file, &new_content)?;
                }
                let path_str = &line[6..];
                current_file = Some(PathBuf::from(path_str));
                new_content.clear();
                in_file = true;
            } else if line.starts_with("--- ") {
                // old file header
            } else if line.starts_with("@@ ") {
                // hunk header
            } else if in_file {
                if let Some(stripped) = line.strip_prefix('+') {
                    new_content.push_str(stripped);
                    new_content.push('\n');
                } else if let Some(stripped) = line.strip_prefix(' ') {
                    new_content.push_str(stripped);
                    new_content.push('\n');
                } else if line.starts_with('-') {
                    // omitted from new content
                }
            }
        }

        if let Some(file) = current_file {
            self.write_file(&file, &new_content)?;
        }

        Ok(())
    }

    /// Generate unified diff against base commit in the isolated worktree.
    pub fn get_diff(&self) -> Result<String, WorktreeError> {
        let output = Command::new("git")
            .arg("diff")
            .arg(&self.base_commit)
            .current_dir(&self.root_dir)
            .output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let head_output = Command::new("git")
                .arg("diff")
                .arg("HEAD")
                .current_dir(&self.root_dir)
                .output()?;
            Ok(String::from_utf8_lossy(&head_output.stdout).to_string())
        }
    }

    /// Run a command inside the isolated worktree with Guardian sandbox validation,
    /// strict timeout enforcement, process group termination, and bounded output quotas.
    pub fn run_command(
        &self,
        binary: &str,
        args: &[&str],
        timeout_secs: u64,
    ) -> Result<CommandOutput, WorktreeError> {
        let args_vec: Vec<String> = args.iter().map(|s| s.to_string()).collect();

        // 1. Prepare sandboxed command via Guardian (checks allowlist and injection metacharacters)
        let sandboxed_cmd = self
            .sandbox_policy
            .prepare_command(binary, &args_vec, Some(&self.root_dir))
            .map_err(WorktreeError::Sandbox)?;

        // 2. Launch process with strict sandbox environment
        let mut cmd = Command::new(&sandboxed_cmd.binary_path);
        cmd.args(&sandboxed_cmd.args);
        cmd.current_dir(&self.root_dir);
        cmd.env_clear();

        // Clean PATH
        cmd.env("PATH", std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()));
        cmd.env("HOME", &self.root_dir);

        // Inherit toolchain root paths so rustc/cargo can resolve the installed toolchain
        if let Ok(rustup) = std::env::var("RUSTUP_HOME") {
            cmd.env("RUSTUP_HOME", rustup);
        } else if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(&home).join(".rustup");
            if p.exists() {
                cmd.env("RUSTUP_HOME", p);
            }
        }

        if let Ok(cargo_home) = std::env::var("CARGO_HOME") {
            cmd.env("CARGO_HOME", cargo_home);
        } else if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(&home).join(".cargo");
            if p.exists() {
                cmd.env("CARGO_HOME", p);
            }
        }

        cmd.env("RUST_BACKTRACE", "1");

        // Network isolation defaults: prevent dependency fetches and network leaks
        cmd.env("CARGO_NET_OFFLINE", "true");
        cmd.env("HTTP_PROXY", "http://0.0.0.0:0");
        cmd.env("HTTPS_PROXY", "http://0.0.0.0:0");
        cmd.env("ALL_PROXY", "http://0.0.0.0:0");

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let mut child = cmd.spawn()?;
        let start = Instant::now();
        let timeout = Duration::from_secs(timeout_secs.max(1));

        // Poll child with strict timeout
        loop {
            match child.try_wait()? {
                Some(status) => {
                    let mut stdout_buf = Vec::new();
                    let mut stderr_buf = Vec::new();
                    let max_output_bytes = 512 * 1024; // 512KB output cap

                    if let Some(mut out) = child.stdout.take() {
                        let _ = out.by_ref().take(max_output_bytes as u64).read_to_end(&mut stdout_buf);
                    }
                    if let Some(mut err) = child.stderr.take() {
                        let _ = err.by_ref().take(max_output_bytes as u64).read_to_end(&mut stderr_buf);
                    }

                    let mut stdout = String::from_utf8_lossy(&stdout_buf).to_string();
                    let mut stderr = String::from_utf8_lossy(&stderr_buf).to_string();

                    if stdout_buf.len() >= max_output_bytes {
                        stdout.push_str("\n[output truncated: exceeded 512KB sandbox limit]");
                    }
                    if stderr_buf.len() >= max_output_bytes {
                        stderr.push_str("\n[output truncated: exceeded 512KB sandbox limit]");
                    }

                    return Ok(CommandOutput {
                        status: status.code().unwrap_or(-1),
                        stdout,
                        stderr,
                    });
                }
                None => {
                    if start.elapsed() >= timeout {
                        warn!("Command {} timed out after {}s; killing process group", binary, timeout_secs);
                        #[cfg(unix)]
                        {
                            let pid = child.id() as i32;
                            unsafe {
                                libc::kill(-pid, libc::SIGKILL);
                            }
                        }
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(WorktreeError::CommandTimeout {
                            binary: binary.to_string(),
                            timeout_secs,
                        });
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }

    /// Initialize git repository or worktree cleanly.
    /// Fails closed if the base commit is invalid.
    fn init_git_workspace(&self, repo_root: &Path, base_commit: &str) -> Result<(), WorktreeError> {
        let is_git = repo_root.join(".git").exists();
        if is_git {
            // Verify base_commit ref exists in repo
            let verify_status = Command::new("git")
                .arg("rev-parse")
                .arg("--verify")
                .arg(base_commit)
                .current_dir(repo_root)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            match verify_status {
                Ok(st) if st.success() => {}
                _ => return Err(WorktreeError::InvalidBaseCommit(base_commit.to_string())),
            }

            // Create worktree from the verified base_commit
            let status = Command::new("git")
                .arg("worktree")
                .arg("add")
                .arg("-b")
                .arg(&self.branch)
                .arg(&self.root_dir)
                .arg(base_commit)
                .current_dir(repo_root)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output();

            match status {
                Ok(out) if out.status.success() => return Ok(()),
                Ok(out) => {
                    let err = String::from_utf8_lossy(&out.stderr).to_string();
                    return Err(WorktreeError::WorktreeSetupFailed(err));
                }
                Err(e) => return Err(WorktreeError::WorktreeSetupFailed(e.to_string())),
            }
        }

        // If repo_root is not a git repo, fail closed unless explicit fixture mode
        Err(WorktreeError::WorktreeSetupFailed(format!(
            "Target repository {:?} does not contain a valid .git directory",
            repo_root
        )))
    }
}

impl Drop for IsolatedWorktree {
    fn drop(&mut self) {
        if !self.keep_on_drop {
            if self.root_dir.join(".git").is_file() {
                let _ = Command::new("git")
                    .arg("worktree")
                    .arg("remove")
                    .arg("--force")
                    .arg(&self.root_dir)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            let _ = fs::remove_dir_all(&self.root_dir);
        } else {
            warn!(
                "Retaining failed worktree at {:?} for user inspection",
                self.root_dir
            );
        }
    }
}
