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

#[test]
fn test_github_issues_priority_sorting() {
    let _fixture = GUARDIAN_FIXTURE.lock().unwrap();
    let _guardian = GuardianSubsystem::new();
    // 1. Verify priority extraction logic
    let cases = [
        (
            "[P0 · SECURITY/BILLING] #92 Make OpenAI review quotas persistent",
            0,
            "P0",
        ),
        ("[P0 · FORGE/CONTINUITY] #92 Wire actual queue", 0, "P0"),
        ("[P1 · HEALTH/ANDROID] SAIL Noop", 1, "P1"),
        ("[Guardian · P1] Durable tamper-evident audit", 1, "P1"),
        ("[P2 · LIFE] SAIL Constraint Solver", 2, "P2"),
        ("[P3 · LIFE/POSSESSIONS] Possessions", 3, "P3"),
        ("[P4] Lab experiments", 4, "P4"),
        ("No priority in title", 99, "P?"),
    ];

    for (title, expected_rank, expected_label) in cases {
        let (rank, label) = LocalKernelToolBroker::extract_issue_priority(title, &[]);
        assert_eq!(rank, expected_rank, "Failed rank for: {title}");
        assert_eq!(label, expected_label, "Failed label for: {title}");
    }

    // Verify label fallback
    let critical_label = vec![serde_json::json!({"name": "critical"})];
    let (rank, label) =
        LocalKernelToolBroker::extract_issue_priority("Generic bug", &critical_label);
    assert_eq!(rank, 0);
    assert_eq!(label, "P0");

    let p2_label = vec![serde_json::json!({"name": "priority: p2"})];
    let (rank, label) = LocalKernelToolBroker::extract_issue_priority("Feature request", &p2_label);
    assert_eq!(rank, 2);
    assert_eq!(label, "P2");

    // 2. Test execution of github:issues (low risk read-only inspection auto-approves in Balanced mode)
    let issues_call = KernelToolCall {
        tool: "github:issues".into(),
        params: serde_json::json!({ "limit": 10 }),
    };
    let broker = sail_guardian::global_broker().unwrap();
    broker.policy().clear_learned_rules();
    let res = LocalKernelToolBroker::execute(&issues_call);

    // If gh CLI is installed and succeeds, verify priority sort invariants
    if res.success {
        assert_eq!(res.output["sorted_by"], "priority");
        let issues = res.output["issues"].as_array().expect("issues array");
        let mut prev_rank = 0u64;
        for iss in issues {
            let rank = iss["priority_rank"].as_u64().unwrap_or(99);
            assert!(
                rank >= prev_rank,
                "Issues should be sorted in ascending priority rank (P0 <= P1 <= P2...): got {} after {}",
                rank,
                prev_rank
            );
            prev_rank = rank;
        }
    }
}

#[test]
fn test_github_issue_specific_view_by_url_and_number() {
    let _fixture = GUARDIAN_FIXTURE.lock().unwrap();
    let _guardian = GuardianSubsystem::new();
    let broker = sail_guardian::global_broker().unwrap();
    broker.policy().clear_learned_rules();

    // 1. Fetch by URL directly (auto-approves as low-risk read-only assessment)
    let url_call = KernelToolCall {
        tool: "github:issues".into(),
        params: serde_json::json!({ "url": "https://github.com/PedroVictorCoding/SAIL/issues/125" }),
    };
    let res_url = LocalKernelToolBroker::execute(&url_call);
    if res_url.success {
        assert_eq!(res_url.output["found"], true);
        assert_eq!(res_url.output["number"], 125);
        assert!(
            res_url.output["title"]
                .as_str()
                .unwrap()
                .contains("Add concise cargo test failure summaries")
        );
        assert!(
            res_url.output["body"]
                .as_str()
                .unwrap()
                .contains("summarize_test_failure")
        );
    }

    // 2. Fetch by number
    let num_call = KernelToolCall {
        tool: "github:issues".into(),
        params: serde_json::json!({ "issue_number": 125 }),
    };
    let res_num = LocalKernelToolBroker::execute(&num_call);
    if res_num.success {
        assert_eq!(res_num.output["found"], true);
        assert_eq!(res_num.output["number"], 125);
    }
}

#[test]
fn test_web_read_read_only_internet_access() {
    let _fixture = GUARDIAN_FIXTURE.lock().unwrap();
    let _guardian = GuardianSubsystem::new();

    let web_call = KernelToolCall {
        tool: "web:read".into(),
        params: serde_json::json!({ "url": "https://httpbin.org/get" }),
    };

    let res = LocalKernelToolBroker::execute(&web_call);
    if res.success {
        assert_eq!(res.output["url"], "https://httpbin.org/get");
        assert!(res.output["bytes"].as_u64().unwrap() > 0);
    }

    // Ensure loopback addresses are restricted
    let loopback_call = KernelToolCall {
        tool: "web:read".into(),
        params: serde_json::json!({ "url": "http://127.0.0.1:8088/secret" }),
    };
    let loopback_res = LocalKernelToolBroker::execute(&loopback_call);
    assert!(!loopback_res.success);
}

#[test]
fn test_proc_list_learned_approval() {
    let _fixture = GUARDIAN_FIXTURE.lock().unwrap();
    let _guardian = GuardianSubsystem::new();
    let broker = sail_guardian::global_broker().unwrap();
    broker.policy().clear_learned_rules();

    let proc_call = KernelToolCall {
        tool: "proc:list".into(),
        params: serde_json::json!({ "limit": 5 }),
    };

    // First attempt: Medium risk requires approval in Balanced mode
    let denied = LocalKernelToolBroker::execute(&proc_call);
    assert!(!denied.success);
    assert_eq!(denied.output["approval_required"], true);

    // Learn approval rule for proc:list
    let cap = LocalKernelToolBroker::tool_to_capability(&proc_call);
    let rule = broker.policy().learn_approval(
        SubsystemId::Dialogue,
        &cap,
        "Always allow listing processes".into(),
    );

    // Second attempt: auto-approves via learned rule
    let res = LocalKernelToolBroker::execute(&proc_call);
    assert!(res.success);
    assert!(res.output["processes"].as_array().is_some());

    broker.policy().revoke_learned_rule(rule.id);
}
