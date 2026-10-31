use sail_common::types::SubsystemId;
use sail_dialogue::{KernelToolCall, LocalKernelToolBroker};
use serde_json::json;
use uuid::Uuid;
#[test]
fn runtime_tool_retry_requires_exact_arguments_original_context_and_one_use() {
    let guardian = sail_guardian::GuardianSubsystem::new();
    let context = Uuid::new_v4();
    let call = KernelToolCall {
        tool: "proc:list".into(),
        params: json!({"limit":1}),
    };
    let denied = LocalKernelToolBroker::execute_guarded_for(
        SubsystemId::Dialogue,
        &call,
        None,
        Some(context),
    );
    assert!(!denied.success);
    assert_eq!(denied.output["approval_action"]["params"], call.params);
    let session = Uuid::parse_str(denied.output["approval_session"].as_str().unwrap()).unwrap();
    let token = guardian.approval().resolve_session(session, true).unwrap();
    let changed = KernelToolCall {
        tool: "proc:list".into(),
        params: json!({"limit":2}),
    };
    assert!(
        !LocalKernelToolBroker::execute_guarded_for(
            SubsystemId::Dialogue,
            &changed,
            Some(token.token_id),
            Some(context)
        )
        .success
    );
    assert!(
        !LocalKernelToolBroker::execute_guarded_for(
            SubsystemId::Dialogue,
            &call,
            Some(token.token_id),
            Some(Uuid::new_v4())
        )
        .success
    );
    let allowed = LocalKernelToolBroker::execute_guarded_for(
        SubsystemId::Dialogue,
        &call,
        Some(token.token_id),
        Some(context),
    );
    assert!(allowed.success);
    assert_eq!(allowed.output["count"], 1);
    assert!(
        !LocalKernelToolBroker::execute_guarded_for(
            SubsystemId::Dialogue,
            &call,
            Some(token.token_id),
            Some(context)
        )
        .success
    );
}
