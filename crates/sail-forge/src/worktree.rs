use std::ffi::CString;
use std::fs;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
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
    #[error("Invalid job ID: expected 1–128 ASCII letters, digits, underscores or hyphens")]
    InvalidJobId,
    #[error("Unsafe workspace root {0:?}: {1}")]
    UnsafeWorkspaceRoot(PathBuf, String),
    #[error("Workspace destination already exists: {0:?}")]
    WorkspaceExists(PathBuf),
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

/// Owns only a freshly allocated directory; cleanup never trusts public path fields.
#[derive(Debug)]
struct WorkspaceAllocation {
    base: fs::File,
    directory: fs::File,
    name: CString,
    path: PathBuf,
}

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

fn open_directory(parent: i32, name: &CString, resolve: u64) -> std::io::Result<fs::File> {
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve,
    };
    // SAFETY: name and initialized open_how live for the syscall; own the returned fd.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent,
            name.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { fs::File::from_raw_fd(fd as i32) })
}

fn open_directory_at(parent: &fs::File, name: &CString) -> std::io::Result<fs::File> {
    // NO_XDEV | NO_MAGICLINKS | NO_SYMLINKS | BENEATH: no mount or link traversal.
    open_directory(parent.as_raw_fd(), name, 0x01 | 0x02 | 0x04 | 0x08)
}

fn same_directory(left: &fs::File, right: &fs::File) -> std::io::Result<bool> {
    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

impl WorkspaceAllocation {
    fn new(base: &Path, job_id: &str, repo: &Path) -> Result<Self, WorktreeError> {
        let unsafe_root = |reason: String| WorktreeError::UnsafeWorkspaceRoot(base.into(), reason);
        // Resolve the parent before creation, rejecting symlinks in every component.
        if !base.is_absolute()
            || base
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(unsafe_root("absolute canonical path required".into()));
        }
        let mut component = PathBuf::new();
        for part in base.components() {
            component.push(part);
            match fs::symlink_metadata(&component) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(unsafe_root("symlink component denied".into()));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && component == base => {}
                Err(e) => return Err(unsafe_root(e.to_string())),
            }
        }
        if base.starts_with(repo) || repo.starts_with(base) {
            return Err(WorktreeError::ProductionPathCollision(base.into()));
        }
        let parent = base
            .parent()
            .ok_or_else(|| unsafe_root("parent required".into()))?;
        let parent_name =
            CString::new(parent.as_os_str().as_bytes()).map_err(|e| unsafe_root(e.to_string()))?;
        let parent_fd = open_directory(libc::AT_FDCWD, &parent_name, 0x02 | 0x04)
            .map_err(|e| unsafe_root(e.to_string()))?;
        let basename = base
            .file_name()
            .ok_or_else(|| unsafe_root("directory name required".into()))?;
        let basename = CString::new(basename.as_bytes()).map_err(|e| unsafe_root(e.to_string()))?;
        // Create relative to a pinned parent, never through a checked-then-reopened pathname.
        if unsafe { libc::mkdirat(parent_fd.as_raw_fd(), basename.as_ptr(), 0o700) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(unsafe_root(error.to_string()));
            }
        }
        let base_fd =
            open_directory_at(&parent_fd, &basename).map_err(|e| unsafe_root(e.to_string()))?;
        let meta = base_fd.metadata()?;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(unsafe_root(
                "must be owned by current user with no group/world permissions".into(),
            ));
        }
        let name = CString::new(job_id).map_err(|_| WorktreeError::InvalidJobId)?;
        // SAFETY: validated single-component name relative to the pinned base.
        if unsafe { libc::mkdirat(base_fd.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            let error = std::io::Error::last_os_error();
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                WorktreeError::WorkspaceExists(base.join(job_id))
            } else {
                WorktreeError::Io(error)
            });
        }
        let directory = open_directory_at(&base_fd, &name)?;
        Ok(Self {
            base: base_fd,
            directory,
            name,
            path: base.join(job_id),
        })
    }

    fn cleanup(&self) -> std::io::Result<()> {
        let current = open_directory_at(&self.base, &self.name)?;
        if !same_directory(&current, &self.directory)? {
            return Err(std::io::Error::other(
                "allocated directory identity changed; leaving replacement untouched",
            ));
        }
        clear_directory(&self.directory)?;
        // Check again after traversal. Never recursively reopen the caller's path.
        let current = open_directory_at(&self.base, &self.name)?;
        if !same_directory(&current, &self.directory)? {
            return Err(std::io::Error::other("directory replaced during cleanup"));
        }
        if unsafe {
            libc::unlinkat(
                self.base.as_raw_fd(),
                self.name.as_ptr(),
                libc::AT_REMOVEDIR,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Traverse pinned directory descriptors; links are removed as entries, never followed.
fn clear_directory(directory: &fs::File) -> std::io::Result<()> {
    for entry in fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))? {
        let entry = entry?;
        let name = CString::new(entry.file_name().as_bytes())?;
        let flags = match open_directory_at(directory, &name) {
            Ok(child) => {
                // Do not cross a mounted filesystem during cleanup.
                if child.metadata()?.dev() != directory.metadata()?.dev() {
                    return Err(std::io::Error::other("mount boundary in workspace"));
                }
                clear_directory(&child)?;
                let current = open_directory_at(directory, &name)?;
                if !same_directory(&current, &child)? {
                    return Err(std::io::Error::other("child replaced during cleanup"));
                }
                libc::AT_REMOVEDIR
            }
            Err(e) if matches!(e.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP)) => 0,
            Err(e) => return Err(e),
        };
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), flags) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
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
    allocation: WorkspaceAllocation,
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

        Self::create_in(job_id, repo_root, base_commit, &temp_base)
    }

    fn create_in(
        job_id: &str,
        repo_root: &Path,
        base_commit: &str,
        temp_base: &Path,
    ) -> Result<Self, WorktreeError> {
        if job_id.is_empty()
            || job_id.len() > 128
            || !job_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err(WorktreeError::InvalidJobId);
        }
        let canonical_repo = repo_root.canonicalize()?;
        let allocation = WorkspaceAllocation::new(temp_base, job_id, &canonical_repo)?;
        let canonical_root = allocation.path.clone();

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
            allocation,
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

        let toolchain = SandboxToolchain::from_env();
        let clean_path = toolchain.sandbox_path(std::env::var("PATH").ok().as_deref());
        let rustup_home = toolchain
            .rustup_home
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());
        let cargo_home = toolchain
            .cargo_home
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());

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
        // Only the parts of CARGO_HOME a build needs: binaries and the offline package
        // cache. Never `credentials.toml` (registry tokens) or `config.toml` (which can
        // hold tokens too): build scripts run in here, and anything they can read they can
        // write into the patch.
        if let Some(ref cargo) = toolchain.cargo_home {
            for sub in SANDBOX_CARGO_HOME_ENTRIES {
                let src = cargo.join(sub);
                if src.exists() {
                    cmd.arg("--ro-bind").arg(&src).arg(&src);
                }
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

        // The binary Guardian resolved (for example `$HOME/.cargo/bin/cargo`) may sit outside
        // the directories mounted above; mount that one file read-only so it can run.
        if sandboxed_cmd.binary_path.is_absolute() && sandboxed_cmd.binary_path.is_file() {
            cmd.arg("--ro-bind")
                .arg(&sandboxed_cmd.binary_path)
                .arg(&sandboxed_cmd.binary_path);
        }

        // Binary and arguments to execute
        cmd.arg(&sandboxed_cmd.binary_path);
        for a in &sandboxed_cmd.args {
            cmd.arg(a);
        }

        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
            let limits = SandboxLimits::from_env().clamped_to_current();
            // SAFETY: only async-signal-safe syscalls on stack data run between fork and
            // exec. bwrap and everything it starts inherit these limits.
            unsafe {
                cmd.pre_exec(move || limits.apply());
            }
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
            if let Err(error) = self.allocation.cleanup() {
                warn!(
                    "Refusing or unable to clean workspace {:?}: {}",
                    self.allocation.path, error
                );
            }
        } else {
            warn!(
                "Retaining failed worktree at {:?} for user inspection",
                self.root_dir
            );
        }
    }
}

/// `CARGO_HOME` entries mounted (read-only) into the sandbox.
pub const SANDBOX_CARGO_HOME_ENTRIES: [&str; 3] = ["bin", "registry", "git"];

/// Where the Rust toolchain lives on the host, as mounted into the sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxToolchain {
    pub cargo_home: Option<PathBuf>,
    pub rustup_home: Option<PathBuf>,
}

impl SandboxToolchain {
    /// `CARGO_HOME`/`RUSTUP_HOME`, else `$HOME/.cargo` and `$HOME/.rustup`, else the
    /// original `/home/main` defaults. Only existing absolute directories are used.
    pub fn from_env() -> Self {
        Self::resolve(
            std::env::var_os("CARGO_HOME").map(PathBuf::from),
            std::env::var_os("RUSTUP_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        )
    }

    fn resolve(cargo: Option<PathBuf>, rustup: Option<PathBuf>, home: Option<PathBuf>) -> Self {
        let pick = |explicit: Option<PathBuf>, dot: &str| {
            explicit
                .into_iter()
                .chain(home.as_ref().map(|h| h.join(dot)))
                .chain(std::iter::once(Path::new("/home/main").join(dot)))
                .find(|p| p.is_absolute() && p.is_dir())
        };
        Self {
            cargo_home: pick(cargo, ".cargo"),
            rustup_home: pick(rustup, ".rustup"),
        }
    }

    /// PATH inside the sandbox: the host PATH plus the cargo bin directory and system dirs.
    pub fn sandbox_path(&self, host_path: Option<&str>) -> String {
        let mut parts: Vec<String> = host_path
            .unwrap_or("")
            .split(':')
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect();
        if let Some(cargo) = &self.cargo_home {
            let bin = cargo.join("bin").to_string_lossy().to_string();
            if !parts.contains(&bin) {
                parts.push(bin);
            }
        }
        for sys in ["/usr/local/bin", "/usr/bin", "/bin"] {
            if !parts.iter().any(|p| p == sys) {
                parts.push(sys.to_string());
            }
        }
        parts.join(":")
    }
}

/// Resource limits applied to every sandboxed command (and inherited by its children).
///
/// Memory and process-count limits need cgroups, which this synchronous Bubblewrap path
/// does not have; the dedicated runner service (`sail-runner`) applies those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxLimits {
    /// Largest file a command may write (`RLIMIT_FSIZE`).
    pub max_file_bytes: u64,
    /// Open file descriptors (`RLIMIT_NOFILE`).
    pub max_open_files: u64,
}

impl SandboxLimits {
    pub const DEFAULT_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
    pub const DEFAULT_MAX_OPEN_FILES: u64 = 16_384;

    /// Defaults, overridden by `SAIL_FORGE_SANDBOX_MAX_FILE_BYTES` and
    /// `SAIL_FORGE_SANDBOX_MAX_OPEN_FILES` when they parse as positive integers.
    pub fn from_env() -> Self {
        let read = |name: &str, default: u64| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(default)
        };
        Self {
            max_file_bytes: read(
                "SAIL_FORGE_SANDBOX_MAX_FILE_BYTES",
                Self::DEFAULT_MAX_FILE_BYTES,
            ),
            max_open_files: read(
                "SAIL_FORGE_SANDBOX_MAX_OPEN_FILES",
                Self::DEFAULT_MAX_OPEN_FILES,
            ),
        }
    }

    /// Never ask for more than the current hard limits: an unprivileged process cannot
    /// raise them, and `setrlimit` would fail.
    #[cfg(unix)]
    pub fn clamped_to_current(self) -> Self {
        let hard = |resource| {
            let mut cur = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: getrlimit only writes into the provided struct.
            if unsafe { libc::getrlimit(resource, &mut cur) } == 0 {
                cur.rlim_max
            } else {
                libc::RLIM_INFINITY
            }
        };
        Self {
            max_file_bytes: self.max_file_bytes.min(hard(libc::RLIMIT_FSIZE)),
            max_open_files: self.max_open_files.min(hard(libc::RLIMIT_NOFILE)),
        }
    }

    /// Apply in the forked child before exec. Async-signal-safe.
    #[cfg(unix)]
    fn apply(self) -> std::io::Result<()> {
        // SAFETY: plain syscalls on stack values.
        unsafe {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            for (resource, value) in [
                (libc::RLIMIT_CORE, 0),
                (libc::RLIMIT_FSIZE, self.max_file_bytes),
                (libc::RLIMIT_NOFILE, self.max_open_files),
            ] {
                let bound = libc::rlimit {
                    rlim_cur: value,
                    rlim_max: value,
                };
                if libc::setrlimit(resource, &bound) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod sandbox_config_tests {
    use super::*;

    #[test]
    fn toolchain_prefers_explicit_then_home() {
        let tmp = std::env::temp_dir().join(format!("sail-tc-{}", uuid::Uuid::new_v4()));
        let explicit = tmp.join("cargo");
        let home = tmp.join("home");
        fs::create_dir_all(&explicit).unwrap();
        fs::create_dir_all(home.join(".cargo")).unwrap();
        fs::create_dir_all(home.join(".rustup")).unwrap();

        let tc = SandboxToolchain::resolve(Some(explicit.clone()), None, Some(home.clone()));
        assert_eq!(tc.cargo_home.as_deref(), Some(explicit.as_path()));
        assert_eq!(tc.rustup_home, Some(home.join(".rustup")));

        // A missing or relative explicit directory falls through to $HOME.
        let tc = SandboxToolchain::resolve(Some("rel".into()), None, Some(home.clone()));
        assert_eq!(tc.cargo_home, Some(home.join(".cargo")));
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn sandbox_path_adds_cargo_bin_once() {
        let tc = SandboxToolchain {
            cargo_home: Some("/opt/cargo".into()),
            rustup_home: None,
        };
        let p = tc.sandbox_path(Some("/opt/cargo/bin:/usr/bin"));
        assert_eq!(p, "/opt/cargo/bin:/usr/bin:/usr/local/bin:/bin");
        assert_eq!(
            SandboxToolchain {
                cargo_home: None,
                rustup_home: None
            }
            .sandbox_path(None),
            "/usr/local/bin:/usr/bin:/bin"
        );
    }

    #[test]
    fn limits_never_exceed_current_hard_limits() {
        let l = SandboxLimits {
            max_file_bytes: u64::MAX,
            max_open_files: u64::MAX,
        }
        .clamped_to_current();
        let mut cur = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut cur) };
        assert_eq!(l.max_open_files, cur.rlim_max);
    }
}

#[cfg(test)]
mod allocation_tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, symlink};
    use uuid::Uuid;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("forge-allocation-{}", Uuid::new_v4()));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            fs::create_dir(root.join("repo")).unwrap();
            Self(root)
        }
        fn base(&self) -> PathBuf {
            self.0.join("workspaces")
        }
        fn repo(&self) -> PathBuf {
            self.0.join("repo")
        }
        fn create(&self, id: &str, commit: &str) -> Result<IsolatedWorktree, WorktreeError> {
            IsolatedWorktree::create_in(id, &self.repo(), commit, &self.base())
        }
        fn git(&self, args: &[&str]) -> String {
            let output = Command::new("git")
                .args(args)
                .current_dir(self.repo())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().into()
        }
        fn init_repo(&self) {
            self.git(&["init", "-b", "main"]);
            self.git(&[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ]);
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn invalid_ids_do_not_touch_filesystem() {
        let f = Fixture::new();
        for id in [
            "",
            ".",
            "..",
            "../repo",
            "/tmp/escape",
            "a/b",
            "a\\b",
            "a.b",
            "é",
            &"a".repeat(129),
        ] {
            assert!(
                matches!(f.create(id, "HEAD"), Err(WorktreeError::InvalidJobId)),
                "{id}"
            );
            assert!(!f.base().exists());
        }
    }

    #[test]
    fn roots_reject_permissions_symlinks_and_overlap() {
        let f = Fixture::new();
        fs::DirBuilder::new().mode(0o755).create(f.base()).unwrap();
        assert!(matches!(
            f.create("job", "HEAD"),
            Err(WorktreeError::UnsafeWorkspaceRoot(..))
        ));
        assert_eq!(fs::metadata(f.base()).unwrap().mode() & 0o777, 0o755);
        fs::remove_dir(f.base()).unwrap();
        symlink(f.repo(), f.base()).unwrap();
        assert!(matches!(
            f.create("job", "HEAD"),
            Err(WorktreeError::UnsafeWorkspaceRoot(..))
        ));
        for base in [f.repo(), f.repo().join("nested"), f.0.clone()] {
            assert!(matches!(
                IsolatedWorktree::create_in("job", &f.repo(), "HEAD", &base),
                Err(WorktreeError::ProductionPathCollision(_))
            ));
        }
        let alias = f.0.join("alias");
        symlink(&f.0, &alias).unwrap();
        assert!(matches!(
            IsolatedWorktree::create_in("job", &f.repo(), "HEAD", &alias.join("nested")),
            Err(WorktreeError::UnsafeWorkspaceRoot(..))
        ));
    }

    #[test]
    fn existing_directories_and_symlinks_are_untouched() {
        let f = Fixture::new();
        fs::DirBuilder::new().mode(0o700).create(f.base()).unwrap();
        fs::create_dir(f.base().join("job")).unwrap();
        fs::write(f.base().join("job/sentinel"), "keep").unwrap();
        assert!(matches!(
            f.create("job", "HEAD"),
            Err(WorktreeError::WorkspaceExists(_))
        ));
        symlink(f.repo(), f.base().join("link")).unwrap();
        assert!(matches!(
            f.create("link", "HEAD"),
            Err(WorktreeError::WorkspaceExists(_))
        ));
        assert_eq!(
            fs::read_to_string(f.base().join("job/sentinel")).unwrap(),
            "keep"
        );
        assert!(
            fs::symlink_metadata(f.base().join("link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn concurrent_duplicate_allocation_has_one_owner() {
        let f = Fixture::new();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let base = f.base();
                let repo = f.repo();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    WorkspaceAllocation::new(&base, "job", &repo)
                })
            })
            .collect();
        let mut winners = Vec::new();
        let mut losers = 0;
        for handle in handles {
            match handle.join().unwrap() {
                Ok(allocation) => winners.push(allocation),
                Err(WorktreeError::WorkspaceExists(_)) => losers += 1,
                other => panic!("unexpected allocation result: {other:?}"),
            }
        }
        assert_eq!(winners.len(), 1);
        assert_eq!(losers, 1);
        assert_eq!(
            winners[0].directory.metadata().unwrap().mode() & 0o777,
            0o700
        );
        winners[0].cleanup().unwrap();
    }

    #[test]
    fn creation_failure_cleans_only_fresh_allocation() {
        let f = Fixture::new();
        f.init_repo();
        assert!(matches!(
            f.create("job", "missing-commit"),
            Err(WorktreeError::InvalidBaseCommit(_))
        ));
        assert!(!f.base().join("job").exists());
        // A non-repository also fails after allocation and must not leak it.
        fs::remove_dir_all(f.repo().join(".git")).unwrap();
        assert!(f.create("job", "HEAD").is_err());
        assert!(!f.base().join("job").exists());
    }

    #[test]
    fn clone_is_pinned_independent_and_retained_on_request() {
        let f = Fixture::new();
        f.init_repo();
        let sha = f.git(&["rev-parse", "HEAD"]);
        // Boundary-length IDs are valid and allocation remains private after Git setup.
        let boundary = f.create(&"a".repeat(128), &sha).unwrap();
        assert_eq!(
            fs::metadata(&boundary.root_dir).unwrap().mode() & 0o777,
            0o700
        );
        drop(boundary);
        let mut worktree = f.create("Job_123-test", &sha).unwrap();
        assert_eq!(worktree.base_commit, sha);
        assert!(worktree.root_dir.join(".git").is_dir());
        assert_ne!(
            fs::metadata(f.repo().join(".git/objects")).unwrap().ino(),
            fs::metadata(worktree.root_dir.join(".git/objects"))
                .unwrap()
                .ino()
        );
        worktree
            .write_file(Path::new("nested/file"), "data")
            .unwrap();
        let root = worktree.root_dir.clone();
        worktree.retain_for_inspection();
        drop(worktree);
        assert_eq!(
            fs::read_to_string(root.join("nested/file")).unwrap(),
            "data"
        );
        let worktree = f.create("cleanup", &sha).unwrap();
        let root = worktree.root_dir.clone();
        drop(worktree);
        assert!(!root.exists());
    }

    #[test]
    fn substituted_directory_or_symlink_is_never_cleaned() {
        for replace_with_link in [false, true] {
            let f = Fixture::new();
            f.init_repo();
            let worktree = f.create("job", "HEAD").unwrap();
            let original = f.base().join("original");
            fs::rename(&worktree.root_dir, &original).unwrap();
            fs::write(f.repo().join("sentinel"), "keep").unwrap();
            if replace_with_link {
                symlink(f.repo(), &worktree.root_dir).unwrap();
            } else {
                fs::create_dir(&worktree.root_dir).unwrap();
                fs::write(worktree.root_dir.join("sentinel"), "keep").unwrap();
            }
            assert!(worktree.allocation.cleanup().is_err());
            let replacement = worktree.root_dir.clone();
            drop(worktree);
            assert_eq!(
                fs::read_to_string(replacement.join("sentinel")).unwrap(),
                "keep"
            );
            assert!(original.join(".git").exists());
        }
    }

    #[test]
    fn cleanup_uses_pinned_base_and_ignores_mutable_public_path() {
        let f = Fixture::new();
        f.init_repo();
        let mut worktree = f.create("job", "HEAD").unwrap();
        symlink(f.repo(), worktree.root_dir.join("external")).unwrap();
        fs::write(f.repo().join("sentinel"), "keep").unwrap();
        fs::rename(f.base(), f.0.join("moved")).unwrap();
        fs::create_dir(f.base()).unwrap();
        fs::create_dir(f.base().join("job")).unwrap();
        fs::write(f.base().join("job/sentinel"), "replacement").unwrap();
        worktree.root_dir = f.repo();
        drop(worktree);
        assert!(!f.0.join("moved/job").exists());
        assert_eq!(
            fs::read_to_string(f.base().join("job/sentinel")).unwrap(),
            "replacement"
        );
        assert_eq!(
            fs::read_to_string(f.repo().join("sentinel")).unwrap(),
            "keep"
        );
    }
}
