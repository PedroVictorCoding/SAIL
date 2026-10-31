use std::sync::Arc;

use sail_common::types::SubsystemId;
use sail_guardian::{
    ApprovalBroker, AuditLedger, Capability, DecisionOutcome, ExecutionDenied,
    GuardianExecutionBroker, PolicyEngine, SandboxPolicy, TokenConsumptionError, ToolCapability,
};

#[test]
fn revoked_approval_cannot_execute_or_record_granted_effect() {
    for revoke_session in [false, true] {
        let approval = Arc::new(ApprovalBroker::new());
        let audit = Arc::new(AuditLedger::new());
        let broker = GuardianExecutionBroker::new(
            Arc::new(PolicyEngine::new()),
            Arc::clone(&audit),
            Arc::clone(&approval),
            Arc::new(SandboxPolicy::new()),
        );
        let capability = Capability::Tool(ToolCapability::KernelTool("vision:frame".into()));
        let session =
            approval.create_session(SubsystemId::Bridge, capability.clone(), "Capture one frame");
        let token = approval.resolve_session(session.session_id, true).unwrap();
        if revoke_session {
            approval.revoke_session(session.session_id);
        } else {
            approval.revoke_grant(token.token_id);
        }

        let mut executed = false;
        let result = broker.authorize_and_execute(
            SubsystemId::Bridge,
            capability,
            Some(token.token_id),
            None,
            || executed = true,
        );
        assert_eq!(
            result,
            Err(ExecutionDenied::InvalidToken(
                TokenConsumptionError::Revoked(token.token_id)
            ))
        );
        assert!(!executed);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit.list(1)[0].outcome, DecisionOutcome::Denied);
    }
}
