use async_trait::async_trait;
use sail_agents::tools::*;
use sail_common::types::SubsystemId;
use sail_guardian::{
    ApprovalBroker, AuditLedger, GuardianExecutionBroker, PolicyEngine, SandboxPolicy,
};
use sail_protocol::execution::{ToolDescriptor, ToolEffect, ToolState, ToolStep};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
struct Fake {
    barrier: Option<Arc<tokio::sync::Barrier>>,
    calls: AtomicUsize,
    fail: bool,
    delay: u64,
}
#[async_trait]
impl Tool for Fake {
    async fn execute(&self, _context: ToolContext, _input: Value) -> Result<Value, ToolFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(barrier) = &self.barrier {
            barrier.wait().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(self.delay)).await;
        if self.fail {
            Err(ToolFailure {
                message: "backend failed".into(),
                retryable: true,
            })
        } else {
            Ok(json!({"value":"ok"}))
        }
    }
}
fn descriptor(id: &str, effect: ToolEffect, timeout: u64) -> ToolDescriptor {
    ToolDescriptor {
        id: id.into(),
        version: 1,
        description: format!("Public weather lookup {id}"),
        input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
        output_schema: json!({"type":"object","properties":{"value":{"type":"string","maxLength":65536}},"required":["value"],"additionalProperties":false}),
        security_effect: effect,
        required_capabilities: vec![id.into()],
        timeout_ms: timeout,
        resource_class: "io".into(),
        concurrency_limit: 2,
    }
}
fn context() -> ToolContext {
    ToolContext {
        request_id: Uuid::new_v4(),
        correlation_id: Uuid::from_u128(1),
        actor: SubsystemId::Agents,
        cancel: CancellationToken::new(),
    }
}
fn scheduler(registry: Arc<ToolRegistry>, policy: Arc<PolicyEngine>) -> Arc<ToolScheduler> {
    Arc::new(ToolScheduler::new(
        registry,
        Arc::new(GuardianExecutionBroker::new(
            policy,
            Arc::new(AuditLedger::new()),
            Arc::new(ApprovalBroker::new()),
            Arc::new(SandboxPolicy::new()),
        )),
        4,
    ))
}
fn fake(barrier: Option<Arc<tokio::sync::Barrier>>, fail: bool, delay: u64) -> Arc<Fake> {
    Arc::new(Fake {
        barrier,
        calls: AtomicUsize::new(0),
        fail,
        delay,
    })
}
#[tokio::test]
async fn independent_tools_overlap_and_events_are_correlated_and_sequenced() {
    let registry = Arc::new(ToolRegistry::default());
    let policy = Arc::new(PolicyEngine::new());
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    for id in ["web:first", "web:second"] {
        registry
            .register(
                descriptor(id, ToolEffect::ReadOnly, 1000),
                fake(Some(barrier.clone()), false, 0),
            )
            .unwrap();
        policy.grant_scoped(
            SubsystemId::Agents,
            ToolScheduler::capability(id, &json!({})),
            Some(Uuid::from_u128(1)),
            chrono::Duration::seconds(60),
        );
    }
    let scheduler = scheduler(registry, policy);
    let mut events = scheduler.subscribe();
    let first = context();
    let second = ToolContext {
        request_id: Uuid::new_v4(),
        ..first.clone()
    };
    let (left, right) = tokio::join!(
        scheduler.execute(first.clone(), "web:first", json!({}), None, true),
        scheduler.execute(second, "web:second", json!({}), None, true)
    );
    assert!(left.success && right.success);
    assert_ne!(left.id, right.id);
    assert_eq!(left.correlation_id, right.correlation_id);
    let mut sequences = std::collections::HashMap::new();
    while let Ok(event) = events.try_recv() {
        assert_eq!(event.correlation_id, first.correlation_id);
        let seq = sequences.entry(event.request_id).or_insert(0);
        assert_eq!(event.sequence, *seq + 1);
        *seq = event.sequence;
    }
    assert_eq!(sequences.len(), 2);
    assert!(sequences.values().all(|seq| *seq == 4));
}
#[test]
fn relevant_schema_selection_stays_bounded_with_thirty_tools() {
    let registry = ToolRegistry::default();
    for index in 0..35 {
        registry
            .register(
                descriptor(&format!("weather:{index}"), ToolEffect::ReadOnly, 1000),
                fake(None, false, 0),
            )
            .unwrap();
    }
    let subset = registry.select("weather", 3, 4096);
    assert_eq!(subset.len(), 3);
    assert!(serde_json::to_vec(&subset).unwrap().len() < 4200);
    let mut unsupported = descriptor("invalid", ToolEffect::ReadOnly, 1000);
    unsupported.input_schema["$ref"] = json!("https://untrusted/schema");
    assert!(
        registry
            .register(unsupported, fake(None, false, 0))
            .is_err()
    );
}
#[tokio::test]
async fn policy_schema_cancellation_and_deadline_fail_before_unapproved_effects() {
    let registry = Arc::new(ToolRegistry::default());
    let backend = fake(None, false, 100);
    registry
        .register(
            descriptor("test:run", ToolEffect::ReadOnly, 20),
            backend.clone(),
        )
        .unwrap();
    let policy = Arc::new(PolicyEngine::new());
    let scheduler = scheduler(registry, policy.clone());
    let denied = scheduler
        .execute(context(), "test:run", json!({}), None, true)
        .await;
    assert!(!denied.success);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    let invalid = scheduler
        .execute(context(), "test:run", json!({"extra":"no"}), None, true)
        .await;
    assert!(!invalid.success);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    policy.grant_scoped(
        SubsystemId::Agents,
        ToolScheduler::capability("test:run", &json!({})),
        Some(Uuid::from_u128(1)),
        chrono::Duration::seconds(60),
    );
    let timeout = scheduler
        .execute(context(), "test:run", json!({}), None, true)
        .await;
    assert_eq!(timeout.error.as_deref(), Some("deadline_exceeded"));
    let cancelled = context();
    cancelled.cancel.cancel();
    let mut events = scheduler.subscribe();
    let result = scheduler
        .execute(cancelled, "test:run", json!({}), None, true)
        .await;
    assert_eq!(result.error.as_deref(), Some("cancelled"));
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| event.state == ToolState::Cancelled)
    );
}
#[tokio::test]
async fn mutations_are_not_retried_and_duplicate_ids_never_repeat_effects() {
    let registry = Arc::new(ToolRegistry::default());
    let backend = fake(None, true, 0);
    registry
        .register(
            descriptor("test:write", ToolEffect::Mutating, 1000),
            backend.clone(),
        )
        .unwrap();
    let policy = Arc::new(PolicyEngine::new());
    policy.grant_scoped(
        SubsystemId::Agents,
        ToolScheduler::capability("test:write", &json!({})),
        Some(Uuid::from_u128(1)),
        chrono::Duration::seconds(60),
    );
    let scheduler = scheduler(registry, policy);
    let ctx = context();
    assert!(
        !scheduler
            .execute(ctx.clone(), "test:write", json!({}), None, true)
            .await
            .success
    );
    assert!(
        !scheduler
            .execute(ctx, "test:write", json!({}), None, true)
            .await
            .success
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn dependency_failure_skips_dependents_and_cycles_are_rejected_without_dispatch() {
    let registry = Arc::new(ToolRegistry::default());
    let backend = fake(None, true, 0);
    registry
        .register(
            descriptor("test:write", ToolEffect::Mutating, 1000),
            backend.clone(),
        )
        .unwrap();
    let policy = Arc::new(PolicyEngine::new());
    policy.grant_scoped(
        SubsystemId::Agents,
        ToolScheduler::capability("test:write", &json!({})),
        Some(Uuid::from_u128(1)),
        chrono::Duration::seconds(60),
    );
    let scheduler = scheduler(registry, policy);
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let step = |id, deps| ToolStep {
        request_id: id,
        tool: "test:write".into(),
        input: json!({}),
        depends_on: deps,
        approval_token: None,
        foreground: true,
    };
    assert!(
        scheduler
            .clone()
            .execute_plan(context(), vec![step(a, vec![b]), step(b, vec![a])])
            .await
            .is_err()
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    let result = scheduler
        .execute_plan(context(), vec![step(a, vec![]), step(b, vec![a])])
        .await
        .unwrap();
    assert_eq!(result[1].error.as_deref(), Some("dependency_failed"));
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
}
struct Large;
#[async_trait]
impl Tool for Large {
    async fn execute(&self, _context: ToolContext, _input: Value) -> Result<Value, ToolFailure> {
        Ok(json!({"value":"evidence".repeat(1024)}))
    }
}
#[tokio::test]
async fn large_observations_are_bounded_and_scoped_to_actor_and_task() {
    let registry = Arc::new(ToolRegistry::default());
    registry
        .register(
            descriptor("test:large", ToolEffect::ReadOnly, 1000),
            Arc::new(Large),
        )
        .unwrap();
    let policy = Arc::new(PolicyEngine::new());
    let ctx = context();
    policy.grant_scoped(
        ctx.actor,
        ToolScheduler::capability("test:large", &json!({})),
        Some(ctx.correlation_id),
        chrono::Duration::seconds(60),
    );
    let scheduler = scheduler(registry, policy);
    let result = scheduler
        .execute(ctx.clone(), "test:large", json!({}), None, true)
        .await;
    assert!(result.success);
    let id = result.data["payload_ref"].as_str().unwrap();
    assert!(scheduler.payload(id, &ctx).is_some());
    assert!(
        scheduler
            .payload(
                id,
                &ToolContext {
                    actor: SubsystemId::Bridge,
                    ..ctx.clone()
                }
            )
            .is_none()
    );
    assert!(
        scheduler
            .payload(
                id,
                &ToolContext {
                    correlation_id: Uuid::new_v4(),
                    ..ctx
                }
            )
            .is_none()
    );
}
#[tokio::test]
async fn revoking_consumed_approval_cancels_an_inflight_backend() {
    let registry = Arc::new(ToolRegistry::default());
    let backend = fake(None, false, 5000);
    registry
        .register(
            descriptor("test:slow", ToolEffect::ReadOnly, 10000),
            backend.clone(),
        )
        .unwrap();
    let approval = Arc::new(ApprovalBroker::new());
    let ctx = context();
    let pending = approval.create_session_for(
        Some(ctx.correlation_id),
        ctx.actor,
        ToolScheduler::capability("test:slow", &json!({})),
        "slow lookup",
    );
    let token = approval.resolve_session(pending.session_id, true).unwrap();
    let guardian = Arc::new(GuardianExecutionBroker::new(
        Arc::new(PolicyEngine::new()),
        Arc::new(AuditLedger::new()),
        approval.clone(),
        Arc::new(SandboxPolicy::new()),
    ));
    let scheduler = Arc::new(ToolScheduler::new(registry, guardian, 4));
    let mut events = scheduler.subscribe();
    let execution = scheduler.execute(
        ctx.clone(),
        "test:slow",
        json!({}),
        Some(token.token_id),
        true,
    );
    let revocation = async {
        while let Ok(event) = events.recv().await {
            if event.state == ToolState::Executing {
                approval.revoke_session(pending.session_id);
                break;
            }
        }
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        tokio::join!(execution, revocation)
    })
    .await
    .unwrap();
    assert_eq!(result.error.as_deref(), Some("revoked"));
    assert!(!ctx.cancel.is_cancelled());
}
