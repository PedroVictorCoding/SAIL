use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::path::{Component, Path, PathBuf};
use tracing::warn;

#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    allowed_roots: Vec<PathBuf>,
    confined_roots: Vec<std::sync::Arc<crate::confined::ConfinedRoot>>,
    read_only_roots: Vec<PathBuf>,
    allowed_proc_paths: Vec<PathBuf>,
    allowed_executables: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SandboxViolation {
    #[error("Path {path:?} escapes allowed roots, resolving to {target:?}")]
    PathEscapesRoot { path: PathBuf, target: PathBuf },
    #[error("Path {path:?} does not exist")]
    PathDoesNotExist { path: PathBuf },
    #[error("Path {path:?} is a special device file (block/char/fifo/socket) and cannot be opened")]
    SpecialDeviceBlocked { path: PathBuf },
    #[error("Path {path:?} is a protected system directory and is blocked")]
    SensitivePathBlocked { path: PathBuf },
    #[error("Attempted write to read-only root {path:?}")]
    WriteToReadOnlyRoot { path: PathBuf },
    #[error("Invalid filename or traversal segment in path {path:?}")]
    InvalidFilename { path: PathBuf },
    #[error("Binary {binary} is not in the authorized executable allowlist")]
    CommandNotAllowed { binary: String },
    #[error("Shell metacharacter '{character}' detected in CLI invocation")]
    ShellMetacharacterDetected { character: char },
    #[error("Interpreter script execution denied for {interpreter}")]
    InterpreterExecutionDenied { interpreter: String },
    #[error("Command argument '{arg}' is prohibited: {reason}")]
    CommandArgumentDenied { arg: String, reason: String },
    #[error("Working directory {dir:?} violates sandbox boundaries")]
    WorkingDirectoryViolation { dir: PathBuf },
    #[error("Execution error: {reason}")]
    ExecutionFailed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxedCommand {
    pub binary: String,
    pub binary_path: PathBuf,
    pub args: Vec<String>,
    pub current_dir: PathBuf,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

#[cfg(all(unix, target_os = "linux"))]
mod linux_openat2 {
    use std::os::unix::io::RawFd;

    #[repr(C)]
    #[derive(Debug, Copy, Clone)]
    pub struct OpenHow {
        pub flags: u64,
        pub mode: u64,
        pub resolve: u64,
    }

    #[allow(dead_code)]
    pub const RESOLVE_NO_XDEV: u64 = 0x01;
    pub const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    #[allow(dead_code)]
    pub const RESOLVE_NO_SYMLINKS: u64 = 0x04;
    pub const RESOLVE_BENEATH: u64 = 0x08;
    #[allow(dead_code)]
    pub const RESOLVE_IN_ROOT: u64 = 0x10;
    #[allow(dead_code)]
    pub const RESOLVE_CACHED: u64 = 0x20;

    pub unsafe fn sys_openat2(
        dfd: RawFd,
        filename: *const libc::c_char,
        how: &OpenHow,
    ) -> libc::c_int {
        unsafe {
            libc::syscall(
                libc::SYS_openat2,
                dfd,
                filename,
                how as *const OpenHow,
                std::mem::size_of::<OpenHow>(),
            ) as libc::c_int
        }
    }
}

impl SandboxPolicy {
    /// Create a new SandboxPolicy with roots dynamically derived from workspace configuration:
    /// 1. `SAIL_WORKSPACE_ROOT` environment variable
    /// 2. `SAIL_TOOL_ROOT` environment variable
    /// 3. Current process working directory
    /// 4. `$HOME/.sail`
    /// 5. Standard transient temp directory for sail (`/tmp/sail`)
    ///
    /// Avoids any hardcoded user paths for full OS portability.
    pub fn new() -> Self {
        let configured = std::env::var_os("SAIL_TOOL_ROOT")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok());
        let canonical_roots: Vec<PathBuf> = configured
            .into_iter()
            .filter_map(|p| fs::canonicalize(p).ok())
            .collect();
        Self {
            confined_roots: canonical_roots
                .iter()
                .filter_map(|p| crate::confined::ConfinedRoot::new(p).ok())
                .map(std::sync::Arc::new)
                .collect(),
            allowed_roots: canonical_roots,
            read_only_roots: Vec::new(),
            allowed_proc_paths: vec![
                PathBuf::from("/proc/uptime"),
                PathBuf::from("/proc/loadavg"),
                PathBuf::from("/proc/meminfo"),
                PathBuf::from("/proc/cpuinfo"),
                PathBuf::from("/proc/stat"),
            ],
            allowed_executables: vec![
                "git".into(),
                "cargo".into(),
                "rustc".into(),
                "ls".into(),
                "grep".into(),
                "find".into(),
                "cat".into(),
                "echo".into(),
                "diff".into(),
                "uname".into(),
                "whoami".into(),
                "lscpu".into(),
                "free".into(),
                "ps".into(),
                "uptime".into(),
                "date".into(),
            ],
        }
    }

    /// Construct a SandboxPolicy with explicit allowed roots.
    pub fn from_roots<I, P>(roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        let mut canonical_roots = Vec::new();
        for r in roots {
            let p = r.into();
            if let Ok(c) = fs::canonicalize(&p) {
                if !canonical_roots.contains(&c) {
                    canonical_roots.push(c);
                }
            } else if !canonical_roots.contains(&p) {
                canonical_roots.push(p);
            }
        }

        Self {
            confined_roots: canonical_roots
                .iter()
                .filter_map(|p| crate::confined::ConfinedRoot::new(p).ok())
                .map(std::sync::Arc::new)
                .collect(),
            allowed_roots: canonical_roots,
            read_only_roots: Vec::new(),
            allowed_proc_paths: vec![
                PathBuf::from("/proc/uptime"),
                PathBuf::from("/proc/loadavg"),
                PathBuf::from("/proc/meminfo"),
                PathBuf::from("/proc/cpuinfo"),
                PathBuf::from("/proc/stat"),
            ],
            allowed_executables: vec![
                "git".into(),
                "cargo".into(),
                "rustc".into(),
                "ls".into(),
                "grep".into(),
                "find".into(),
                "cat".into(),
                "echo".into(),
                "diff".into(),
                "uname".into(),
                "whoami".into(),
                "lscpu".into(),
                "free".into(),
                "ps".into(),
                "uptime".into(),
                "date".into(),
            ],
        }
    }

    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        let r = root.into();
        let canonical = fs::canonicalize(&r).unwrap_or(r);
        if !self.allowed_roots.contains(&canonical) {
            if let Ok(root) = crate::confined::ConfinedRoot::new(&canonical) {
                self.confined_roots.push(std::sync::Arc::new(root));
            }
            self.allowed_roots.push(canonical);
        }
        self
    }

    pub fn with_read_only_root(mut self, root: impl Into<PathBuf>) -> Self {
        let r = root.into();
        let canonical = fs::canonicalize(&r).unwrap_or(r);
        if !self.read_only_roots.contains(&canonical) {
            self.read_only_roots.push(canonical);
        }
        self
    }

    /// Return the actual descriptor used by the read operation; validation alone
    /// never grants permission to reopen a caller-controlled pathname.
    pub fn open_read(
        &self,
        target: &Path,
    ) -> Result<crate::confined::ConfinedFile, SandboxViolation> {
        for root in &self.confined_roots {
            if target.is_absolute() && !target.starts_with(root.path()) {
                continue;
            }
            return root
                .open(target)
                .map_err(|_| SandboxViolation::SensitivePathBlocked {
                    path: target.into(),
                });
        }
        Err(SandboxViolation::PathEscapesRoot {
            path: target.into(),
            target: target.into(),
        })
    }
    pub fn write_file(&self, path: &Path, content: &[u8]) -> Result<usize, SandboxViolation> {
        for root in &self.confined_roots {
            if path.is_absolute() && !path.starts_with(root.path()) {
                continue;
            }
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.path().join(path)
            };
            if self.read_only_roots.iter().any(|r| absolute.starts_with(r)) {
                return Err(SandboxViolation::WriteToReadOnlyRoot { path: absolute });
            }
            return root
                .atomic_write(path, content)
                .map_err(|_| SandboxViolation::SensitivePathBlocked { path: absolute });
        }
        Err(SandboxViolation::PathEscapesRoot {
            path: path.into(),
            target: path.into(),
        })
    }
    pub fn allowed_roots(&self) -> &[PathBuf] {
        &self.allowed_roots
    }

    /// Validate that a path is securely constrained within the sandbox.
    /// Canonicalizes the path, resolves symlinks, denies path traversal escapes,
    /// verifies real targets stay beneath an allowed root, rejects special device nodes,
    /// and uses openat2 handle-based verification when available.
    pub fn validate_path(
        &self,
        target: &Path,
        is_write: bool,
    ) -> Result<PathBuf, SandboxViolation> {
        // 1. Check for null bytes or illegal empty path
        if target.as_os_str().is_empty() {
            return Err(SandboxViolation::PathEscapesRoot {
                path: target.to_path_buf(),
                target: PathBuf::new(),
            });
        }

        #[cfg(unix)]
        if target.as_os_str().as_bytes().contains(&0) {
            return Err(SandboxViolation::InvalidFilename {
                path: target.to_path_buf(),
            });
        }

        // 2. Check for system mount points using normalized path components
        let comps: Vec<_> = target.components().collect();
        if comps.len() >= 2
            && comps[0] == Component::RootDir
            && let Component::Normal(first_part) = comps[1]
        {
            let first_str = first_part.to_string_lossy();

            // Telemetry exception for /proc (strictly read-only and constrained to allowed proc paths)
            if first_str == "proc" {
                if is_write {
                    return Err(SandboxViolation::WriteToReadOnlyRoot {
                        path: target.to_path_buf(),
                    });
                }
                if self.is_allowed_proc_path(target) {
                    return Ok(target.to_path_buf());
                } else {
                    return Err(SandboxViolation::SensitivePathBlocked {
                        path: target.to_path_buf(),
                    });
                }
            }

            // Deny all sensitive host mounts
            if matches!(
                first_str.as_ref(),
                "sys" | "dev" | "etc" | "root" | "boot" | "run" | "var"
            ) {
                return Err(SandboxViolation::SensitivePathBlocked {
                    path: target.to_path_buf(),
                });
            }
        }

        // 3. Resolve path canonicalization
        let canonical_target = if target.exists() {
            let canonical =
                fs::canonicalize(target).map_err(|_| SandboxViolation::PathDoesNotExist {
                    path: target.to_path_buf(),
                })?;

            // Check file type: reject special devices (block, char, fifo, socket)
            #[cfg(unix)]
            if let Ok(meta) = fs::symlink_metadata(&canonical) {
                let ft = meta.file_type();
                if ft.is_block_device() || ft.is_char_device() || ft.is_fifo() || ft.is_socket() {
                    return Err(SandboxViolation::SpecialDeviceBlocked {
                        path: target.to_path_buf(),
                    });
                }
            }

            canonical
        } else {
            // For files or directories that do not yet exist, find the closest existing ancestor directory
            let mut ancestors = Vec::new();
            let mut curr = target;
            while !curr.exists() {
                if let Some(file_name) = curr.file_name() {
                    let file_name_str = file_name.to_string_lossy();
                    if file_name_str == ".."
                        || file_name_str == "."
                        || file_name_str.contains('/')
                        || file_name_str.contains('\\')
                    {
                        return Err(SandboxViolation::InvalidFilename {
                            path: target.to_path_buf(),
                        });
                    }
                    ancestors.push(file_name);
                } else {
                    break;
                }
                match curr.parent() {
                    Some(p) => curr = p,
                    None => break,
                }
            }

            let canonical_base = if curr.exists() {
                fs::canonicalize(curr).map_err(|_| SandboxViolation::PathDoesNotExist {
                    path: curr.to_path_buf(),
                })?
            } else {
                return Err(SandboxViolation::PathDoesNotExist {
                    path: target.to_path_buf(),
                });
            };

            let mut resolved = canonical_base;
            for segment in ancestors.into_iter().rev() {
                resolved.push(segment);
            }
            resolved
        };

        // 4. Verify canonical target resides strictly beneath one of the allowed roots
        let mut is_beneath_root = false;
        let mut matched_root: Option<&PathBuf> = None;
        for root in &self.allowed_roots {
            if self.is_subpath(&canonical_target, root) {
                is_beneath_root = true;
                matched_root = Some(root);
                break;
            }
        }

        if !is_beneath_root {
            warn!(
                path = ?target,
                canonical = ?canonical_target,
                roots = ?self.allowed_roots,
                "Sandbox path escape detected"
            );
            return Err(SandboxViolation::PathEscapesRoot {
                path: target.to_path_buf(),
                target: canonical_target,
            });
        }

        // 5. Check read-only roots when write operation is requested
        if is_write {
            for ro_root in &self.read_only_roots {
                if self.is_subpath(&canonical_target, ro_root) {
                    return Err(SandboxViolation::WriteToReadOnlyRoot {
                        path: target.to_path_buf(),
                    });
                }
            }
        }

        // 6. Handle-based descriptor verification with openat2 (RESOLVE_BENEATH) on Linux
        #[cfg(all(unix, target_os = "linux"))]
        if let Some(root) = matched_root
            && let Ok(rel) = canonical_target.strip_prefix(root)
            && let Ok(root_c) = std::ffi::CString::new(root.as_os_str().as_bytes())
        {
            let dfd = unsafe {
                libc::open(
                    root_c.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            if dfd >= 0 {
                let rel_bytes = if rel.as_os_str().is_empty() {
                    b".".as_slice()
                } else {
                    rel.as_os_str().as_bytes()
                };

                if let Ok(rel_c) = std::ffi::CString::new(rel_bytes) {
                    let how = linux_openat2::OpenHow {
                        flags: (libc::O_PATH | libc::O_CLOEXEC) as u64,
                        mode: 0,
                        resolve: linux_openat2::RESOLVE_BENEATH
                            | linux_openat2::RESOLVE_NO_MAGICLINKS,
                    };

                    let res = unsafe { linux_openat2::sys_openat2(dfd, rel_c.as_ptr(), &how) };
                    unsafe { libc::close(dfd) };

                    if res >= 0 {
                        unsafe { libc::close(res) };
                    } else {
                        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                        if err == libc::EXDEV || err == libc::ELOOP {
                            return Err(SandboxViolation::PathEscapesRoot {
                                path: target.to_path_buf(),
                                target: canonical_target,
                            });
                        }
                    }
                } else {
                    unsafe { libc::close(dfd) };
                }
            }
        }

        Ok(canonical_target)
    }

    /// Convenience check returning boolean
    pub fn is_path_allowed(&self, target: &Path, is_write: bool) -> bool {
        self.validate_path(target, is_write).is_ok()
    }

    /// Safely opens a file beneath an allowed root, verifying descriptor mode
    /// and rejecting special devices or path escapes.
    pub fn open_sandboxed_file(
        &self,
        target: &Path,
        is_write: bool,
    ) -> Result<fs::File, SandboxViolation> {
        let canonical = self.validate_path(target, is_write)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            use std::os::unix::io::AsRawFd;

            let mut opts = fs::OpenOptions::new();
            if is_write {
                opts.create(true).truncate(false).write(true).read(true);
            } else {
                opts.read(true);
            }
            opts.custom_flags(libc::O_CLOEXEC);

            let file = opts
                .open(&canonical)
                .map_err(|_| SandboxViolation::PathDoesNotExist {
                    path: target.to_path_buf(),
                })?;

            // Descriptor check via fstat
            let fd = file.as_raw_fd();
            let mut stat_buf: libc::stat = unsafe { std::mem::zeroed() };
            let res = unsafe { libc::fstat(fd, &mut stat_buf) };
            if res == 0 {
                let mode = stat_buf.st_mode & libc::S_IFMT;
                if mode == libc::S_IFBLK
                    || mode == libc::S_IFCHR
                    || mode == libc::S_IFIFO
                    || mode == libc::S_IFSOCK
                {
                    return Err(SandboxViolation::SpecialDeviceBlocked {
                        path: target.to_path_buf(),
                    });
                }
            }

            Ok(file)
        }

        #[cfg(not(unix))]
        {
            let mut opts = fs::OpenOptions::new();
            if is_write {
                opts.create(true).write(true).read(true);
            } else {
                opts.read(true);
            }
            opts.open(&canonical)
                .map_err(|_| SandboxViolation::PathDoesNotExist {
                    path: target.to_path_buf(),
                })
        }
    }

    /// Prepares and strictly validates a CLI command execution:
    /// - Positively verifies binary name against `allowed_executables`
    /// - Rejects shell injection metacharacters
    /// - Rejects sub-process spawning flags (`find -exec`, `git config`, interpreter `-c`)
    /// - Constrains working directory to allowed roots
    /// - Clears and constructs a sanitized, minimal environment (preventing secret leakage)
    pub fn prepare_command(
        &self,
        binary_input: &str,
        args: &[String],
        workdir: Option<&Path>,
    ) -> Result<SandboxedCommand, SandboxViolation> {
        let binary_trimmed = binary_input.trim();
        if binary_trimmed.is_empty() {
            return Err(SandboxViolation::CommandNotAllowed {
                binary: String::new(),
            });
        }

        // 1. Resolve binary name
        let binary_path_buf = PathBuf::from(binary_trimmed);
        let binary_name = binary_path_buf
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(binary_trimmed)
            .to_string();

        // Reject arbitrary interpreter execution
        let interpreters = [
            "bash", "sh", "zsh", "python", "python3", "perl", "ruby", "node", "php",
        ];
        if interpreters.contains(&binary_name.as_str()) {
            return Err(SandboxViolation::InterpreterExecutionDenied {
                interpreter: binary_name,
            });
        }

        // Executable allowlist check (positive allowlist)
        if !self.allowed_executables.contains(&binary_name) {
            return Err(SandboxViolation::CommandNotAllowed {
                binary: binary_name,
            });
        }

        // 2. Reject shell injection metacharacters
        let forbidden_metachars = [
            ';', '&', '|', '>', '<', '$', '`', '\n', '\r', '\0', '(', ')',
        ];
        for arg in args {
            for c in arg.chars() {
                if forbidden_metachars.contains(&c) {
                    return Err(SandboxViolation::ShellMetacharacterDetected { character: c });
                }
            }
        }

        // 3. Command-specific sub-process and interpreter spawn hardening
        if binary_name == "find" {
            for arg in args {
                if arg == "-exec" || arg == "-execdir" || arg == "-ok" || arg == "-okdir" {
                    return Err(SandboxViolation::CommandArgumentDenied {
                        arg: arg.clone(),
                        reason: "Subprocess execution via find option is prohibited".into(),
                    });
                }
            }
        } else if binary_name == "git" {
            for arg in args {
                if arg.starts_with("--exec-path")
                    || arg.starts_with("--upload-pack")
                    || arg == "-c"
                    || arg.starts_with("-c")
                    || arg == "config"
                    || arg == "alias"
                {
                    return Err(SandboxViolation::CommandArgumentDenied {
                        arg: arg.clone(),
                        reason:
                            "Arbitrary execution or config alteration via git option is prohibited"
                                .into(),
                    });
                }
            }
        }

        for arg in args {
            if arg == "-c" || arg == "-e" || arg == "--eval" {
                return Err(SandboxViolation::InterpreterExecutionDenied {
                    interpreter: binary_name.clone(),
                });
            }
        }

        // 4. Working directory constraint
        let current_dir = match workdir {
            Some(dir) => self.validate_path(dir, false).map_err(|_| {
                SandboxViolation::WorkingDirectoryViolation {
                    dir: dir.to_path_buf(),
                }
            })?,
            None => {
                if let Some(first_root) = self.allowed_roots.first() {
                    first_root.clone()
                } else {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                }
            }
        };

        // 5. Binary resolution from safe system directories
        let resolved_binary_path =
            resolve_safe_binary(&binary_name).unwrap_or_else(|| PathBuf::from(&binary_name));

        // 6. Sanitized minimal environment (strip secrets and arbitrary library preloads)
        let mut env = BTreeMap::new();
        let safe_path = if let Ok(home) = std::env::var("HOME") {
            let cargo_bin = format!("{}/.cargo/bin", home);
            format!("{}:/usr/local/bin:/usr/bin:/bin", cargo_bin)
        } else {
            "/usr/local/bin:/usr/bin:/bin".to_string()
        };
        env.insert("PATH".to_string(), safe_path);
        env.insert("TERM".to_string(), "dumb".to_string());
        env.insert("LANG".to_string(), "C.UTF-8".to_string());
        env.insert("LC_ALL".to_string(), "C.UTF-8".to_string());
        env.insert("PWD".to_string(), current_dir.display().to_string());
        env.insert(
            "TMPDIR".to_string(),
            std::env::temp_dir().join("sail").display().to_string(),
        );

        Ok(SandboxedCommand {
            binary: binary_name,
            binary_path: resolved_binary_path,
            args: args.to_vec(),
            current_dir,
            env,
        })
    }

    /// Execute a verified SandboxedCommand directly with argv (never invoking a shell),
    /// isolated working directory, new process group, and sanitized environment.
    pub fn execute_command(
        &self,
        cmd: &SandboxedCommand,
    ) -> Result<CommandOutput, SandboxViolation> {
        let mut process = std::process::Command::new(&cmd.binary_path);
        process.args(&cmd.args);
        process.current_dir(&cmd.current_dir);
        process.env_clear();
        for (k, v) in &cmd.env {
            process.env(k, v);
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            process.process_group(0);
        }

        let output = process
            .output()
            .map_err(|e| SandboxViolation::ExecutionFailed {
                reason: format!("Failed to spawn {}: {}", cmd.binary, e),
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let status = output.status.code().unwrap_or(-1);

        Ok(CommandOutput {
            status,
            stdout,
            stderr,
        })
    }

    /// Legacy string-based command validation: tokenizes command and runs prepare_command.
    /// A shell string is never an execution permission. Forge uses the separate
    /// typed namespace-bound prepare_command API; user commands use sail-runner.
    pub fn validate_command(
        &self,
        raw_cmd: &str,
    ) -> Result<(String, Vec<String>), SandboxViolation> {
        Err(SandboxViolation::CommandNotAllowed {
            binary: raw_cmd.into(),
        })
    }
    pub fn is_command_safe(&self, cmd: &str) -> bool {
        self.validate_command(cmd).is_ok()
    }

    /// Check if target is strictly a subpath beneath root (preventing sibling prefix matching)
    fn is_subpath(&self, target: &Path, root: &Path) -> bool {
        let mut target_comps = target.components();
        let root_comps = root.components();

        for root_c in root_comps {
            match target_comps.next() {
                Some(target_c) if target_c == root_c => {}
                _ => return false,
            }
        }

        true
    }

    /// Check if path is in the strictly allowed proc telemetry list or safe process inspection
    fn is_allowed_proc_path(&self, path: &Path) -> bool {
        // Direct match against allowed telemetry files
        if self.allowed_proc_paths.contains(&path.to_path_buf()) {
            return true;
        }

        // Safe process stats: /proc/<pid>/stat, /proc/<pid>/cmdline, /proc/<pid>/status
        let comps: Vec<_> = path.components().collect();
        if comps.len() == 4
            && comps[0] == Component::RootDir
            && comps[1] == Component::Normal("proc".as_ref())
        {
            let pid_part = comps[2].as_os_str().to_string_lossy();
            let file_part = comps[3].as_os_str().to_string_lossy();

            let is_pid = pid_part.chars().all(|c| c.is_ascii_digit());
            let is_safe_stat = matches!(file_part.as_ref(), "stat" | "cmdline" | "status");

            if is_pid && is_safe_stat {
                return true;
            }
        }

        false
    }
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// Dynamically locate the workspace root from environment, manifest, or filesystem ancestry
/// Resolves binary name to canonical system binary path to prevent PATH hijacking
fn resolve_safe_binary(binary_name: &str) -> Option<PathBuf> {
    let search_dirs = [
        Path::new("/usr/bin"),
        Path::new("/bin"),
        Path::new("/usr/local/bin"),
    ];
    for dir in &search_dirs {
        let candidate = dir.join(binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let cargo_bin = PathBuf::from(home)
            .join(".cargo")
            .join("bin")
            .join(binary_name);
        if cargo_bin.is_file() {
            return Some(cargo_bin);
        }
    }
    None
}
