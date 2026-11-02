//! Comprehensive integration tests for nono-backed Guardian SecurityRuntime.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

use sail_common::types::SubsystemId;
use sail_guardian::{
    ActionRequest, ApprovalBroker, AuditLedger, AutonomyPreset, Capability, CapabilityProfile,
    FsCapability, GuardianExecutionBroker, NonoSecurityRuntime, PolicyEngine, SandboxPolicy,
    SecurityRuntime, SecurityStatus, SystemCapability, is_sensitive_host_path,
};

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("sail-test-nono-{}", Uuid::new_v4()));
        let _ = fs::create_dir_all(&path);
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn test_nono_runtime_reports_genuine_enforcement() {
    let runtime = NonoSecurityRuntime::new();
    let caps = runtime.inspect_capabilities();

    assert_eq!(caps.nono_version, "0.81.0");
    assert!(caps.landlock_supported);
    assert_eq!(caps.status, SecurityStatus::Protected);
    assert!(
        caps.features
            .contains(&"Landlock filesystem isolation".to_string())
    );
    assert!(
        caps.features
            .contains(&"Bubblewrap namespace containerization".to_string())
    );
    assert!(
        caps.features
            .contains(&"cgroups v2 task and memory controls".to_string())
    );
}

#[test]
fn test_authorized_worker_can_read_and_write_within_assigned_worktree() {
    let tmp = TestDir::new();
    let worktree_dir = tmp.path.join("worktree");
    fs::create_dir_all(&worktree_dir).unwrap();

    let runtime = NonoSecurityRuntime::new();
    let approvals = ApprovalBroker::new();
    let audit = AuditLedger::new();

    let target_file = worktree_dir.join("main.rs");
    fs::write(&target_file, b"fn main() {}").unwrap();

    // 1. Read request
    let read_req = ActionRequest {
        session_id: Some(Uuid::new_v4()),
        correlation_id: None,
        caller: SubsystemId::Forge,
        profile: CapabilityProfile::Forge,
        capability: Capability::Filesystem(FsCapability::Read {
            path: target_file.clone(),
        }),
        target_path: Some(target_file.clone()),
        is_write: false,
        justification: "reading source for task".into(),
    };

    let decision = runtime
        .validate(&read_req, AutonomyPreset::Autonomous, &approvals, &audit)
        .unwrap();
    assert!(matches!(
        decision,
        sail_guardian::AuthorizationDecision::Allowed
    ));

    // 2. Write request within worktree
    let write_req = ActionRequest {
        session_id: Some(Uuid::new_v4()),
        correlation_id: None,
        caller: SubsystemId::Forge,
        profile: CapabilityProfile::Forge,
        capability: Capability::Filesystem(FsCapability::Write {
            path: target_file.clone(),
        }),
        target_path: Some(target_file.clone()),
        is_write: true,
        justification: "updating source code".into(),
    };

    let write_decision = runtime
        .validate(&write_req, AutonomyPreset::Autonomous, &approvals, &audit)
        .unwrap();
    assert!(matches!(
        write_decision,
        sail_guardian::AuthorizationDecision::Allowed
    ));
}

#[test]
fn test_unrelated_private_files_and_credentials_are_strictly_blocked() {
    let runtime = NonoSecurityRuntime::new();
    let approvals = ApprovalBroker::new();
    let audit = AuditLedger::new();

    let private_paths = [
        "/home/main/.ssh/id_rsa",
        "/home/main/.ssh/id_ed25519",
        "/home/main/.gnupg/secring.gpg",
        "/home/main/.aws/credentials",
        "/etc/shadow",
        "/srv/sail/.local/sail_owner_token",
        "/tmp/sail_vault/keys",
    ];

    for path_str in private_paths {
        let p = PathBuf::from(path_str);
        assert!(
            is_sensitive_host_path(&p),
            "Path {path_str} must be detected as sensitive"
        );

        let req = ActionRequest {
            session_id: Some(Uuid::new_v4()),
            correlation_id: None,
            caller: SubsystemId::Forge,
            profile: CapabilityProfile::Forge,
            capability: Capability::Filesystem(FsCapability::Read { path: p.clone() }),
            target_path: Some(p.clone()),
            is_write: false,
            justification: "attempt exfiltrate secret".into(),
        };

        // Must be denied even under Autonomous preset!
        let decision = runtime
            .validate(&req, AutonomyPreset::Autonomous, &approvals, &audit)
            .unwrap();

        assert!(
            matches!(
                decision,
                sail_guardian::AuthorizationDecision::Denied { .. }
            ),
            "Expected Denied for sensitive path {path_str}"
        );
    }
}

#[test]
fn test_readonly_profile_prohibits_any_filesystem_write() {
    let tmp = TestDir::new();
    let target = tmp.path.join("doc.md");
    fs::write(&target, b"# Doc").unwrap();

    let runtime = NonoSecurityRuntime::new();
    let approvals = ApprovalBroker::new();
    let audit = AuditLedger::new();

    let write_req = ActionRequest {
        session_id: Some(Uuid::new_v4()),
        correlation_id: None,
        caller: SubsystemId::Dialogue,
        profile: CapabilityProfile::ReadOnly,
        capability: Capability::Filesystem(FsCapability::Write {
            path: target.clone(),
        }),
        target_path: Some(target.clone()),
        is_write: true,
        justification: "modifying doc under readonly profile".into(),
    };

    let decision = runtime
        .validate(&write_req, AutonomyPreset::Autonomous, &approvals, &audit)
        .unwrap();

    assert!(matches!(
        decision,
        sail_guardian::AuthorizationDecision::Denied { .. }
    ));
}

#[test]
fn test_autonomy_presets_adjust_approval_frequency_without_bypassing_boundaries() {
    let runtime = NonoSecurityRuntime::new();
    let approvals = ApprovalBroker::new();
    let audit = AuditLedger::new();

    let req = ActionRequest {
        session_id: Some(Uuid::new_v4()),
        correlation_id: None,
        caller: SubsystemId::Dialogue,
        profile: CapabilityProfile::Workspace,
        capability: Capability::Filesystem(FsCapability::Write {
            path: PathBuf::from("/tmp/workspace/file.txt"),
        }),
        target_path: Some(PathBuf::from("/tmp/workspace/file.txt")),
        is_write: true,
        justification: "writing workspace file".into(),
    };

    // 1. In Careful mode: write requires explicit user approval
    let careful_decision = runtime
        .validate(&req, AutonomyPreset::Careful, &approvals, &audit)
        .unwrap();
    assert!(matches!(
        careful_decision,
        sail_guardian::AuthorizationDecision::RequiresUserApproval { .. }
    ));

    // 2. In Autonomous mode: workspace write inside granted bounds is allowed
    let auto_decision = runtime
        .validate(&req, AutonomyPreset::Autonomous, &approvals, &audit)
        .unwrap();
    assert!(matches!(
        auto_decision,
        sail_guardian::AuthorizationDecision::Allowed
    ));

    // 3. Critical operations (e.g. process kill or shell command) ALWAYS require approval even in Autonomous mode
    let critical_req = ActionRequest {
        session_id: Some(Uuid::new_v4()),
        correlation_id: None,
        caller: SubsystemId::Dialogue,
        profile: CapabilityProfile::Privileged,
        capability: Capability::System(SystemCapability::ProcessKill),
        target_path: None,
        is_write: false,
        justification: "terminating host process".into(),
    };

    let critical_decision = runtime
        .validate(
            &critical_req,
            AutonomyPreset::Autonomous,
            &approvals,
            &audit,
        )
        .unwrap();
    assert!(matches!(
        critical_decision,
        sail_guardian::AuthorizationDecision::RequiresUserApproval { .. }
    ));
}

#[test]
fn test_broker_audit_chain_and_preset_sync() {
    let policy = Arc::new(PolicyEngine::new());
    let audit = Arc::new(AuditLedger::new());
    let approval = Arc::new(ApprovalBroker::new());
    let sandbox = Arc::new(SandboxPolicy::new());

    let broker = GuardianExecutionBroker::new(policy.clone(), audit.clone(), approval, sandbox);

    // Initial preset is Balanced
    assert_eq!(broker.policy().autonomy_preset(), AutonomyPreset::Balanced);

    // Switch to Autonomous
    broker
        .policy()
        .set_autonomy_preset(AutonomyPreset::Autonomous);
    assert_eq!(
        broker.policy().autonomy_preset(),
        AutonomyPreset::Autonomous
    );

    // Verify audit chain
    assert!(audit.verify_chain());
}
