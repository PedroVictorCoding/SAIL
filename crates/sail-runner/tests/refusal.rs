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
