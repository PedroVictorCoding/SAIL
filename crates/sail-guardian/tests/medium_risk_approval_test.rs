use std::sync::Arc;

use sail_common::types::SubsystemId;
use sail_guardian::{
    ApprovalBroker, AuditLedger, AuthorizationDecision, Capability, DecisionOutcome,
    DeviceCapability, ExecutionDenied, FsCapability, GuardianExecutionBroker, PolicyEngine,
    SandboxPolicy, SystemCapability, TokenConsumptionError, ToolCapability,
};

const CALLERS: [SubsystemId; 10] = [
    SubsystemId::Core,
    SubsystemId::Dialogue,
    SubsystemId::Arena,
    SubsystemId::Canvas,
    SubsystemId::Flow,
    SubsystemId::Discovery,
    SubsystemId::Agents,
    SubsystemId::Mind,
    SubsystemId::Bridge,
    SubsystemId::Guardian,
];

fn medium_capabilities() -> Vec<Capability> {
    vec![
        Capability::Tool(ToolCapability::KernelTool("proc:list".into())),
        Capability::Tool(ToolCapability::KernelTool("fs:query".into())),
        Capability::System(SystemCapability::ProcessInspect),
        Capability::Device(DeviceCapability::Microphone),
        Capability::Filesystem(FsCapability::Read {
            path: "/proc/loadavg".into(),
        }),
    ]
}

struct Fixture {
    policy: Arc<PolicyEngine>,
    audit: Arc<AuditLedger>,
    approval: Arc<ApprovalBroker>,
    broker: GuardianExecutionBroker,
}

impl Fixture {
    fn new() -> Self {
        let policy = Arc::new(PolicyEngine::new());
        let audit = Arc::new(AuditLedger::new());
        let approval = Arc::new(ApprovalBroker::new());
        let broker = GuardianExecutionBroker::new(
            Arc::clone(&policy),
            Arc::clone(&audit),
            Arc::clone(&approval),
            Arc::new(SandboxPolicy::new()),
        );
        Self {
            policy,
            audit,
            approval,
            broker,
        }
    }
}

#[test]
fn no_subsystem_identity_implies_medium_risk_consent() {
    for caller in CALLERS {
        for capability in medium_capabilities() {
            let fixture = Fixture::new();
            let decision = fixture.policy.evaluate(
                caller,
                &capability,
                "Interactive request",
                &fixture.audit,
                &fixture.approval,
            );
            let AuthorizationDecision::RequiresUserApproval { session_id, .. } = decision else {
                panic!("Expected approval for {caller:?}: {capability:?}, got {decision:?}");
            };
            let pending = fixture.approval.list_pending();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].session_id, session_id);
            assert_eq!(pending[0].requester, caller);
            assert_eq!(pending[0].capability, capability);
            assert_eq!(fixture.audit.len(), 0, "Prompting must not record a grant");
        }
    }
}

#[test]
fn missing_token_never_executes_medium_risk_operation() {
    for caller in CALLERS {
        for capability in medium_capabilities() {
            let fixture = Fixture::new();
            let mut executed = false;
            let result = fixture.broker.authorize_and_execute(
                caller,
                capability.clone(),
                None,
                None,
                || executed = true,
            );
            if matches!(capability, Capability::Filesystem(_)) {
                assert_eq!(result, Err(ExecutionDenied::DescriptorRequired));
                assert!(!executed);
                assert_eq!(fixture.audit.list(1)[0].outcome, DecisionOutcome::Denied);
                continue;
            }
            let Err(ExecutionDenied::ApprovalRequired { session_id, .. }) = result else {
                panic!("Expected approval for {caller:?}: {capability:?}, got {result:?}");
            };
            assert!(!executed);
            let pending = fixture.approval.list_pending();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].session_id, session_id);
            assert_eq!(pending[0].requester, caller);
            assert_eq!(pending[0].capability, capability);
            assert_eq!(fixture.audit.len(), 1);
            assert_eq!(fixture.audit.list(1)[0].outcome, DecisionOutcome::Denied);
        }
    }
}

#[test]
fn approval_allows_one_exact_medium_risk_effect() {
    let fixture = Fixture::new();
    let capability = medium_capabilities().remove(0);
    let decision = fixture.policy.evaluate(
        SubsystemId::Bridge,
        &capability,
        "Inspect processes",
        &fixture.audit,
        &fixture.approval,
    );
    let AuthorizationDecision::RequiresUserApproval { session_id, .. } = decision else {
        panic!("Expected approval");
    };
    let token = fixture.approval.resolve_session(session_id, true).unwrap();
    let mut effects = 0;
    assert_eq!(
        fixture.broker.authorize_and_execute(
            SubsystemId::Dialogue,
            capability.clone(),
            Some(token.token_id),
            None,
            || effects += 1,
        ),
        Err(ExecutionDenied::InvalidToken(
            TokenConsumptionError::ActorMismatch {
                actual: SubsystemId::Dialogue,
                expected: SubsystemId::Bridge,
            }
        ))
    );
    assert_eq!(
        fixture.broker.authorize_and_execute(
            SubsystemId::Bridge,
            medium_capabilities().remove(1),
            Some(token.token_id),
            None,
            || effects += 1,
        ),
        Err(ExecutionDenied::InvalidToken(
            TokenConsumptionError::CapabilityMismatch
        ))
    );
    assert!(
        fixture
            .broker
            .authorize_and_execute(
                SubsystemId::Bridge,
                capability.clone(),
                Some(token.token_id),
                None,
                || effects += 1,
            )
            .is_ok()
    );
    assert_eq!(
        fixture.broker.authorize_and_execute(
            SubsystemId::Bridge,
            capability,
            Some(token.token_id),
            None,
            || effects += 1,
        ),
        Err(ExecutionDenied::InvalidToken(
            TokenConsumptionError::TokenReplayed(token.token_id)
        ))
    );
    assert_eq!(effects, 1);
    assert_eq!(fixture.audit.len(), 4);
    assert_eq!(
        fixture
            .audit
            .list(4)
            .iter()
            .filter(|r| r.outcome == DecisionOutcome::Granted)
            .count(),
        1
    );
}

#[test]
fn explicit_grants_remain_actor_scoped_and_revocable() {
    let fixture = Fixture::new();
    let capability = medium_capabilities().remove(0);
    fixture
        .policy
        .grant_capability(SubsystemId::Bridge, capability.clone());
    assert_eq!(
        fixture.policy.evaluate(
            SubsystemId::Bridge,
            &capability,
            "Granted",
            &fixture.audit,
            &fixture.approval,
        ),
        AuthorizationDecision::Allowed
    );
    let mut effects = 0;
    assert!(
        fixture
            .broker
            .authorize_and_execute(SubsystemId::Bridge, capability.clone(), None, None, || {
                effects += 1
            },)
            .is_ok()
    );
    assert!(matches!(
        fixture.broker.authorize_and_execute(
            SubsystemId::Dialogue,
            capability.clone(),
            None,
            None,
            || effects += 1,
        ),
        Err(ExecutionDenied::ApprovalRequired { .. })
    ));
    fixture
        .policy
        .revoke_capability(SubsystemId::Bridge, &capability);
    assert!(matches!(
        fixture.policy.evaluate(
            SubsystemId::Bridge,
            &capability,
            "Revoked",
            &fixture.audit,
            &fixture.approval,
        ),
        AuthorizationDecision::RequiresUserApproval { .. }
    ));
    assert!(matches!(
        fixture
            .broker
            .authorize_and_execute(SubsystemId::Bridge, capability, None, None, || effects += 1,),
        Err(ExecutionDenied::ApprovalRequired { .. })
    ));
    assert_eq!(effects, 1);
}

#[test]
fn low_risk_telemetry_still_executes_without_approval() {
    let fixture = Fixture::new();
    let capability = Capability::System(SystemCapability::OsTelemetry);
    assert_eq!(
        fixture.policy.evaluate(
            SubsystemId::Bridge,
            &capability,
            "Telemetry",
            &fixture.audit,
            &fixture.approval,
        ),
        AuthorizationDecision::Allowed
    );
    assert_eq!(
        fixture
            .broker
            .authorize_and_execute(SubsystemId::Bridge, capability, None, None, || 42),
        Ok(42)
    );
    assert!(fixture.approval.list_pending().is_empty());
}
