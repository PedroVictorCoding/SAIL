use sail_protocol::execution::{CommandLimits, CommandPlan, CommandStatus};
use sail_runner::{RunnerConfig, capability, execute};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
fn plan() -> CommandPlan {
    CommandPlan {
        version: 1,
        request_id: Uuid::new_v4(),
        correlation_id: Uuid::new_v4(),
        executable: "/bin/echo".into(),
        argv: vec!["hello".into()],
        limits: CommandLimits::default(),
    }
}
#[tokio::test]
async fn same_uid_runner_is_unavailable_and_cannot_create_host_marker() {
    let mut command = plan();
    let marker = std::env::temp_dir().join(format!("sail-forbidden-{}", Uuid::new_v4()));
    command.executable = "/bin/sh".into();
    command.argv = vec![
        "-c".into(),
        format!("echo forbidden > {}", marker.display()),
    ];
    let uid = unsafe { libc::geteuid() };
    let config = RunnerConfig {
        rootfs: "/".into(),
        cgroup_parent: "/tmp".into(),
        runner_uid: uid,
        daemon_uid: uid,
    };
    let result = execute(&config, &command, CancellationToken::new()).await;
    assert_eq!(result.status, CommandStatus::Unavailable);
    assert!(!marker.exists());
}
#[test]
fn request_binding_covers_argv_budgets_and_task_correlation() {
    let original = plan();
    let original_cap = capability(&original);
    let mut changed = original.clone();
    changed.argv.push("modified".into());
    assert_ne!(capability(&changed), original_cap);
    changed = original.clone();
    changed.limits.pids += 1;
    assert_ne!(capability(&changed), original_cap);
    changed = original;
    changed.correlation_id = Uuid::new_v4();
    assert_ne!(capability(&changed), original_cap);
}
#[test]
fn unsupported_versions_and_unbounded_plans_are_denied() {
    let mut command = plan();
    command.version = 2;
    assert!(command.validate().is_err());
    command = plan();
    command.limits.output_bytes = usize::MAX;
    assert!(command.validate().is_err());
    command = plan();
    command.argv = vec!["a".repeat(33000)];
    assert!(command.validate().is_err());
}

use sail_common::types::SubsystemId;
use sail_runner::{capability_plan, execute_plan};
use sail_sandbox::{
    CredentialRef, ExecutableIdentity, ExecutionLimits, ExecutionPlan, MountRoot, NetworkPolicyRef,
};

fn execution_plan() -> ExecutionPlan {
    let now = chrono::Utc::now().timestamp_millis();
    ExecutionPlan {
        version: 1,
        job_id: Uuid::new_v4(),
        request_id: Uuid::new_v4(),
        principal: SubsystemId::Forge,
        capability_grant_id: Uuid::new_v4(),
        executable: ExecutableIdentity {
            sandbox_path: "/bin/echo".into(),
            sha256: "a".repeat(64),
        },
        argv: vec!["hello".into()],
        cwd: "/work".into(),
        read_roots: vec![MountRoot {
            path: "/bin".into(),
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

fn same_uid_config() -> RunnerConfig {
    let uid = unsafe { libc::geteuid() };
    RunnerConfig {
        rootfs: "/".into(),
        cgroup_parent: "/tmp".into(),
        runner_uid: uid,
        daemon_uid: uid,
    }
}

#[tokio::test]
async fn a_valid_plan_reaches_the_same_fail_closed_boundary() {
    let marker = std::env::temp_dir().join(format!("sail-forbidden-{}", Uuid::new_v4()));
    let mut plan = execution_plan();
    plan.executable.sandbox_path = "/bin/sh".into();
    plan.argv = vec![
        "-c".into(),
        format!("echo forbidden > {}", marker.display()),
    ];
    let result = execute_plan(&same_uid_config(), &plan, CancellationToken::new()).await;
    assert_eq!(result.status, CommandStatus::Unavailable);
    assert!(!marker.exists());
}

#[tokio::test]
async fn network_and_credential_plans_are_denied_before_any_spawn() {
    let mut plan = execution_plan();
    plan.network = Some(NetworkPolicyRef {
        policy_id: "registry".into(),
        policy_version: 1,
    });
    let result = execute_plan(&same_uid_config(), &plan, CancellationToken::new()).await;
    assert_eq!(result.status, CommandStatus::Denied);
    assert!(result.stderr.contains("network"));

    plan.network = None;
    plan.credentials.push(CredentialRef {
        handle_id: Uuid::new_v4(),
        purpose: "github:issues".into(),
    });
    let result = execute_plan(&same_uid_config(), &plan, CancellationToken::new()).await;
    assert_eq!(result.status, CommandStatus::Denied);
    assert!(result.stderr.contains("credential"));
}

#[tokio::test]
async fn expired_plans_are_denied() {
    let mut plan = execution_plan();
    plan.issued_at_ms = chrono::Utc::now().timestamp_millis() - 120_000;
    plan.expires_at_ms = chrono::Utc::now().timestamp_millis() - 60_000;
    let result = execute_plan(&same_uid_config(), &plan, CancellationToken::new()).await;
    assert_eq!(result.status, CommandStatus::Denied);
}

#[test]
fn plan_capability_covers_every_binding_field() {
    let original = execution_plan();
    let original_cap = capability_plan(&original);
    let mut changed = original.clone();
    changed.argv.push("modified".into());
    assert_ne!(capability_plan(&changed), original_cap);
    changed = original.clone();
    changed.audit_correlation_id = Uuid::new_v4();
    assert_ne!(capability_plan(&changed), original_cap);
    changed = original.clone();
    changed.write_roots.push(MountRoot {
        path: "/tmp".into(),
    });
    assert_ne!(capability_plan(&changed), original_cap);
}
