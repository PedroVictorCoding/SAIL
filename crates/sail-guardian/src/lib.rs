pub mod approval;
pub mod audit;
pub mod capability;
pub mod credentials;
pub mod policy;
pub mod sandbox;

use std::sync::Arc;
use async_trait::async_trait;
use tracing::{info, warn};

pub use approval::{ApprovalBroker, ApprovalSession};
pub use audit::{AuditLedger, AuditReceipt, DecisionOutcome};
pub use capability::{Capability, DeviceCapability, FsCapability, RiskLevel, SystemCapability, ToolCapability};
pub use credentials::CredentialStore;
pub use policy::{AuthorizationDecision, PolicyEngine};
pub use sandbox::SandboxPolicy;

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
}

impl GuardianSubsystem {
    pub fn new() -> Self {
        Self {
            bus: None,
            policy: Arc::new(PolicyEngine::new()),
            audit: Arc::new(AuditLedger::new()),
            approval: Arc::new(ApprovalBroker::new()),
            credentials: Arc::new(CredentialStore::new()),
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
                    let resolved = self.approval.resolve_session(session_uuid, *approved);
                    if let Some(session) = resolved {
                        info!(session_id = %session_uuid, approved, "User approval resolved by Guardian");
                        let receipt = self.audit.record(
                            session.requester,
                            session.capability.clone(),
                            if *approved { DecisionOutcome::Granted } else { DecisionOutcome::Denied },
                            "User interactive approval resolution",
                        );

                        let receipt_event = SailEvent::new(
                            self.id(),
                            EventPayload::AuditReceiptCreated {
                                receipt_id: receipt.receipt_id.to_string(),
                                hash: receipt.hash,
                                action: format!("{:?}", session.capability),
                                decision: if *approved { "granted".into() } else { "denied".into() },
                            },
                        );
                        let _ = bus.publish(receipt_event);
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
