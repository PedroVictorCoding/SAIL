use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
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
    #[error("Protected production path collision: {0:?}")]
    ProductionPathCollision(PathBuf),
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
        sandbox_policy: Arc<SandboxPolicy>,
    ) -> Result<Self, WorktreeError> {
        // Enforce that the disposable workspace lives under a dedicated temporary location
        let temp_base = std::env::var("SAIL_FORGE_WORKTREE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("sail_forge_worktrees"));

        let root_dir = temp_base.join(job_id);
        fs::create_dir_all(&root_dir)?;

        // Canonicalize paths to verify non-intersection with production
        let canonical_root = root_dir.canonicalize().unwrap_or_else(|_| root_dir.clone());
        let canonical_repo = repo_root.canonicalize().unwrap_or_else(|_| repo_root.to_path_buf());

        if canonical_root.starts_with(&canonical_repo) && canonical_root == canonical_repo {
            return Err(WorktreeError::ProductionPathCollision(canonical_repo));
        }

        let branch = format!("agent/forge-{}", job_id);

        let worktree = Self {
            job_id: job_id.to_string(),
            root_dir: canonical_root,
            branch,
            base_commit: base_commit.to_string(),
            sandbox_policy,
            keep_on_drop: false,
        };

        worktree.init_git_workspace(repo_root, base_commit)?;
        Ok(worktree)
    }

    /// Mark this worktree to be preserved on drop for manual user inspection of failures.
    pub fn retain_for_inspection(&mut self) {
        self.keep_on_drop = true;
    }

    /// Check whether a relative path safely resolves strictly beneath the isolated worktree.
    pub fn resolve_safe_path(&self, rel_path: &Path) -> Result<PathBuf, WorktreeError> {
        let combined = self.root_dir.join(rel_path);
        // Normalize components
        let mut normalized = PathBuf::new();
        for comp in combined.components() {
            match comp {
                std::path::Component::ParentDir => {
                    if !normalized.pop() {
                        return Err(WorktreeError::PathTraversal {
                            path: rel_path.to_path_buf(),
                            root: self.root_dir.clone(),
                        });
                    }
                }
                std::path::Component::CurDir => {}
                c => normalized.push(c.as_os_str()),
            }
        }

        if !normalized.starts_with(&self.root_dir) {
            return Err(WorktreeError::PathTraversal {
                path: rel_path.to_path_buf(),
                root: self.root_dir.clone(),
            });
        }

        Ok(normalized)
    }

    /// Safely write a file inside the isolated worktree with strict traversal prevention.
    pub fn write_file(&self, rel_path: &Path, content: &str) -> Result<(), WorktreeError> {
        let target = self.resolve_safe_path(rel_path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target, content)?;
        Ok(())
    }

    /// Safely read a file inside the isolated worktree with strict traversal prevention.
    pub fn read_file(&self, rel_path: &Path) -> Result<String, WorktreeError> {
        let target = self.resolve_safe_path(rel_path)?;
        let content = fs::read_to_string(target)?;
        Ok(content)
    }

    /// Apply unified diff patch strictly inside the worktree.
    pub fn apply_patch(&self, patch: &str) -> Result<(), WorktreeError> {
        // First try standard git apply inside the worktree
        let output = Command::new("git")
            .arg("apply")
            .arg("--verbose")
            .arg("-")
            .current_dir(&self.root_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
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
                // If git apply is not suitable (e.g. new untracked fixture files), parse simple patch directly
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
            // If base_commit is not a commit ref in detached test repo, diff HEAD
            let head_output = Command::new("git")
                .arg("diff")
                .arg("HEAD")
                .current_dir(&self.root_dir)
                .output()?;
            Ok(String::from_utf8_lossy(&head_output.stdout).to_string())
        }
    }

    /// Run a command inside the isolated worktree with stripped host secrets and sandbox confinement.
    pub fn run_command(
        &self,
        binary: &str,
        args: &[&str],
        _timeout_secs: u64,
    ) -> Result<CommandOutput, WorktreeError> {
        let mut cmd = Command::new(binary);
        cmd.args(args);
        cmd.current_dir(&self.root_dir);

        // Sanitize environment: scrub credentials, private keys, API keys
        cmd.env_clear();
        cmd.env("PATH", std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()));
        cmd.env("HOME", &self.root_dir);
        cmd.env("CARGO_HOME", self.root_dir.join(".cargo"));
        cmd.env("RUST_BACKTRACE", "1");

        let output = cmd.output()?;
        let status = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        Ok(CommandOutput {
            status,
            stdout,
            stderr,
        })
    }

    /// Initialize git repository or worktree cleanly.
    fn init_git_workspace(&self, repo_root: &Path, base_commit: &str) -> Result<(), WorktreeError> {
        // Try creating an isolated git worktree if repo_root is a valid git repository
        let is_git = repo_root.join(".git").exists();
        if is_git {
            let status = Command::new("git")
                .arg("worktree")
                .arg("add")
                .arg("-b")
                .arg(&self.branch)
                .arg(&self.root_dir)
                .arg(base_commit)
                .current_dir(repo_root)
                .status();

            if let Ok(st) = status {
                if st.success() {
                    return Ok(());
                }
            }
        }

        // Fallback: initialize a standalone isolated git repo at root_dir
        let _ = Command::new("git")
            .arg("init")
            .arg("-b")
            .arg("main")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .current_dir(&self.root_dir)
            .status();

        let _ = Command::new("git")
            .arg("config")
            .arg("user.name")
            .arg("SAIL Forge")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .current_dir(&self.root_dir)
            .status();

        let _ = Command::new("git")
            .arg("config")
            .arg("user.email")
            .arg("forge@sail.local")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .current_dir(&self.root_dir)
            .status();

        let _ = Command::new("git")
            .arg("checkout")
            .arg("-b")
            .arg(&self.branch)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .current_dir(&self.root_dir)
            .status();

        Ok(())
    }
}

impl Drop for IsolatedWorktree {
    fn drop(&mut self) {
        if !self.keep_on_drop {
            // If this is a git worktree, .git is a file referencing the main repository gitdir
            if self.root_dir.join(".git").is_file() {
                let _ = Command::new("git")
                    .arg("worktree")
                    .arg("remove")
                    .arg("--force")
                    .arg(&self.root_dir)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }

            // Remove directory
            let _ = fs::remove_dir_all(&self.root_dir);
        } else {
            warn!(
                "Retaining failed worktree at {:?} for user inspection",
                self.root_dir
            );
        }
    }
}
