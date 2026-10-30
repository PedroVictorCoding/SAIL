use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Component, Path, PathBuf};
use tracing::warn;

#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    allowed_roots: Vec<PathBuf>,
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
}

impl SandboxPolicy {
    pub fn new() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/home/main/SAIL"));
        let mut allowed = vec![
            current_dir,
            PathBuf::from("/home/main/SAIL"),
            PathBuf::from("/home/main/.sail"),
            PathBuf::from("/tmp/sail"),
        ];

        // Deduplicate and canonicalize roots where possible
        let mut canonical_roots = Vec::new();
        for r in allowed.drain(..) {
            if let Ok(c) = fs::canonicalize(&r) {
                if !canonical_roots.contains(&c) {
                    canonical_roots.push(c);
                }
            } else if !canonical_roots.contains(&r) {
                canonical_roots.push(r);
            }
        }

        Self {
            allowed_roots: canonical_roots,
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
            self.allowed_roots.push(canonical);
        }
        self
    }

    /// Validate that a path is securely constrained within the sandbox.
    /// Canonicalizes the path, resolves symlinks, denies path traversal escapes,
    /// verifies real targets stay beneath an allowed root, and rejects special device nodes.
    pub fn validate_path(&self, target: &Path, is_write: bool) -> Result<PathBuf, SandboxViolation> {
        // 1. Check for null bytes or illegal empty path
        if target.as_os_str().is_empty() {
            return Err(SandboxViolation::PathEscapesRoot {
                path: target.to_path_buf(),
                target: PathBuf::new(),
            });
        }

        // 2. Telemetry exception for /proc (strictly read-only and constrained to allowed proc paths)
        let target_str = target.to_string_lossy();
        if target_str.starts_with("/proc") {
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

        // Deny all other system mounts (/sys, /dev, /etc, /root, /boot)
        if target_str.starts_with("/sys")
            || target_str.starts_with("/dev")
            || target_str.starts_with("/etc")
            || target_str.starts_with("/root")
            || target_str.starts_with("/boot")
        {
            return Err(SandboxViolation::SensitivePathBlocked {
                path: target.to_path_buf(),
            });
        }

        // 3. Resolve path canonicalization
        let canonical_target = if target.exists() {
            let canonical = fs::canonicalize(target).map_err(|_| SandboxViolation::PathDoesNotExist {
                path: target.to_path_buf(),
            })?;

            // Check file type: reject special devices (block, char, fifo, socket)
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
                    if file_name_str == ".." || file_name_str == "." || file_name_str.contains('/') || file_name_str.contains('\\') {
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
        for root in &self.allowed_roots {
            if self.is_subpath(&canonical_target, root) {
                is_beneath_root = true;
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

        Ok(canonical_target)
    }

    /// Legacy convenience boolean check
    pub fn is_path_allowed(&self, target: &Path, is_write: bool) -> bool {
        self.validate_path(target, is_write).is_ok()
    }

    /// Validate CLI execution command: enforces executable allowlist, rejects shell metacharacters,
    /// and parses into typed executable binary + argument vector.
    pub fn validate_command(&self, raw_cmd: &str) -> Result<(String, Vec<String>), SandboxViolation> {
        let trimmed = raw_cmd.trim();
        if trimmed.is_empty() {
            return Err(SandboxViolation::CommandNotAllowed {
                binary: String::new(),
            });
        }

        // 1. Reject shell injection metacharacters
        let forbidden_metachars = [';', '&', '|', '>', '<', '$', '`', '\n', '\r', '\0'];
        for c in trimmed.chars() {
            if forbidden_metachars.contains(&c) {
                return Err(SandboxViolation::ShellMetacharacterDetected { character: c });
            }
        }

        // 2. Tokenize into binary and arguments (handling standard quotes safely)
        let tokens = tokenize_command(trimmed);
        if tokens.is_empty() {
            return Err(SandboxViolation::CommandNotAllowed {
                binary: String::new(),
            });
        }

        let binary_path = &tokens[0];
        let binary_name = Path::new(binary_path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(binary_path);

        // 3. Reject interpreter chain execution
        let interpreters = ["bash", "sh", "zsh", "python", "python3", "perl", "ruby", "node"];
        if interpreters.contains(&binary_name) {
            // Check if attempting inline script execution (-c, -e)
            for arg in &tokens[1..] {
                if arg == "-c" || arg == "-e" || arg.starts_with("-c") {
                    return Err(SandboxViolation::InterpreterExecutionDenied {
                        interpreter: binary_name.to_string(),
                    });
                }
            }
        }

        // 4. Executable allowlist check
        if !self.allowed_executables.contains(&binary_name.to_string()) {
            return Err(SandboxViolation::CommandNotAllowed {
                binary: binary_name.to_string(),
            });
        }

        let args = tokens[1..].to_vec();
        Ok((binary_name.to_string(), args))
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
        if comps.len() == 4 && comps[0] == Component::RootDir && comps[1] == Component::Normal("proc".as_ref()) {
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

/// Simple whitespace and quote-aware command tokenizer without shell expansion
fn tokenize_command(cmd: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    for c in cmd.chars() {
        match c {
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
            }
            c if c.is_whitespace() && !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => {
                current.push(c);
            }
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}
