use std::path::{Path, PathBuf};

pub struct SandboxPolicy {
    allowed_roots: Vec<PathBuf>,
    blocked_patterns: Vec<String>,
}

impl SandboxPolicy {
    pub fn new() -> Self {
        Self {
            allowed_roots: vec![
                PathBuf::from("/home/main/SAIL"),
                PathBuf::from("/home/main/.sail"),
                PathBuf::from("/tmp/sail"),
            ],
            blocked_patterns: vec![
                "/etc/shadow".into(),
                "/etc/passwd".into(),
                "/root".into(),
                ".ssh/id_".into(),
                ".gnupg".into(),
                "/dev/mem".into(),
            ],
        }
    }

    /// Check if a filesystem path is permitted under the sandbox policy
    pub fn is_path_allowed(&self, target: &Path, is_write: bool) -> bool {
        let path_str = target.to_string_lossy();

        // 1. Check blocked sensitive patterns
        for pattern in &self.blocked_patterns {
            if path_str.contains(pattern) {
                return false;
            }
        }

        // 2. Read-only access to /proc and /sys for safe telemetry is permitted
        if !is_write && (path_str.starts_with("/proc") || path_str.starts_with("/sys")) {
            return true;
        }

        // 3. For write access, path must reside within an allowed root
        for root in &self.allowed_roots {
            if target.starts_with(root) {
                return true;
            }
        }

        // If not explicitly inside an allowed root, deny
        !is_write && path_str.starts_with("/home/main/SAIL")
    }

    /// Check if a CLI command is safe or contains prohibited privilege escalation attempts
    pub fn is_command_safe(&self, cmd: &str) -> bool {
        let lower = cmd.to_lowercase();
        let dangerous = [
            "sudo", "su -", "chmod 777", "rm -rf /", "mkfs", "dd if=",
            ":(){ :|:& };:", "chown root", "iptables -F", "ufw disable"
        ];

        for d in &dangerous {
            if lower.contains(d) {
                return false;
            }
        }

        true
    }
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self::new()
    }
}

