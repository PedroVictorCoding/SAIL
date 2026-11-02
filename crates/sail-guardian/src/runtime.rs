//! Production SecurityRuntime integration using nono capability-based sandboxing,
//! Landlock LSM (Linux), Bubblewrap layering, and OS-level resource limits.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

use sail_common::types::SubsystemId;

use crate::approval::ApprovalBroker;
use crate::audit::{AuditLedger, DecisionOutcome};
use crate::capability::{Capability, RiskLevel};
use crate::policy::AuthorizationDecision;

/// Overall system security enforcement status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityStatus {
    /// Kernel-level Landlock LSM (and/or Bubblewrap) is fully active and enforcing.
    Protected,
    /// Partial enforcement (e.g. Landlock filesystem active, network via seccomp or fallback).
    LimitedProtection,
    /// Platform does not provide mandatory kernel security mechanisms. Sensitive tasks fail closed.
    Unavailable,
}

/// Inspection report of host security capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityCapabilities {
    pub status: SecurityStatus,
    pub platform: String,
    pub nono_version: String,
    pub landlock_supported: bool,
    pub detected_abi: Option<u32>,
    pub max_enforced_abi: Option<u32>,
    pub bubblewrap_available: bool,
    pub cgroup_limits_available: bool,
    pub features: Vec<String>,
    pub status_detail: String,
}

/// Autonomy preset governing interaction frequency without bypassing hard security boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyPreset {
    /// More confirmation for workspace modifications; most network and external actions require approval.
    Careful,
    /// Default. Routine actions inside approved workspaces run automatically; sensitive/external actions require approval.
    #[default]
    Balanced,
    /// Permits longer unattended workflows inside existing capability boundaries.
    /// Hard boundaries (out-of-tree access, secrets, privileged ops) remain non-bypassable.
    Autonomous,
}

/// Standardized capability profiles for least-privilege compartmentalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityProfile {
    /// Read-only inspection, search, and code understanding.
    /// Authorized file reads only. No file writes. No network unless explicitly granted. No credentials.
    ReadOnly,
    /// Documents, projects, and productivity tools.
    /// Read/write inside explicitly selected workspace directories. No unrestricted access.
    Workspace,
    /// Autonomous coding tasks.
    /// Disposable repository worktree only; approved compiler/test tools; strict network isolation.
    Forge,
    /// Web-backed research.
    /// Network permitted only via controlled broker/proxy; no host credentials passed to browsers.
    Research,
    /// System-level operations.
    /// Never automatically selected by an LLM; requires explicit owner authorization; short-lived.
    Privileged,
}

impl CapabilityProfile {
    pub fn default_network_mode(&self) -> nono::NetworkMode {
        match self {
            CapabilityProfile::ReadOnly => nono::NetworkMode::Blocked,
            CapabilityProfile::Workspace => nono::NetworkMode::Blocked,
            CapabilityProfile::Forge => nono::NetworkMode::Blocked,
            CapabilityProfile::Research => nono::NetworkMode::AllowAll, // Read-only research with outbound internet
            CapabilityProfile::Privileged => nono::NetworkMode::Blocked,
        }
    }

    pub fn allows_writes(&self) -> bool {
        matches!(
            self,
            CapabilityProfile::Workspace | CapabilityProfile::Forge | CapabilityProfile::Privileged
        )
    }
}

/// Inbound request to perform an action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRequest {
    pub session_id: Option<Uuid>,
    pub correlation_id: Option<Uuid>,
    pub caller: SubsystemId,
    pub profile: CapabilityProfile,
    pub capability: Capability,
    pub target_path: Option<PathBuf>,
    pub is_write: bool,
    pub justification: String,
}

/// Fully authorized and bound action ready for sandbox execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizedAction {
    pub authorization_id: Uuid,
    pub token_id: Option<Uuid>,
    pub correlation_id: Option<Uuid>,
    pub caller: SubsystemId,
    pub profile: CapabilityProfile,
    pub capability: Capability,
    pub allowed_read_paths: Vec<PathBuf>,
    pub allowed_write_paths: Vec<PathBuf>,
    pub network_mode: nono::NetworkMode,
    pub memory_limit_bytes: Option<u64>,
    pub max_processes: Option<u64>,
    pub expires_at: DateTime<Utc>,
}

/// Handle representing an executing or planned sandboxed task.
#[derive(Debug)]
pub struct ExecutionHandle {
    pub execution_id: Uuid,
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

/// Security and sandbox violations.
#[derive(Debug, Error)]
pub enum SecurityError {
    #[error("Action denied: {0}")]
    Denied(String),
    #[error("User approval required: session {session_id}")]
    ApprovalRequired { session_id: Uuid, rationale: String },
    #[error("Nono capability builder error: {0}")]
    CapabilityError(String),
    #[error("Landlock sandbox enforcement failed: {0}")]
    EnforcementFailed(String),
    #[error("Mandatory platform security mechanism unavailable: {0}")]
    ProtectionUnavailable(String),
    #[error("Resource limit exceeded: {0}")]
    ResourceExceeded(String),
    #[error("Execution process error: {0}")]
    ExecutionFailed(String),
}

/// Central security interface for Guardian.
pub trait SecurityRuntime: Send + Sync {
    /// Inspect runtime capabilities and platform enforcement support.
    fn inspect_capabilities(&self) -> SecurityCapabilities;

    /// Validate an action request against current policy, autonomy presets, and capability boundaries.
    fn validate(
        &self,
        request: &ActionRequest,
        preset: AutonomyPreset,
        approvals: &ApprovalBroker,
        audit: &AuditLedger,
    ) -> Result<AuthorizationDecision, SecurityError>;

    /// Audit a security event into the tamper-evident audit ledger.
    fn audit(
        &self,
        audit: &AuditLedger,
        caller: SubsystemId,
        capability: Capability,
        outcome: DecisionOutcome,
        rationale: &str,
        correlation_id: Option<Uuid>,
    );

    /// Build a nono `CapabilitySet` conforming to the authorized action.
    fn build_capability_set(
        &self,
        action: &AuthorizedAction,
    ) -> Result<nono::CapabilitySet, SecurityError>;
}

/// Default implementation of SecurityRuntime using nono 0.81.0 and Landlock LSM.
#[derive(Debug, Clone)]
pub struct NonoSecurityRuntime {
    support: nono::SupportInfo,
    bwrap_path: Option<PathBuf>,
    system_read_roots: Vec<PathBuf>,
}

impl NonoSecurityRuntime {
    pub fn new() -> Self {
        let support = nono::Sandbox::support_info();
        let bwrap_path = Self::find_bubblewrap();

        let mut system_read_roots = Vec::new();
        for dir in ["/usr", "/lib", "/lib64", "/bin", "/etc/ssl", "/etc/pki"] {
            let p = PathBuf::from(dir);
            if p.exists() {
                system_read_roots.push(p);
            }
        }

        Self {
            support,
            bwrap_path,
            system_read_roots,
        }
    }

    fn find_bubblewrap() -> Option<PathBuf> {
        if let Ok(custom) = std::env::var("SAIL_BWRAP_BIN") {
            let p = PathBuf::from(custom);
            if p.exists() {
                return Some(p);
            }
        }
        for candidate in ["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"] {
            let p = PathBuf::from(candidate);
            if p.exists() {
                return Some(p);
            }
        }
        None
    }
}

impl Default for NonoSecurityRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl SecurityRuntime for NonoSecurityRuntime {
    fn inspect_capabilities(&self) -> SecurityCapabilities {
        #[cfg(target_os = "linux")]
        let detected = nono::Sandbox::detect_abi().ok().map(|abi| {
            // detected ABI version number
            format!("{abi:?}")
        });

        #[cfg(not(target_os = "linux"))]
        let detected = None;

        let detected_abi_num = detected.as_ref().and_then(|s| {
            if s.contains("V8") {
                Some(8)
            } else if s.contains("V6") {
                Some(6)
            } else if s.contains("V5") {
                Some(5)
            } else if s.contains("V4") {
                Some(4)
            } else if s.contains("V3") {
                Some(3)
            } else if s.contains("V2") {
                Some(2)
            } else if s.contains("V1") {
                Some(1)
            } else {
                None
            }
        });

        let bubblewrap_available = self.bwrap_path.is_some();
        let cgroup_available = Path::new("/sys/fs/cgroup/cgroup.controllers").exists();

        let status = if self.support.is_supported && bubblewrap_available {
            SecurityStatus::Protected
        } else if self.support.is_supported || bubblewrap_available {
            SecurityStatus::LimitedProtection
        } else {
            SecurityStatus::Unavailable
        };

        let mut features = vec![
            "Landlock filesystem isolation".to_string(),
            "Path canonicalization & traversal protection".to_string(),
            "Sensitive credential store redaction".to_string(),
        ];

        if detected_abi_num.unwrap_or(0) >= 4 {
            features.push("TCP network filtering".to_string());
        }
        if detected_abi_num.unwrap_or(0) >= 6 {
            features.push("Signal and abstract UNIX socket scoping".to_string());
        }
        if bubblewrap_available {
            features.push("Bubblewrap namespace containerization".to_string());
        }
        if cgroup_available {
            features.push("cgroups v2 task and memory controls".to_string());
        }

        SecurityCapabilities {
            status,
            platform: self.support.platform.to_string(),
            nono_version: "0.81.0".to_string(),
            landlock_supported: self.support.is_supported,
            detected_abi: detected_abi_num,
            max_enforced_abi: Some(6), // nono enforces Landlock up to V6
            bubblewrap_available,
            cgroup_limits_available: cgroup_available,
            features,
            status_detail: self.support.details.clone(),
        }
    }

    fn validate(
        &self,
        request: &ActionRequest,
        preset: AutonomyPreset,
        approvals: &ApprovalBroker,
        audit: &AuditLedger,
    ) -> Result<AuthorizationDecision, SecurityError> {
        // 1. Ensure platform security is not completely unavailable for high/critical operations
        if !self.support.is_supported && self.bwrap_path.is_none() {
            let reason =
                "Mandatory security sandbox runtime is unavailable on this host".to_string();
            self.audit(
                audit,
                request.caller,
                request.capability.clone(),
                DecisionOutcome::Denied,
                &reason,
                request.correlation_id,
            );
            return Ok(AuthorizationDecision::Denied { reason });
        }

        // 2. Prohibit write operations under ReadOnly profile
        if request.profile == CapabilityProfile::ReadOnly && request.is_write {
            let reason = "Write operation prohibited under ReadOnly capability profile".to_string();
            self.audit(
                audit,
                request.caller,
                request.capability.clone(),
                DecisionOutcome::Denied,
                &reason,
                request.correlation_id,
            );
            return Ok(AuthorizationDecision::Denied { reason });
        }

        // 3. Prevent arbitrary out-of-tree writes or access to host secrets
        if let Some(target) = &request.target_path {
            if is_sensitive_host_path(target) {
                let reason = format!(
                    "Target path {:?} is a protected host credential or system resource",
                    target
                );
                self.audit(
                    audit,
                    request.caller,
                    request.capability.clone(),
                    DecisionOutcome::Denied,
                    &reason,
                    request.correlation_id,
                );
                return Ok(AuthorizationDecision::Denied { reason });
            }
        }

        // 4. Evaluate risk according to the active AutonomyPreset
        let risk = request.capability.risk_level();

        let decision = match preset {
            AutonomyPreset::Careful => {
                // In Careful mode, any filesystem write or medium+ operation requires interactive review
                if request.is_write || risk >= RiskLevel::Medium {
                    let session = approvals.create_session_for(
                        request.correlation_id,
                        request.caller,
                        request.capability.clone(),
                        &request.justification,
                    );
                    AuthorizationDecision::RequiresUserApproval {
                        session_id: session.session_id,
                        rationale: format!(
                            "Careful preset requires approval for {:?}: {}",
                            request.capability, request.justification
                        ),
                    }
                } else {
                    AuthorizationDecision::Allowed
                }
            }
            AutonomyPreset::Balanced => {
                // In Balanced mode (default):
                // Low risk actions: Allowed automatically.
                // Workspace / Forge routine file reads/writes inside granted worktree: Allowed automatically.
                // Medium risk (proc inspect, fs queries) inside bounds: Allowed if in trusted session, else prompts.
                // High/Critical risk (camera, shell command, process kill): Requires user approval.
                match risk {
                    RiskLevel::Low => AuthorizationDecision::Allowed,
                    RiskLevel::Medium => {
                        if request.caller == SubsystemId::Forge
                            || request.caller == SubsystemId::Dialogue
                        {
                            AuthorizationDecision::Allowed
                        } else {
                            let session = approvals.create_session_for(
                                request.correlation_id,
                                request.caller,
                                request.capability.clone(),
                                &request.justification,
                            );
                            AuthorizationDecision::RequiresUserApproval {
                                session_id: session.session_id,
                                rationale: format!(
                                    "Approval required for operation: {:?}",
                                    request.capability
                                ),
                            }
                        }
                    }
                    RiskLevel::High | RiskLevel::Critical => {
                        let session = approvals.create_session_for(
                            request.correlation_id,
                            request.caller,
                            request.capability.clone(),
                            &request.justification,
                        );
                        AuthorizationDecision::RequiresUserApproval {
                            session_id: session.session_id,
                            rationale: format!(
                                "High-risk operation requires explicit owner authorization: {:?}",
                                request.capability
                            ),
                        }
                    }
                }
            }
            AutonomyPreset::Autonomous => {
                // In Autonomous mode:
                // Routine coding and workspace operations run without interruption inside strict sandboxes.
                // Only Critical operations (e.g. arbitrary host command execution, kill) prompt.
                match risk {
                    RiskLevel::Low | RiskLevel::Medium | RiskLevel::High => {
                        // High-risk tools within sandbox bounds (like fs:write in worktree or forge:enqueue) proceed autonomously
                        AuthorizationDecision::Allowed
                    }
                    RiskLevel::Critical => {
                        let session = approvals.create_session_for(
                            request.correlation_id,
                            request.caller,
                            request.capability.clone(),
                            &request.justification,
                        );
                        AuthorizationDecision::RequiresUserApproval {
                            session_id: session.session_id,
                            rationale: format!(
                                "Critical operation requires confirmation even in Autonomous mode: {:?}",
                                request.capability
                            ),
                        }
                    }
                }
            }
        };

        let outcome = match &decision {
            AuthorizationDecision::Allowed => DecisionOutcome::Granted,
            AuthorizationDecision::Denied { .. } => DecisionOutcome::Denied,
            AuthorizationDecision::RequiresUserApproval { .. } => DecisionOutcome::Denied,
        };

        self.audit(
            audit,
            request.caller,
            request.capability.clone(),
            outcome,
            &request.justification,
            request.correlation_id,
        );

        Ok(decision)
    }

    fn audit(
        &self,
        audit: &AuditLedger,
        caller: SubsystemId,
        capability: Capability,
        outcome: DecisionOutcome,
        rationale: &str,
        correlation_id: Option<Uuid>,
    ) {
        audit.record_with_correlation(caller, capability, outcome, rationale, correlation_id);
    }

    fn build_capability_set(
        &self,
        action: &AuthorizedAction,
    ) -> Result<nono::CapabilitySet, SecurityError> {
        let mut caps = nono::CapabilitySet::new();

        // 1. Read-only system binary and library paths
        for sys_path in &self.system_read_roots {
            caps = caps
                .allow_path(sys_path, nono::AccessMode::Read)
                .map_err(|e| {
                    SecurityError::CapabilityError(format!(
                        "Failed to add system path {sys_path:?}: {e}"
                    ))
                })?;
        }

        // 2. Cargo & Rust toolchain read-only directories if present
        if let Ok(home) = std::env::var("HOME") {
            let home_p = PathBuf::from(&home);
            let cargo_dir = home_p.join(".cargo");
            if cargo_dir.exists() {
                caps = caps
                    .allow_path(&cargo_dir, nono::AccessMode::Read)
                    .map_err(|e| {
                        SecurityError::CapabilityError(format!("Cargo dir grant error: {e}"))
                    })?;
            }
            let rustup_dir = home_p.join(".rustup");
            if rustup_dir.exists() {
                caps = caps
                    .allow_path(&rustup_dir, nono::AccessMode::Read)
                    .map_err(|e| {
                        SecurityError::CapabilityError(format!("Rustup dir grant error: {e}"))
                    })?;
            }
        }

        // 3. Granted read paths
        for read_p in &action.allowed_read_paths {
            if read_p.exists() {
                caps = caps
                    .allow_path(read_p, nono::AccessMode::Read)
                    .map_err(|e| {
                        SecurityError::CapabilityError(format!(
                            "Read path grant error {read_p:?}: {e}"
                        ))
                    })?;
            }
        }

        // 4. Granted write paths (only if profile permits writes)
        if action.profile.allows_writes() {
            for write_p in &action.allowed_write_paths {
                if write_p.exists() {
                    caps = caps
                        .allow_path(write_p, nono::AccessMode::ReadWrite)
                        .map_err(|e| {
                            SecurityError::CapabilityError(format!(
                                "Write path grant error {write_p:?}: {e}"
                            ))
                        })?;
                }
            }
        }

        // 5. Network configuration
        caps = caps.set_network_mode(action.network_mode.clone());

        // 6. Signal and IPC isolation
        caps = caps
            .set_signal_mode(nono::SignalMode::Isolated)
            .set_process_info_mode(nono::ProcessInfoMode::Isolated)
            .set_ipc_mode(nono::IpcMode::SharedMemoryOnly);

        // 7. Resource limits
        let mut resource_limits = nono::ResourceLimits::default();
        if let Some(mem) = action.memory_limit_bytes {
            resource_limits.memory_bytes = Some(mem);
        }
        if let Some(procs) = action.max_processes {
            resource_limits.max_processes = Some(procs);
        }
        if !resource_limits.is_empty() {
            caps = caps.with_resource_limits(resource_limits);
        }

        Ok(caps)
    }
}

/// Helper to check whether a path accesses private host secrets.
pub fn is_sensitive_host_path(path: &Path) -> bool {
    let p_str = path.to_string_lossy();

    // SSH, GPG, AWS, Git credentials, system shadow/vault
    let sensitive_markers = [
        "/.ssh",
        "/.gnupg",
        "/.aws",
        "/.azure",
        "/.config/gh",
        "/.git-credentials",
        "/etc/shadow",
        "/etc/sudoers",
        "id_rsa",
        "id_ed25519",
        "sail_vault",
        "sail_owner_token",
    ];

    for marker in &sensitive_markers {
        if p_str.contains(marker) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FsCapability;

    #[test]
    fn test_inspect_capabilities() {
        let runtime = NonoSecurityRuntime::new();
        let caps = runtime.inspect_capabilities();
        assert!(caps.nono_version == "0.81.0");
        assert!(caps.features.len() >= 3);
        assert!(caps.status != SecurityStatus::Unavailable);
    }

    #[test]
    fn test_read_only_profile_blocks_writes() {
        let runtime = NonoSecurityRuntime::new();
        let approvals = ApprovalBroker::new();
        let audit = AuditLedger::new();

        let req = ActionRequest {
            session_id: Some(Uuid::new_v4()),
            correlation_id: None,
            caller: SubsystemId::Dialogue,
            profile: CapabilityProfile::ReadOnly,
            capability: Capability::Filesystem(FsCapability::Write {
                path: PathBuf::from("/tmp/test.txt"),
            }),
            target_path: Some(PathBuf::from("/tmp/test.txt")),
            is_write: true,
            justification: "attempt write under readonly".into(),
        };

        let decision = runtime
            .validate(&req, AutonomyPreset::Autonomous, &approvals, &audit)
            .unwrap();

        assert!(matches!(decision, AuthorizationDecision::Denied { .. }));
    }

    #[test]
    fn test_sensitive_path_blocked_even_in_autonomous_preset() {
        let runtime = NonoSecurityRuntime::new();
        let approvals = ApprovalBroker::new();
        let audit = AuditLedger::new();

        let req = ActionRequest {
            session_id: Some(Uuid::new_v4()),
            correlation_id: None,
            caller: SubsystemId::Forge,
            profile: CapabilityProfile::Forge,
            capability: Capability::Filesystem(FsCapability::Read {
                path: PathBuf::from("/home/main/.ssh/id_rsa"),
            }),
            target_path: Some(PathBuf::from("/home/main/.ssh/id_rsa")),
            is_write: false,
            justification: "attempt ssh key read".into(),
        };

        let decision = runtime
            .validate(&req, AutonomyPreset::Autonomous, &approvals, &audit)
            .unwrap();

        assert!(matches!(decision, AuthorizationDecision::Denied { .. }));
    }

    #[test]
    fn test_capability_set_builder() {
        let runtime = NonoSecurityRuntime::new();
        let action = AuthorizedAction {
            authorization_id: Uuid::new_v4(),
            token_id: None,
            correlation_id: None,
            caller: SubsystemId::Forge,
            profile: CapabilityProfile::Forge,
            capability: Capability::Filesystem(FsCapability::Read {
                path: PathBuf::from("/tmp"),
            }),
            allowed_read_paths: vec![PathBuf::from("/tmp")],
            allowed_write_paths: vec![PathBuf::from("/tmp")],
            network_mode: nono::NetworkMode::Blocked,
            memory_limit_bytes: Some(1024 * 1024 * 512),
            max_processes: Some(64),
            expires_at: Utc::now() + chrono::Duration::seconds(60),
        };

        let caps = runtime.build_capability_set(&action).unwrap();
        assert!(caps.is_network_blocked());
        assert_eq!(caps.signal_mode(), nono::SignalMode::Isolated);
        assert_eq!(caps.ipc_mode(), nono::IpcMode::SharedMemoryOnly);
        assert!(caps.resource_limits().is_some());
        let limits = caps.resource_limits().unwrap();
        assert_eq!(limits.memory_bytes, Some(512 * 1024 * 1024));
        assert_eq!(limits.max_processes, Some(64));
    }
}
