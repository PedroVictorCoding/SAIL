use std::sync::{Arc, Mutex};
static GUARDIAN_FIXTURE: Mutex<()> = Mutex::new(());
use sail_common::types::SubsystemId;
use sail_dialogue::{KernelToolCall, LocalKernelToolBroker};
use sail_forge::{
    ForgeOrchestrator, OrchestratorConfig, ReviewBroker, ReviewPolicyConfig, TaskCheckpointStore,
};
use sail_guardian::GuardianSubsystem;

#[test]
fn test_forge_tools_enqueue_and_status() {
    let _fixture = GUARDIAN_FIXTURE.lock().unwrap();
    let guardian = GuardianSubsystem::new();
    let checkpoint_dir =
        std::env::temp_dir().join(format!("sail-checkpoint-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&checkpoint_dir).unwrap();
    let checkpoint_store = Arc::new(TaskCheckpointStore::new(checkpoint_dir));
    let review_broker = Arc::new(ReviewBroker::new(ReviewPolicyConfig::default()));
    let orchestrator = Arc::new(ForgeOrchestrator::new(
        OrchestratorConfig::default(),
        checkpoint_store,
        review_broker,
        guardian.sandbox(),
    ));
    sail_forge::register_global_orchestrator(orchestrator);

    // 1. Enqueue an issue via kernel tool (triggers Guardian approval gate for High risk)
    let enqueue_call = KernelToolCall {
        tool: "forge:enqueue".into(),
        params: serde_json::json!({
            "repo": "PedroVictorCoding/SAIL",
            "issue_number": 90,
            "task": "Test issue resolution from UI"
        }),
    };
    let enqueue_denied = LocalKernelToolBroker::execute(&enqueue_call);
    assert!(!enqueue_denied.success);
    assert_eq!(enqueue_denied.output["approval_required"], true);
    let session_id: uuid::Uuid = enqueue_denied.output["session_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let broker = sail_guardian::global_broker().unwrap();
    let token = broker.approval().resolve_session(session_id, true).unwrap();

    let enqueue_res = LocalKernelToolBroker::execute_guarded(
        SubsystemId::Dialogue,
        &enqueue_call,
        Some(token.token_id),
    );
    assert!(
        enqueue_res.success,
        "forge:enqueue should succeed: {:?}",
        enqueue_res.output
    );
    let job_id = enqueue_res.output["job_id"]
        .as_str()
        .expect("job_id present")
        .to_string();
    assert!(job_id.starts_with("job-90-"));

    // 2. Query status via kernel tool (Low risk telemetry succeeds immediately)
    let status_call = KernelToolCall {
        tool: "forge:status".into(),
        params: serde_json::json!({
            "job_id": job_id
        }),
    };
    let status_res = LocalKernelToolBroker::execute(&status_call);
    assert!(
        status_res.success,
        "forge:status should succeed: {:?}",
        status_res.output
    );
    assert_eq!(status_res.output["job_id"], job_id);

    // 3. List all jobs via kernel tool
    let list_call = KernelToolCall {
        tool: "forge:list".into(),
        params: serde_json::json!({}),
    };
    let list_res = LocalKernelToolBroker::execute(&list_call);
    assert!(
        list_res.success,
        "forge:list should succeed: {:?}",
        list_res.output
    );
    assert!(list_res.output["count"].as_u64().unwrap() >= 1);
}

#[test]
fn test_filesystem_read_write_guardian_approval_lifecycle() {
    let _fixture = GUARDIAN_FIXTURE.lock().unwrap();
    let temp_workspace =
        std::env::temp_dir().join(format!("sail-workspace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_workspace).unwrap();
    let workspace_path = temp_workspace.to_str().unwrap().to_string();
    unsafe {
        std::env::set_var("SAIL_TOOL_ROOT", &workspace_path);
    }
    let _guardian = GuardianSubsystem::new();

    let test_file = "hello.rs";
    let test_content = "fn main() { println!(\"Hello, SAIL One UI!\"); }";

    // 1. fs:write without token must trigger Guardian approval requirement
    let write_call = KernelToolCall {
        tool: "fs:write".into(),
        params: serde_json::json!({
            "path": test_file,
            "content": test_content
        }),
    };
    let write_denied = LocalKernelToolBroker::execute(&write_call);
    assert!(!write_denied.success);
    assert_eq!(write_denied.output["approval_required"], true);
    assert_eq!(write_denied.output["error_code"], "permission_denied");
    let session_id_str = write_denied.output["session_id"].as_str().unwrap();
    let session_uuid: uuid::Uuid = session_id_str.parse().unwrap();

    // 2. Trusted in-process fixture resolves approval; HTTP owner authentication has separate end-to-end coverage.
    let broker = sail_guardian::global_broker().unwrap();
    let token = broker
        .approval()
        .resolve_session(session_uuid, true)
        .expect("Trusted fixture resolves approval");

    // 3. Re-execute guarded tool with the issued capability token
    let write_approved = LocalKernelToolBroker::execute_guarded(
        SubsystemId::Dialogue,
        &write_call,
        Some(token.token_id),
    );
    assert!(
        write_approved.success,
        "Write should succeed with token: {:?}",
        write_approved.output
    );
    assert_eq!(write_approved.output["bytes_written"], test_content.len());

    // 4. fs:read without token triggers approval
    let read_call = KernelToolCall {
        tool: "fs:read".into(),
        params: serde_json::json!({
            "path": test_file
        }),
    };
    let read_denied = LocalKernelToolBroker::execute(&read_call);
    assert!(!read_denied.success);
    assert_eq!(read_denied.output["approval_required"], true);
    let read_session_uuid: uuid::Uuid = read_denied.output["session_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let read_token = broker
        .approval()
        .resolve_session(read_session_uuid, true)
        .unwrap();

    let read_approved = LocalKernelToolBroker::execute_guarded(
        SubsystemId::Dialogue,
        &read_call,
        Some(read_token.token_id),
    );
    assert!(
        read_approved.success,
        "Read should succeed with token: {:?}",
        read_approved.output
    );
    assert_eq!(read_approved.output["content"], test_content);

    // 5. Path traversal attempt outside workspace root is blocked by sandbox policy
    for traversal in ["/etc/shadow", "../../../../etc/passwd"] {
        let bad_call = KernelToolCall {
            tool: "fs:read".into(),
            params: serde_json::json!({ "path": traversal }),
        };
        let bad_res = LocalKernelToolBroker::execute(&bad_call);
        assert!(!bad_res.success);
        assert_eq!(bad_res.output["error_code"], "outside_workspace");
    }
}
