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
    #[error("Bubblewrap sandbox unavailable: {0}")]
    BwrapUnavailable(String),
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
        let canonical_repo = repo_root
            .canonicalize()
            .unwrap_or_else(|_| repo_root.to_path_buf());

        if canonical_root.starts_with(&canonical_repo) && canonical_root == canonical_repo {
            return Err(WorktreeError::ProductionPathCollision(canonical_repo));
        }

        // Construct a scoped sandbox policy that only allows write access inside canonical_root,
        // and treats the production repo strictly as read-only.
        let scoped_policy = Arc::new(
            SandboxPolicy::from_roots([&canonical_root]).with_read_only_root(&canonical_repo),
        );

        let branch = format!("agent/forge-{}", job_id);

        let mut worktree = Self {
            job_id: job_id.to_string(),
            root_dir: canonical_root,
            branch,
            base_commit: base_commit.to_string(),
            sandbox_policy: scoped_policy,
            keep_on_drop: false,
        };

        let pinned_sha = worktree.init_git_workspace(&canonical_repo, base_commit)?;
        worktree.base_commit = pinned_sha;
        Ok(worktree)
    }

    /// Mark this worktree to be preserved on drop for manual user inspection of failures.
    pub fn retain_for_inspection(&mut self) {
        self.keep_on_drop = true;
    }

    /// Check whether a relative path safely resolves strictly beneath the isolated worktree.
    /// Denies path traversal (..), absolute paths, and symlinks that resolve outside the worktree.
    pub fn resolve_safe_path(
        &self,
        rel_path: &Path,
        is_write: bool,
    ) -> Result<PathBuf, WorktreeError> {
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
        if patch.trim().is_empty() {
            return Err(WorktreeError::PatchError(
                "Refusing to apply empty patch".to_string(),
            ));
        }

        // First validate that patch headers do not contain forbidden traversal paths
        let mut saw_target = false;
        for line in patch.lines() {
            if let Some(path_str) = line.strip_prefix("+++ b/") {
                let p = Path::new(path_str.trim());
                self.resolve_safe_path(p, true)?;
                saw_target = true;
            } else if let Some(path_str) = line.strip_prefix("--- a/") {
                let p = Path::new(path_str.trim());
                self.resolve_safe_path(p, false)?;
            }
        }

        if !saw_target {
            return Err(WorktreeError::PatchError(
                "Patch does not contain any valid '+++ b/<path>' target headers".to_string(),
            ));
        }

        let output = Command::new("git")
            .arg("apply")
            .arg("--verbose")
            .arg("--recount")
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
                    if !patch.ends_with('\n') {
                        let _ = stdin.write_all(b"\n");
                    }
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
                    .map_err(|e| WorktreeError::PatchError(format!("{}: {}", stderr.trim(), e)))
            }
            Err(e) => self
                .fallback_apply_patch(patch)
                .map_err(|fe| WorktreeError::PatchError(format!("{}: {}", e, fe))),
        }
    }

    /// Context-verified hunk patcher for unified diffs when `git apply` rejects minor header/line-number drift.
    /// Never overwrites a file blindly; verifies that all removed/context lines match the existing file.
    fn fallback_apply_patch(&self, patch: &str) -> Result<(), WorktreeError> {
        #[derive(Default)]
        struct FilePatch {
            old_is_dev_null: bool,
            target_path: PathBuf,
            hunks: Vec<Vec<String>>,
        }

        let mut file_patches: Vec<FilePatch> = Vec::new();
        let mut pending_old_dev_null = false;

        for raw_line in patch.lines() {
            if let Some(rest) = raw_line.strip_prefix("--- ") {
                pending_old_dev_null = rest.trim() == "/dev/null";
            } else if let Some(rest) = raw_line.strip_prefix("+++ b/") {
                let target_path = PathBuf::from(rest.trim());
                self.resolve_safe_path(&target_path, true)?;
                file_patches.push(FilePatch {
                    old_is_dev_null: pending_old_dev_null,
                    target_path,
                    hunks: Vec::new(),
                });
                pending_old_dev_null = false;
            } else if raw_line.starts_with("@@ ") {
                if let Some(fp) = file_patches.last_mut() {
                    fp.hunks.push(Vec::new());
                }
            } else if raw_line.starts_with("diff --git ") || raw_line.starts_with("index ") {
                // metadata headers
            } else if let Some(fp) = file_patches.last_mut() {
                if raw_line.starts_with('+')
                    || raw_line.starts_with('-')
                    || raw_line.starts_with(' ')
                {
                    if fp.hunks.is_empty() {
                        fp.hunks.push(Vec::new());
                    }
                    if let Some(hunk) = fp.hunks.last_mut() {
                        hunk.push(raw_line.to_string());
                    }
                }
            }
        }

        if file_patches.is_empty() {
            return Err(WorktreeError::PatchError(
                "No valid file patches found in unified diff".to_string(),
            ));
        }

        // First compute all new file contents in memory so a multi-file patch is atomic
        let mut staged_writes: Vec<(PathBuf, String)> = Vec::new();

        for fp in file_patches {
            let full_target = self.resolve_safe_path(&fp.target_path, true)?;
            let existing = if full_target.exists() {
                fs::read_to_string(&full_target)?
            } else if fp.old_is_dev_null {
                String::new()
            } else {
                return Err(WorktreeError::PatchError(format!(
                    "Target file {:?} does not exist and patch is not a new-file diff",
                    fp.target_path
                )));
            };

            let mut file_lines: Vec<String> = existing.lines().map(|s| s.to_string()).collect();
            let mut search_from = 0usize;

            for hunk in &fp.hunks {
                let mut old_block: Vec<String> = Vec::new();
                let mut new_block: Vec<String> = Vec::new();
                for hline in hunk {
                    if let Some(added) = hline.strip_prefix('+') {
                        new_block.push(added.to_string());
                    } else if let Some(removed) = hline.strip_prefix('-') {
                        old_block.push(removed.to_string());
                    } else if let Some(ctx) = hline.strip_prefix(' ') {
                        old_block.push(ctx.to_string());
                        new_block.push(ctx.to_string());
                    }
                }

                if old_block.is_empty() {
                    if file_lines.is_empty() {
                        file_lines = new_block;
                    } else {
                        return Err(WorktreeError::PatchError(format!(
                            "Hunk with no context cannot be unambiguously applied to non-empty file {:?}",
                            fp.target_path
                        )));
                    }
                } else {
                    let match_idx = (search_from
                        ..=file_lines.len().saturating_sub(old_block.len()))
                        .find(|&i| {
                            file_lines.get(i..i + old_block.len()) == Some(old_block.as_slice())
                        });

                    if let Some(idx) = match_idx {
                        let end_idx = idx + old_block.len();
                        let new_len = new_block.len();
                        file_lines.splice(idx..end_idx, new_block);
                        search_from = idx + new_len;
                    } else {
                        return Err(WorktreeError::PatchError(format!(
                            "Hunk context mismatch in {:?}",
                            fp.target_path
                        )));
                    }
                }
            }

            let mut updated = file_lines.join("\n");
            if !updated.is_empty() {
                updated.push('\n');
            }
            staged_writes.push((fp.target_path, updated));
        }

        for (rel_path, content) in staged_writes {
            self.write_file(&rel_path, &content)?;
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

    /// Run a command inside the isolated worktree with Bubblewrap OS sandbox namespace isolation,
    /// Guardian policy validation, strict timeout enforcement, process group termination,
    /// and non-blocking concurrent output draining to prevent pipe deadlocks.
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

        let bwrap_bin = if let Ok(custom) = std::env::var("SAIL_BWRAP_BIN") {
            PathBuf::from(custom)
        } else if let Ok(path_var) = std::env::var("PATH") {
            let mut found = None;
            for dir in std::env::split_paths(&path_var) {
                let candidate = dir.join("bwrap");
                if candidate.is_file() {
                    found = Some(candidate);
                    break;
                }
            }
            found.unwrap_or_else(|| PathBuf::from("/usr/bin/bwrap"))
        } else {
            PathBuf::from("/usr/bin/bwrap")
        };

        if !bwrap_bin.exists() {
            return Err(WorktreeError::BwrapUnavailable(format!(
                "Mandatory Bubblewrap sandbox binary not found at {:?}. Sandbox is required and fails closed.",
                bwrap_bin
            )));
        }

        // Clean PATH including ~/.cargo/bin and standard system paths
        let clean_path = if let Ok(path) = std::env::var("PATH") {
            if path.contains(".cargo/bin") {
                path
            } else {
                format!(
                    "{}:/home/main/.cargo/bin:/usr/local/bin:/usr/bin:/bin",
                    path
                )
            }
        } else {
            "/home/main/.cargo/bin:/usr/local/bin:/usr/bin:/bin".to_string()
        };

        let rustup_home = std::env::var("RUSTUP_HOME").ok().or_else(|| {
            let p = PathBuf::from("/home/main/.rustup");
            if p.exists() {
                Some(p.to_string_lossy().to_string())
            } else {
                None
            }
        });

        let cargo_home = std::env::var("CARGO_HOME").ok().or_else(|| {
            let p = PathBuf::from("/home/main/.cargo");
            if p.exists() {
                Some(p.to_string_lossy().to_string())
            } else {
                None
            }
        });

        let mut cmd = Command::new(&bwrap_bin);
        // Minimal confidential allowlisted filesystem:
        // System libraries and binaries are mounted read-only:
        if Path::new("/usr").exists() {
            cmd.arg("--ro-bind").arg("/usr").arg("/usr");
        }
        if Path::new("/lib").exists() {
            cmd.arg("--ro-bind").arg("/lib").arg("/lib");
        }
        if Path::new("/lib64").exists() {
            cmd.arg("--ro-bind").arg("/lib64").arg("/lib64");
        }
        if Path::new("/bin").exists() {
            cmd.arg("--ro-bind").arg("/bin").arg("/bin");
        }
        cmd.arg("--ro-bind-try")
            .arg("/etc/ld.so.cache")
            .arg("/etc/ld.so.cache");
        cmd.arg("--ro-bind-try")
            .arg("/etc/ld.so.conf")
            .arg("/etc/ld.so.conf");
        cmd.arg("--ro-bind-try")
            .arg("/etc/ld.so.conf.d")
            .arg("/etc/ld.so.conf.d");
        cmd.arg("--ro-bind-try")
            .arg("/etc/alternatives")
            .arg("/etc/alternatives");
        cmd.arg("--ro-bind-try").arg("/etc/ssl").arg("/etc/ssl");
        cmd.arg("--ro-bind-try").arg("/etc/pki").arg("/etc/pki");

        // Isolated user home: empty tmpfs for /tmp, and read-only tmpfs at /home/main with ONLY cargo & rustup toolchain mounted read-only
        cmd.arg("--tmpfs").arg("/tmp");
        cmd.arg("--tmpfs").arg("/home");
        cmd.arg("--tmpfs").arg("/home/main");
        if let Some(ref cargo) = cargo_home {
            let cargo_p = Path::new(cargo);
            if cargo_p.exists() {
                cmd.arg("--ro-bind").arg(cargo).arg(cargo);
            }
        }
        if let Some(ref rustup) = rustup_home {
            let rustup_p = Path::new(rustup);
            if rustup_p.exists() {
                cmd.arg("--ro-bind").arg(rustup).arg(rustup);
            }
        }
        cmd.arg("--remount-ro").arg("/home/main");
        cmd.arg("--remount-ro").arg("/home");

        // Isolated worktree: root_dir is strictly the only directory writable outside tmpfs
        cmd.arg("--bind").arg(&self.root_dir).arg(&self.root_dir);
        cmd.arg("--chdir").arg(&self.root_dir);

        // Security isolation flags (defense-in-depth namespace containment)
        cmd.arg("--proc").arg("/proc");
        cmd.arg("--dev").arg("/dev");
        cmd.arg("--unshare-net");
        cmd.arg("--unshare-pid");
        cmd.arg("--unshare-ipc");
        cmd.arg("--unshare-uts");
        cmd.arg("--new-session");
        cmd.arg("--die-with-parent");
        cmd.arg("--clearenv");

        // Environment variables
        cmd.arg("--setenv").arg("PATH").arg(&clean_path);
        cmd.arg("--setenv").arg("HOME").arg("/tmp");
        if let Some(ref rustup) = rustup_home {
            cmd.arg("--setenv").arg("RUSTUP_HOME").arg(rustup);
        }
        if let Some(ref cargo) = cargo_home {
            cmd.arg("--setenv").arg("CARGO_HOME").arg(cargo);
        }
        cmd.arg("--setenv").arg("CARGO_NET_OFFLINE").arg("true");
        cmd.arg("--setenv").arg("RUST_BACKTRACE").arg("1");

        // Binary and arguments to execute
        cmd.arg(&sandboxed_cmd.binary_path);
        for a in &sandboxed_cmd.args {
            cmd.arg(a);
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let mut child = cmd.spawn()?;

        let max_output_bytes = 512 * 1024; // 512KB output cap
        let mut stdout_pipe = child.stdout.take().expect("stdout must be piped");
        let mut stderr_pipe = child.stderr.take().expect("stderr must be piped");

        // Concurrently drain stdout in background thread to prevent pipe buffer deadlock
        let stdout_handle = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout_pipe
                .by_ref()
                .take(max_output_bytes as u64)
                .read_to_end(&mut buf);
            let mut scratch = [0u8; 8192];
            let mut excess = false;
            while let Ok(n) = stdout_pipe.read(&mut scratch) {
                if n == 0 {
                    break;
                }
                excess = true;
            }
            (buf, excess)
        });

        // Concurrently drain stderr in background thread to prevent pipe buffer deadlock
        let stderr_handle = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr_pipe
                .by_ref()
                .take(max_output_bytes as u64)
                .read_to_end(&mut buf);
            let mut scratch = [0u8; 8192];
            let mut excess = false;
            while let Ok(n) = stderr_pipe.read(&mut scratch) {
                if n == 0 {
                    break;
                }
                excess = true;
            }
            (buf, excess)
        });

        let start = Instant::now();
        let timeout = Duration::from_secs(timeout_secs.max(1));

        // Poll child with timeout; streams are drained concurrently so child never blocks on stdout/stderr
        loop {
            match child.try_wait()? {
                Some(status) => {
                    let (stdout_buf, stdout_excess) = stdout_handle.join().unwrap_or_default();
                    let (stderr_buf, stderr_excess) = stderr_handle.join().unwrap_or_default();

                    let mut stdout = String::from_utf8_lossy(&stdout_buf).to_string();
                    let mut stderr = String::from_utf8_lossy(&stderr_buf).to_string();

                    if stdout_excess || stdout_buf.len() >= max_output_bytes {
                        stdout.push_str("\n[output truncated: exceeded 512KB sandbox limit]");
                    }
                    if stderr_excess || stderr_buf.len() >= max_output_bytes {
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
                        warn!(
                            "Command {} timed out after {}s; killing process group",
                            binary, timeout_secs
                        );
                        #[cfg(unix)]
                        {
                            let pid = child.id() as i32;
                            unsafe {
                                libc::kill(-pid, libc::SIGKILL);
                            }
                        }
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = stdout_handle.join();
                        let _ = stderr_handle.join();
                        return Err(WorktreeError::CommandTimeout {
                            binary: binary.to_string(),
                            timeout_secs,
                        });
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }

    /// Initialize a completely standalone git repository clone at the pinned SHA.
    /// Does NOT write into or link back to `repo_root/.git`.
    /// Fails closed if the base commit is invalid.
    fn init_git_workspace(
        &self,
        repo_root: &Path,
        base_commit: &str,
    ) -> Result<String, WorktreeError> {
        let is_git = repo_root.join(".git").exists();
        if is_git {
            // Resolve base_commit to an exact 40-char commit SHA in repo_root (read-only query)
            let rev_out = Command::new("git")
                .arg("rev-parse")
                .arg("--verify")
                .arg(format!("{}^{{commit}}", base_commit))
                .current_dir(repo_root)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output();

            let pinned_sha = match rev_out {
                Ok(out) if out.status.success() => {
                    String::from_utf8_lossy(&out.stdout).trim().to_string()
                }
                _ => return Err(WorktreeError::InvalidBaseCommit(base_commit.to_string())),
            };

            if pinned_sha.is_empty() {
                return Err(WorktreeError::InvalidBaseCommit(base_commit.to_string()));
            }

            // Ensure root_dir is empty before cloning into it
            if self.root_dir.exists() {
                if let Ok(entries) = fs::read_dir(&self.root_dir) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.is_dir() {
                            let _ = fs::remove_dir_all(&p);
                        } else {
                            let _ = fs::remove_file(&p);
                        }
                    }
                }
            }

            // Create a standalone local clone without hardlinks so `.git` is an independent directory
            let clone_out = Command::new("git")
                .arg("clone")
                .arg("--local")
                .arg("--no-hardlinks")
                .arg("--no-checkout")
                .arg(repo_root)
                .arg(&self.root_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output();

            match clone_out {
                Ok(out) if out.status.success() => {}
                Ok(out) => {
                    let err = String::from_utf8_lossy(&out.stderr).to_string();
                    return Err(WorktreeError::WorktreeSetupFailed(err));
                }
                Err(e) => return Err(WorktreeError::WorktreeSetupFailed(e.to_string())),
            }

            // Detach origin remote so nothing inside the worktree points back to the production repo
            let _ = Command::new("git")
                .arg("remote")
                .arg("remove")
                .arg("origin")
                .current_dir(&self.root_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            // Checkout the isolated branch at the exact pinned SHA
            let checkout_out = Command::new("git")
                .arg("checkout")
                .arg("-B")
                .arg(&self.branch)
                .arg(&pinned_sha)
                .current_dir(&self.root_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output();

            match checkout_out {
                Ok(out) if out.status.success() => {}
                Ok(out) => {
                    let err = String::from_utf8_lossy(&out.stderr).to_string();
                    return Err(WorktreeError::WorktreeSetupFailed(err));
                }
                Err(e) => return Err(WorktreeError::WorktreeSetupFailed(e.to_string())),
            }

            let _ = Command::new("git")
                .args(["config", "user.name", "SAIL Forge"])
                .current_dir(&self.root_dir)
                .status();
            let _ = Command::new("git")
                .args(["config", "user.email", "forge@sail.local"])
                .current_dir(&self.root_dir)
                .status();

            return Ok(pinned_sha);
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
            let _ = fs::remove_dir_all(&self.root_dir);
        } else {
            warn!(
                "Retaining failed worktree at {:?} for user inspection",
                self.root_dir
            );
        }
    }
}
