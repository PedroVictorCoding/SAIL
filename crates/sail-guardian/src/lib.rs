pub mod approval;
pub mod audit;
pub mod broker;
pub mod capability;
pub mod credentials;
pub mod policy;
pub mod sandbox;

use std::sync::Arc;
use async_trait::async_trait;
use tracing::{info, warn};

pub use approval::{ApprovalBroker, ApprovalError, ApprovalSession, CapabilityToken, TokenConsumptionError};
pub use audit::{AuditIntegrityError, AuditLedger, AuditReceipt, DecisionOutcome, GENESIS_HASH};
pub use broker::{clear_global_broker, global_broker, register_global_broker, ExecutionDenied, GuardianExecutionBroker};
pub use capability::{Capability, DeviceCapability, FsCapability, RiskLevel, SystemCapability, ToolCapability};
pub use credentials::{CredentialError, CredentialHandle, CredentialStore};
pub use policy::{AuthorizationDecision, PolicyEngine};
pub use sandbox::{SandboxPolicy, SandboxViolation};

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

pub struct GuardianSubsystem {
    bus: Option<EventBus>,
    policy: Arc<PolicyEngine>,
    audit: Arc<AuditLedger>,
    approval: Arc<ApprovalBroker>,
    credentials: Arc<CredentialStore>,
    sandbox: Arc<SandboxPolicy>,
    broker: Arc<GuardianExecutionBroker>,
}

impl GuardianSubsystem {
    pub fn new() -> Self {
        let policy = Arc::new(PolicyEngine::new());
        let audit = Arc::new(AuditLedger::new());
        let approval = Arc::new(ApprovalBroker::new());
        let credentials = Arc::new(CredentialStore::new());
        let sandbox = Arc::new(SandboxPolicy::new());
        // Default interactive session camera consent for Dialogue
        policy.grant_capability(SubsystemId::Dialogue, Capability::Device(DeviceCapability::Camera));

        let broker = Arc::new(GuardianExecutionBroker::new(
            policy.clone(),
            audit.clone(),
            approval.clone(),
            sandbox.clone(),
        ));

        register_global_broker(broker.clone());

        Self {
            bus: None,
            policy,
            audit,
            approval,
            credentials,
            sandbox,
            broker,
        }
    }

    pub fn policy(&self) -> Arc<PolicyEngine> {
        self.policy.clone()
    }

    pub fn audit(&self) -> Arc<AuditLedger> {
        self.audit.clone()
    }

    pub fn approval(&self) -> Arc<ApprovalBroker> {
        self.approval.clone()
    }

    pub fn credentials(&self) -> Arc<CredentialStore> {
        self.credentials.clone()
    }

    pub fn sandbox(&self) -> Arc<SandboxPolicy> {
        self.sandbox.clone()
    }

    pub fn broker(&self) -> Arc<GuardianExecutionBroker> {
        self.broker.clone()
    }

    /// Direct evaluation of a capability request
    pub fn evaluate_request(
        &self,
        requester: SubsystemId,
        capability: &Capability,
        justification: &str,
    ) -> AuthorizationDecision {
        self.policy.evaluate(
            requester,
            capability,
            justification,
            &self.audit,
            &self.approval,
        )
    }
}

impl Default for GuardianSubsystem {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Subsystem for GuardianSubsystem {
    fn id(&self) -> SubsystemId {
        SubsystemId::Guardian
    }

    async fn init(&mut self, bus: &EventBus) -> Result<()> {
        self.bus = Some(bus.clone());
        info!("SAIL Guardian initialized (access control, capability grants, sandbox policy & tamper-evident audit ledger)");
        Ok(())
    }

    async fn start(&mut self) -> Result<()> {
        info!("SAIL Guardian access control subsystem active");
        if let Some(bus) = &self.bus {
            let event = SailEvent::new(
                self.id(),
                EventPayload::SystemReady {
                    subsystem: self.id(),
                },
            );
            let _ = bus.publish(event);
        }
        Ok(())
    }

    async fn handle_event(&self, event: &SailEvent) -> Result<()> {
        let bus = match &self.bus {
            Some(b) => b,
            None => return Ok(()),
        };

        match &event.payload {
            EventPayload::AccessEvaluationRequested {
                request_id,
                requester,
                capability: cap_str,
                resource,
                justification,
            } => {
                info!(
                    request_id = %request_id,
                    requester = %requester,
                    capability = %cap_str,
                    resource = %resource,
                    "SAIL Guardian evaluating access request"
                );

                // Map string to capability enum
                let cap = match cap_str.as_str() {
                    "tool:os:status" => Capability::Tool(ToolCapability::KernelTool("os:status".into())),
                    "tool:proc:list" => Capability::Tool(ToolCapability::KernelTool("proc:list".into())),
                    "tool:fs:query" => Capability::Tool(ToolCapability::KernelTool("fs:query".into())),
                    "tool:vision:frame" => Capability::Tool(ToolCapability::KernelTool("vision:frame".into())),
                    "device:camera" => Capability::Device(DeviceCapability::Camera),
                    "device:mic" => Capability::Device(DeviceCapability::Microphone),
                    _ => Capability::Tool(ToolCapability::CliCommand(cap_str.clone())),
                };

                let decision = self.policy.evaluate(
                    *requester,
                    &cap,
                    justification,
                    &self.audit,
                    &self.approval,
                );

                match decision {
                    AuthorizationDecision::Allowed => {
                        let granted = SailEvent::new(
                            self.id(),
                            EventPayload::AccessDecisionGranted {
                                request_id: request_id.clone(),
                                capability: cap_str.clone(),
                                resource: resource.clone(),
                            },
                        )
                        .with_target(*requester);
                        let _ = bus.publish(granted);
                    }
                    AuthorizationDecision::Denied { reason } => {
                        warn!(request_id = %request_id, reason = %reason, "Access denied by Guardian");
                        let denied = SailEvent::new(
                            self.id(),
                            EventPayload::AccessDecisionDenied {
                                request_id: request_id.clone(),
                                capability: cap_str.clone(),
                                resource: resource.clone(),
                                reason,
                            },
                        )
                        .with_target(*requester);
                        let _ = bus.publish(denied);
                    }
                    AuthorizationDecision::RequiresUserApproval { session_id, rationale } => {
                        info!(session_id = %session_id, rationale = %rationale, "User approval prompted");
                        let prompt = SailEvent::new(
                            self.id(),
                            EventPayload::UserApprovalPrompted {
                                request_id: session_id.to_string(),
                                risk_level: format!("{:?}", cap.risk_level()).to_lowercase(),
                                description: rationale,
                            },
                        );
                        let _ = bus.publish(prompt);
                    }
                }
            }
            EventPayload::UserApprovalResolved { request_id, approved } => {
                if let Ok(session_uuid) = uuid::Uuid::parse_str(request_id) {
                    match self.approval.resolve_session(session_uuid, *approved) {
                        Ok(token) => {
                            info!(session_id = %session_uuid, token_id = %token.token_id, "User approval granted, issued capability token");
                            let receipt = self.audit.record(
                                token.bound_actor,
                                token.bound_capability.clone(),
                                DecisionOutcome::Granted,
                                format!("User interactive approval granted token {}", token.token_id),
                            );

                            let receipt_event = SailEvent::new(
                                self.id(),
                                EventPayload::AuditReceiptCreated {
                                    receipt_id: receipt.receipt_id.to_string(),
                                    hash: receipt.hash,
                                    action: format!("{:?}", token.bound_capability),
                                    decision: "granted".into(),
                                },
                            );
                            let _ = bus.publish(receipt_event);
                        }
                        Err(err) => {
                            warn!(session_id = %session_uuid, error = %err, "User approval failed or denied");
                            let receipt_event = SailEvent::new(
                                self.id(),
                                EventPayload::AuditReceiptCreated {
                                    receipt_id: uuid::Uuid::new_v4().to_string(),
                                    hash: String::new(),
                                    action: format!("session_{}", session_uuid),
                                    decision: "denied".into(),
                                },
                            );
                            let _ = bus.publish(receipt_event);
                        }
                    }
                }
            }
            _ => {}
        }

        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        info!("SAIL Guardian subsystem shutting down");
        Ok(())
    }
}

