//! Typed, versioned execution plan shared by the Guardian authority, the runner and Forge.
//!
//! The plan is data, not authorization. It becomes actionable only after Guardian validates the
//! grant and the trusted supervisor hands it to the single authoritative runner. A plan carries
//! sandbox paths and policy references only: no host paths, no secrets, no environment values.
use crate::network::{NetworkLimits, NetworkPolicyRef};
use sail_common::types::SubsystemId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PLAN_VERSION: u32 = 1;
pub const MAX_PLAN_LIFETIME_MS: i64 = 24 * 60 * 60 * 1_000;
pub const MAX_CLOCK_SKEW_MS: i64 = 5 * 60 * 1_000;
pub const MAX_ARGV: usize = 128;
pub const MAX_ARGV_BYTES: usize = 32 * 1024;
pub const MAX_MOUNTS: usize = 32;
pub const MAX_CREDENTIALS: usize = 8;
pub const MAX_SANDBOX_PATH: usize = 4_096;
pub const MAX_PURPOSE: usize = 128;
pub const MAX_EXECUTABLE_BYTES: usize = 4_096;

/// Roots a task may write. Empty plan-authored write roots are not a grant on their own; the
/// runner still applies its own rootfs and cgroup policy.
pub const WRITABLE_PREFIXES: [&str; 3] = ["/work", "/cache", "/tmp"];

/// Read roots that would leak the owner's credentials or private data if mounted by mistake.
pub const FORBIDDEN_READ_PREFIXES: [&str; 2] = ["/home", "/root"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPlan {
    pub version: u32,
    pub job_id: Uuid,
    pub request_id: Uuid,
    pub principal: SubsystemId,
    pub capability_grant_id: Uuid,
    pub executable: ExecutableIdentity,
    pub argv: Vec<String>,
    pub cwd: String,
    pub read_roots: Vec<MountRoot>,
    pub write_roots: Vec<MountRoot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkPolicyRef>,
    #[serde(default)]
    pub credentials: Vec<CredentialRef>,
    pub limits: ExecutionLimits,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub audit_correlation_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutableIdentity {
    /// Absolute path inside the runner's trusted rootfs image.
    pub sandbox_path: String,
    /// Lowercase SHA-256 of the image file the trusted supervisor intends to execute.
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MountRoot {
    /// Absolute sandbox path resolved inside the trusted rootfs. Never a host path.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CredentialRef {
    /// Opaque Data Vault handle; the process never receives the secret itself.
    pub handle_id: Uuid,
    /// Short purpose label for the audit record, never a secret or token.
    pub purpose: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionLimits {
    pub wall_ms: u64,
    pub cpu_seconds: u64,
    pub memory_bytes: u64,
    pub pids: u32,
    pub fds: u32,
    pub scratch_bytes: u64,
    pub output_bytes: usize,
    pub network: NetworkLimits,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            wall_ms: 30_000,
            cpu_seconds: 10,
            memory_bytes: 512 * 1024 * 1024,
            pids: 32,
            fds: 64,
            scratch_bytes: 32 * 1024 * 1024,
            output_bytes: 64 * 1024,
            network: NetworkLimits::default(),
        }
    }
}

impl From<sail_protocol::execution::CommandLimits> for ExecutionLimits {
    fn from(limits: sail_protocol::execution::CommandLimits) -> Self {
        Self {
            wall_ms: limits.wall_ms,
            cpu_seconds: limits.cpu_seconds,
            memory_bytes: limits.memory_bytes,
            pids: limits.pids,
            fds: limits.fds,
            scratch_bytes: limits.scratch_bytes,
            output_bytes: limits.output_bytes,
            network: NetworkLimits::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("unsupported execution plan version {0}")]
    Version(u32),
    #[error("execution plan field {0} is missing or invalid")]
    Invalid(&'static str),
    #[error("execution plan has expired")]
    Expired,
    #[error("execution plan was issued in the future beyond the allowed clock skew")]
    NotYetValid,
    #[error("execution plan lifetime exceeds the maximum allowed")]
    LifetimeTooLong,
    #[error("sandbox path {0:?} is not a safe absolute sandbox path")]
    UnsafePath(String),
    #[error("write root {0:?} is outside the disposable writable roots")]
    UnwritableRoot(String),
    #[error("read root {0:?} would expose credentials or private data")]
    ForbiddenReadRoot(String),
    #[error("working directory {0:?} is outside every declared root")]
    CwdOutsideRoots(String),
    #[error("executable digest must be a lowercase SHA-256 hex string")]
    BadDigest,
    #[error("credential purpose {0:?} is not a plain label")]
    BadCredentialPurpose(String),
    #[error("network policy reference is invalid")]
    BadNetworkPolicy,
    #[error("execution limits are outside the service ceilings")]
    BadLimits,
}

impl ExecutionPlan {
    /// Fail-closed validation. `now_ms` comes from the trusted supervisor, never from the plan.
    pub fn validate(&self, now_ms: i64) -> Result<(), PlanError> {
        if self.version != PLAN_VERSION {
            return Err(PlanError::Version(self.version));
        }
        if [
            self.job_id,
            self.request_id,
            self.capability_grant_id,
            self.audit_correlation_id,
        ]
        .iter()
        .any(|id| id.is_nil())
        {
            return Err(PlanError::Invalid("identifiers"));
        }
        if self.issued_at_ms > now_ms + MAX_CLOCK_SKEW_MS {
            return Err(PlanError::NotYetValid);
        }
        if self.expires_at_ms <= self.issued_at_ms {
            return Err(PlanError::Invalid("expiry"));
        }
        if self.expires_at_ms - self.issued_at_ms > MAX_PLAN_LIFETIME_MS {
            return Err(PlanError::LifetimeTooLong);
        }
        if self.expires_at_ms <= now_ms {
            return Err(PlanError::Expired);
        }
        validate_executable(&self.executable)?;
        validate_argv(&self.argv)?;
        validate_path(&self.cwd)?;
        validate_mounts(&self.read_roots, &self.write_roots)?;
        validate_network(self.network.as_ref())?;
        validate_credentials(&self.credentials)?;
        validate_limits(&self.limits)?;
        let in_root = self
            .read_roots
            .iter()
            .chain(self.write_roots.iter())
            .any(|root| path_within(&self.cwd, &root.path));
        if !in_root {
            return Err(PlanError::CwdOutsideRoots(self.cwd.clone()));
        }
        Ok(())
    }

    /// Compatibility constructor for the existing v1 command protocol. The caller must supply
    /// the grant and the digest computed from the trusted image; this constructor does not
    /// authorize anything.
    pub fn from_command_plan(
        command: sail_protocol::execution::CommandPlan,
        principal: SubsystemId,
        capability_grant_id: Uuid,
        executable_sha256: impl Into<String>,
        issued_at_ms: i64,
        ttl_ms: i64,
    ) -> Self {
        Self {
            version: PLAN_VERSION,
            job_id: command.request_id,
            request_id: command.request_id,
            principal,
            capability_grant_id,
            executable: ExecutableIdentity {
                sandbox_path: command.executable,
                sha256: executable_sha256.into(),
            },
            argv: command.argv,
            cwd: "/work".into(),
            read_roots: [
                "/usr", "/lib", "/lib64", "/bin", "/etc/ssl", "/etc/pki", "/cache",
            ]
            .into_iter()
            .map(|path| MountRoot { path: path.into() })
            .collect(),
            write_roots: ["/work", "/tmp"]
                .into_iter()
                .map(|path| MountRoot { path: path.into() })
                .collect(),
            network: None,
            credentials: Vec::new(),
            limits: command.limits.into(),
            issued_at_ms,
            expires_at_ms: issued_at_ms + ttl_ms,
            audit_correlation_id: command.correlation_id,
        }
    }
}

fn validate_executable(executable: &ExecutableIdentity) -> Result<(), PlanError> {
    validate_path(&executable.sandbox_path)?;
    if executable.sha256.len() != 64
        || !executable
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(PlanError::BadDigest);
    }
    Ok(())
}

fn validate_argv(argv: &[String]) -> Result<(), PlanError> {
    if argv.len() > MAX_ARGV
        || argv.iter().map(String::len).sum::<usize>() > MAX_ARGV_BYTES
        || argv
            .iter()
            .any(|arg| arg.contains('\0') || arg.len() > MAX_SANDBOX_PATH)
    {
        return Err(PlanError::Invalid("argv"));
    }
    Ok(())
}

fn validate_mounts(read: &[MountRoot], write: &[MountRoot]) -> Result<(), PlanError> {
    if read.len() > MAX_MOUNTS || write.len() > MAX_MOUNTS {
        return Err(PlanError::Invalid("mount count"));
    }
    for root in read {
        validate_path(&root.path)?;
        if FORBIDDEN_READ_PREFIXES
            .iter()
            .any(|prefix| path_within(&root.path, prefix))
        {
            return Err(PlanError::ForbiddenReadRoot(root.path.clone()));
        }
    }
    for root in write {
        validate_path(&root.path)?;
        if !WRITABLE_PREFIXES
            .iter()
            .any(|prefix| path_within(&root.path, prefix))
        {
            return Err(PlanError::UnwritableRoot(root.path.clone()));
        }
    }
    Ok(())
}

fn validate_network(network: Option<&NetworkPolicyRef>) -> Result<(), PlanError> {
    if let Some(network) = network {
        crate::network::validate_policy_id(&network.policy_id)
            .map_err(|_| PlanError::BadNetworkPolicy)?;
        if network.policy_version == 0 {
            return Err(PlanError::BadNetworkPolicy);
        }
    }
    Ok(())
}

fn validate_credentials(credentials: &[CredentialRef]) -> Result<(), PlanError> {
    if credentials.len() > MAX_CREDENTIALS {
        return Err(PlanError::Invalid("credential count"));
    }
    for credential in credentials {
        if credential.handle_id.is_nil()
            || credential.purpose.is_empty()
            || credential.purpose.len() > MAX_PURPOSE
            || !credential.purpose.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b' ' | b'-' | b'_' | b'.' | b':' | b'/')
            })
        {
            return Err(PlanError::BadCredentialPurpose(credential.purpose.clone()));
        }
    }
    Ok(())
}

fn validate_limits(limits: &ExecutionLimits) -> Result<(), PlanError> {
    if limits.wall_ms == 0
        || limits.wall_ms > 120_000
        || limits.cpu_seconds == 0
        || limits.cpu_seconds > 60
        || !(16 * 1024 * 1024..=1024 * 1024 * 1024).contains(&limits.memory_bytes)
        || !(2..=64).contains(&limits.pids)
        || !(8..=128).contains(&limits.fds)
        || limits.scratch_bytes == 0
        || limits.scratch_bytes > 64 * 1024 * 1024
        || !(2..=256 * 1024).contains(&limits.output_bytes)
    {
        return Err(PlanError::BadLimits);
    }
    limits.network.validate().map_err(|_| PlanError::BadLimits)
}

pub fn validate_path(path: &str) -> Result<(), PlanError> {
    if path.is_empty()
        || path.len() > MAX_SANDBOX_PATH
        || !path.starts_with('/')
        || path.contains('\0')
        || path.chars().any(char::is_control)
    {
        return Err(PlanError::UnsafePath(path.into()));
    }
    let mut segments = path.split('/');
    segments.next();
    for segment in segments {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(PlanError::UnsafePath(path.into()));
        }
    }
    Ok(())
}

/// True when `path` equals `root` or is a descendant of it. Both must be validated first.
pub fn path_within(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest() -> String {
        "a".repeat(64)
    }

    fn plan(now: i64) -> ExecutionPlan {
        ExecutionPlan {
            version: PLAN_VERSION,
            job_id: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            principal: SubsystemId::Forge,
            capability_grant_id: Uuid::new_v4(),
            executable: ExecutableIdentity {
                sandbox_path: "/usr/bin/cargo".into(),
                sha256: digest(),
            },
            argv: vec!["check".into()],
            cwd: "/work/repo".into(),
            read_roots: vec![MountRoot {
                path: "/usr".into(),
            }],
            write_roots: vec![MountRoot {
                path: "/work".into(),
            }],
            network: None,
            credentials: Vec::new(),
            limits: ExecutionLimits::default(),
            issued_at_ms: now,
            expires_at_ms: now + 60_000,
            audit_correlation_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn a_bounded_plan_validates() {
        let now = 1_800_000_000_000;
        plan(now).validate(now + 1).unwrap();
    }

    #[test]
    fn expired_future_and_overlong_plans_fail() {
        let now = 1_800_000_000_000;
        let mut expired = plan(now - 120_000);
        expired.issued_at_ms = now - 120_000;
        expired.expires_at_ms = now - 60_000;
        assert_eq!(expired.validate(now), Err(PlanError::Expired));

        let future = plan(now + MAX_CLOCK_SKEW_MS + 1);
        assert_eq!(future.validate(now), Err(PlanError::NotYetValid));

        let mut long = plan(now);
        long.expires_at_ms = now + MAX_PLAN_LIFETIME_MS + 1;
        assert_eq!(long.validate(now), Err(PlanError::LifetimeTooLong));
    }

    #[test]
    fn traversal_credentials_and_writable_escapes_are_rejected() {
        let now = 1_800_000_000_000;
        let mut traversal = plan(now);
        traversal.cwd = "/work/../etc".into();
        assert!(traversal.validate(now).is_err());

        let mut home = plan(now);
        home.read_roots.push(MountRoot {
            path: "/home/main".into(),
        });
        assert_eq!(
            home.validate(now),
            Err(PlanError::ForbiddenReadRoot("/home/main".into()))
        );

        let mut system_write = plan(now);
        system_write.write_roots.push(MountRoot {
            path: "/usr/local".into(),
        });
        assert_eq!(
            system_write.validate(now),
            Err(PlanError::UnwritableRoot("/usr/local".into()))
        );
    }

    #[test]
    fn cwd_must_be_inside_a_declared_root() {
        let now = 1_800_000_000_000;
        let mut outside = plan(now);
        outside.cwd = "/cache/x".into();
        assert_eq!(
            outside.validate(now),
            Err(PlanError::CwdOutsideRoots("/cache/x".into()))
        );
    }

    #[test]
    fn bad_digest_and_limits_fail_closed() {
        let now = 1_800_000_000_000;
        let mut bad_digest = plan(now);
        bad_digest.executable.sha256 = "ABC".into();
        assert_eq!(bad_digest.validate(now), Err(PlanError::BadDigest));

        let mut bad_limits = plan(now);
        bad_limits.limits.memory_bytes = 1024;
        assert_eq!(bad_limits.validate(now), Err(PlanError::BadLimits));
    }

    #[test]
    fn command_plan_conversion_is_not_authorization() {
        let now = 1_800_000_000_000;
        let command = sail_protocol::execution::CommandPlan {
            version: 1,
            request_id: Uuid::new_v4(),
            correlation_id: Uuid::new_v4(),
            executable: "/usr/bin/cargo".into(),
            argv: vec!["check".into()],
            limits: sail_protocol::execution::CommandLimits::default(),
        };
        let plan = ExecutionPlan::from_command_plan(
            command,
            SubsystemId::Forge,
            Uuid::new_v4(),
            digest(),
            now,
            60_000,
        );
        plan.validate(now).unwrap();
        assert!(plan.network.is_none());
    }
}
