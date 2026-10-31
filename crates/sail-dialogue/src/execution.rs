//! Conversation adapter for the shared typed execution scheduler.
use async_trait::async_trait;
use sail_agents::tools::{Tool, ToolContext, ToolFailure, ToolRegistry, ToolScheduler};
use sail_guardian::{Capability, SystemCapability};
use sail_protocol::execution::{ToolDescriptor, ToolEffect};
use serde_json::{Value, json};
use std::sync::Arc;
struct Kernel;
#[async_trait]
impl Tool for Kernel {
    fn capability(&self, tool: &str, input: &Value) -> Capability {
        if tool == "os:status" {
            Capability::System(SystemCapability::OsTelemetry)
        } else {
            ToolScheduler::capability(tool, input)
        }
    }
    async fn execute(&self, _context: ToolContext, input: Value) -> Result<Value, ToolFailure> {
        // Registry fixes the tool name; callers cannot select a different adapter.
        let call: crate::KernelToolCall =
            serde_json::from_value(input).map_err(|e| ToolFailure {
                message: e.to_string(),
                retryable: false,
            })?;
        let result = crate::LocalKernelToolBroker::execute_scheduled(&call);
        if result.success {
            Ok(Value::String(result.output.to_string()))
        } else {
            Err(ToolFailure {
                message: result.output.to_string(),
                retryable: false,
            })
        }
    }
}
struct NamedKernel(&'static str);
#[async_trait]
impl Tool for NamedKernel {
    fn capability(&self, tool: &str, input: &Value) -> Capability {
        Kernel.capability(tool, input)
    }
    async fn execute(&self, context: ToolContext, input: Value) -> Result<Value, ToolFailure> {
        Kernel
            .execute(context, json!({"tool":self.0,"params":input}))
            .await
    }
}
struct Command;
#[async_trait]
impl Tool for Command {
    fn capability(&self, tool: &str, input: &Value) -> Capability {
        let mut reviewed = input.clone();
        reviewed["limits"] = json!(sail_protocol::execution::CommandLimits::default());
        ToolScheduler::capability(tool, &reviewed)
    }
    async fn execute(&self, context: ToolContext, input: Value) -> Result<Value, ToolFailure> {
        let socket = std::env::var_os("SAIL_RUNNER_SOCKET").ok_or_else(|| ToolFailure {
            message: "Isolated command runner is not configured".into(),
            retryable: false,
        })?;
        let uid: u32 = std::env::var("SAIL_RUNNER_UID")
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| ToolFailure {
                message: "Runner service UID is not configured".into(),
                retryable: false,
            })?;
        let plan = sail_protocol::execution::CommandPlan {
            version: 1,
            request_id: context.request_id,
            correlation_id: context.correlation_id,
            executable: input["executable"].as_str().unwrap_or_default().into(),
            argv: input["argv"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            limits: sail_protocol::execution::CommandLimits::default(),
        };
        plan.validate().map_err(|message| ToolFailure {
            message,
            retryable: false,
        })?;
        let result = tokio::task::spawn_blocking(move || {
            sail_runner::dispatch_authorized(
                std::path::Path::new(&socket),
                uid,
                &plan,
                context.cancel,
            )
        })
        .await
        .map_err(|e| ToolFailure {
            message: e.to_string(),
            retryable: false,
        })?
        .map_err(|e| ToolFailure {
            message: e.to_string(),
            retryable: false,
        })?;
        if result.status != sail_protocol::execution::CommandStatus::Completed {
            return Err(ToolFailure {
                message: serde_json::to_string(&result).unwrap_or_default(),
                retryable: false,
            });
        }
        Ok(Value::String(
            serde_json::to_string(&result).unwrap_or_default(),
        ))
    }
}
struct Canvas;
#[async_trait]
impl Tool for Canvas {
    async fn execute(&self, _context: ToolContext, input: Value) -> Result<Value, ToolFailure> {
        Ok(Value::String(input.to_string()))
    }
}
pub fn scheduler(broker: Arc<sail_guardian::GuardianExecutionBroker>) -> Arc<ToolScheduler> {
    let registry = Arc::new(ToolRegistry::default());
    sail_agents::tools::register_builtin_time(&registry).expect("time descriptor");
    for (id, properties) in [
        ("os:status", json!({})),
        (
            "proc:list",
            json!({"limit":{"type":"integer","minimum":1,"maximum":100}}),
        ),
        (
            "fs:query",
            json!({"path":{"type":"string","maxLength":4096}}),
        ),
    ] {
        registry.register(ToolDescriptor {
            id:id.into(),version:1,description:format!("Local {id}"),
            input_schema:json!({"type":"object","properties":properties,"additionalProperties":false}),
            output_schema:json!({"type":"string","maxLength":1048576}),
            security_effect:ToolEffect::ReadOnly,required_capabilities:vec![id.into()],
            timeout_ms:120000,resource_class:"io".into(),concurrency_limit:4,
        },Arc::new(NamedKernel(id))).expect("kernel descriptor");
    }
    for (id, properties, effect) in [
        (
            "fs:write",
            json!({"path":{"type":"string","maxLength":4096},"content":{"type":"string","maxLength":32700}}),
            ToolEffect::Mutating,
        ),
        (
            "fs:read",
            json!({"path":{"type":"string","maxLength":4096}}),
            ToolEffect::ReadOnly,
        ),
        (
            "forge:enqueue",
            json!({"issue_number":{"type":"integer","minimum":1},"repo":{"type":"string","maxLength":512},"task":{"type":"string","maxLength":12000},"body":{"type":"string","maxLength":12000}}),
            ToolEffect::Mutating,
        ),
        (
            "forge:cancel",
            json!({"job_id":{"type":"string","maxLength":128},"reason":{"type":"string","maxLength":2048}}),
            ToolEffect::Mutating,
        ),
        (
            "forge:status",
            json!({"job_id":{"type":"string","maxLength":128}}),
            ToolEffect::ReadOnly,
        ),
        ("forge:list", json!({}), ToolEffect::ReadOnly),
        (
            "github:issues",
            json!({"limit":{"type":"integer","minimum":1,"maximum":50}}),
            ToolEffect::ReadOnly,
        ),
    ] {
        registry.register(ToolDescriptor {
            id:id.into(),version:1,description:format!("Local {id}"),
            input_schema:json!({"type":"object","properties":properties,"additionalProperties":false}),
            output_schema:json!({"type":"string","maxLength":1048576}),security_effect:effect,
            required_capabilities:vec![id.into()],timeout_ms:120000,resource_class:"io".into(),concurrency_limit:2,
        },Arc::new(NamedKernel(id))).expect("Forge and file descriptor");
    }
    registry.register(ToolDescriptor {
        id:"canvas:publish".into(),version:1,description:"Present a diagram or structured information beside the conversation".into(),
        input_schema:json!({"type":"object","properties":{"artifact_type":{"type":"string","enum":["mermaid","json","svg"],"maxLength":16},"content":{"type":"string","maxLength":32700}},"required":["artifact_type","content"],"additionalProperties":false}),
        output_schema:json!({"type":"string","maxLength":65536}),security_effect:ToolEffect::ReadOnly,
        required_capabilities:vec!["canvas:publish".into()],timeout_ms:1000,resource_class:"telemetry".into(),concurrency_limit:4,
    },Arc::new(Canvas)).expect("canvas descriptor");
    registry.register(ToolDescriptor {
        id:"command:execute".into(),version:1,description:"Execute argv in the isolated no-network command service; requires owner approval".into(),
        input_schema:json!({"type":"object","properties":{"executable":{"type":"string","maxLength":4096},"argv":{"type":"array","maxItems":128,"items":{"type":"string","maxLength":8192}}},"required":["executable","argv"],"additionalProperties":false}),
        output_schema:json!({"type":"string","maxLength":1048576}),security_effect:ToolEffect::Sandbox,
        required_capabilities:vec!["command:execute".into()],timeout_ms:120000,resource_class:"sandbox".into(),concurrency_limit:1,
    },Arc::new(Command)).expect("command descriptor");
    Arc::new(ToolScheduler::new(registry, broker, 4))
}

pub fn shared() -> Result<Arc<ToolScheduler>, sail_guardian::ExecutionDenied> {
    static SCHEDULER: std::sync::OnceLock<Arc<ToolScheduler>> = std::sync::OnceLock::new();
    if let Some(scheduler) = SCHEDULER.get() {
        return Ok(scheduler.clone());
    }
    let instance = scheduler(sail_guardian::global_broker()?);
    Ok(SCHEDULER.get_or_init(|| instance).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use sail_common::types::SubsystemId;
    use sail_guardian::{
        ApprovalBroker, AuditLedger, GuardianExecutionBroker, PolicyEngine, SandboxPolicy,
    };
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use uuid::Uuid;
    #[tokio::test]
    async fn owner_http_approval_resumes_real_registered_tool_and_caches_its_result() {
        let broker = Arc::new(GuardianExecutionBroker::new(
            Arc::new(PolicyEngine::new()),
            Arc::new(AuditLedger::new()),
            Arc::new(ApprovalBroker::new()),
            Arc::new(SandboxPolicy::new()),
        ));
        let scheduler = scheduler(broker.clone());
        let mut events = scheduler.subscribe();
        let context = ToolContext {
            request_id: Uuid::new_v4(),
            correlation_id: Uuid::new_v4(),
            actor: SubsystemId::Dialogue,
            cancel: CancellationToken::new(),
        };
        let scheduled = scheduler.clone();
        let next = context.clone();
        let operation = tokio::spawn(async move {
            scheduled
                .execute(next, "proc:list", json!({"limit":1}), None, true)
                .await
        });
        let session = loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
                .await
                .unwrap()
                .unwrap();
            if let Some(session) = event.approval_session {
                assert_eq!(event.request_id, context.request_id);
                break session;
            }
        };
        assert!(!operation.is_finished());
        let secret = "test-owner-token-with-more-than-32-bytes";
        let app = crate::approval_http::router(
            Some(sail_guardian::authority::ApprovalAuthority::new(secret).unwrap()),
            Some(broker.clone()),
        );
        let body = json!({"session_id":session,"approved":true}).to_string();
        let denied = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/resolve")
                    .header("content-type", "application/json")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        assert!(!operation.is_finished());
        let accepted = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/resolve")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {secret}"))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);
        let observed = tokio::time::timeout(std::time::Duration::from_secs(2), operation)
            .await
            .unwrap()
            .unwrap();
        assert!(observed.success, "{:?}", observed.error);
        let data: Value = serde_json::from_str(observed.data.as_str().unwrap()).unwrap();
        assert!(data["count"].as_u64().unwrap() <= 1);
        let replay = scheduler
            .execute(context, "proc:list", json!({"limit":1}), None, true)
            .await;
        assert_eq!(replay.id, observed.id);
        assert_eq!(
            broker
                .audit()
                .list(100)
                .iter()
                .filter(|a| a.outcome == sail_guardian::DecisionOutcome::Granted)
                .count(),
            1
        );
    }
    #[tokio::test]
    async fn actual_clock_and_canvas_use_registered_bounded_tools() {
        let broker = Arc::new(GuardianExecutionBroker::new(
            Arc::new(PolicyEngine::new()),
            Arc::new(AuditLedger::new()),
            Arc::new(ApprovalBroker::new()),
            Arc::new(SandboxPolicy::new()),
        ));
        let scheduler = scheduler(broker);
        let context = ToolContext {
            request_id: Uuid::new_v4(),
            correlation_id: Uuid::new_v4(),
            actor: SubsystemId::Dialogue,
            cancel: CancellationToken::new(),
        };
        let clock = scheduler
            .execute(context.clone(), "local:time", json!({}), None, true)
            .await;
        assert!(clock.success);
        assert!(clock.data["unix_ms"].as_u64().unwrap() > 0);
        let context = ToolContext {
            request_id: Uuid::new_v4(),
            ..context
        };
        let canvas = scheduler
            .execute(
                context,
                "canvas:publish",
                json!({"artifact_type":"mermaid","content":"graph LR; A-->B"}),
                None,
                true,
            )
            .await;
        assert!(canvas.success);
    }
}
