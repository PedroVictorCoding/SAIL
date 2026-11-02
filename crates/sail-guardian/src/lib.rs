pub mod approval;
pub mod audit;
pub mod authority;
pub mod broker;
pub mod capability;
pub mod confined;
pub mod credentials;
pub mod memory;
pub mod policy;
pub mod runtime;
pub mod safety_review;
pub mod sandbox;

use async_trait::async_trait;
use std::sync::Arc;
use tracing::{info, warn};

pub use approval::{
    ApprovalBroker, ApprovalError, ApprovalSession, CapabilityToken, TokenConsumptionError,
};
pub use audit::{AuditIntegrityError, AuditLedger, AuditReceipt, DecisionOutcome, GENESIS_HASH};
pub use broker::{
    ExecutionDenied, GuardianExecutionBroker, clear_global_broker, global_broker,
    register_global_broker,
};
pub use capability::{
    Capability, DeviceCapability, FsCapability, RiskLevel, SystemCapability, ToolCapability,
};
pub use credentials::{
    CredentialError, CredentialHandle, CredentialScope, CredentialStore, ScopedResource,
    SecretString, VaultStatus,
};
pub use policy::{AuthorizationDecision, LearnedApprovalRule, LearnedPattern, PolicyEngine};
pub use runtime::{
    ActionRequest, AuthorizedAction, AutonomyPreset, CapabilityProfile, ExecutionHandle,
    NonoSecurityRuntime, SecurityCapabilities, SecurityError, SecurityRuntime, SecurityStatus,
    is_sensitive_host_path,
};
pub use safety_review::{
    FindingStatus, RecommendedAction, SafetyFinding, SafetyFindingSeverity, SafetyReviewEngine,
    SafetyReviewRunSummary,
};
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
        let credentials = Arc::new(match std::env::var_os("SAIL_VAULT_DIR") {
            Some(directory) => CredentialStore::open_persistent(std::path::Path::new(&directory))
                .expect("configured credential vault unavailable; refusing insecure fallback"),
            None => CredentialStore::new(),
        });
        let redaction_store = credentials.clone();
        sail_common::diagnostics::register_redactor(Arc::new(move |text| {
            redaction_store.redact_text(text)
        }));
        let sandbox = Arc::new(SandboxPolicy::new());
        // Device consent is established explicitly by a trusted session, never at boot.

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

    pub fn safety_review(&self) -> Arc<safety_review::SafetyReviewEngine> {
        self.broker.safety_review().clone()
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
        info!(
            "SAIL Guardian initialized (access control, capability grants, sandbox policy & tamper-evident audit ledger)"
        );
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
                    "tool:os:status" => {
                        Capability::Tool(ToolCapability::KernelTool("os:status".into()))
                    }
                    "tool:proc:list" => {
                        Capability::Tool(ToolCapability::KernelTool("proc:list".into()))
                    }
                    "tool:fs:query" => {
                        Capability::Tool(ToolCapability::KernelTool("fs:query".into()))
                    }
                    "tool:vision:frame" => {
                        Capability::Tool(ToolCapability::KernelTool("vision:frame".into()))
                    }
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
                    AuthorizationDecision::RequiresUserApproval {
                        session_id,
                        rationale,
                    } => {
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
            EventPayload::UserApprovalResolved { .. } => {
                // EventBus source IDs are not authenticated identities. Only a trusted
                // owner transport may resolve ApprovalBroker sessions directly.
                warn!("Ignored unauthenticated approval event");
                self.audit.record_with_correlation(event.source,Capability::Tool(ToolCapability::KernelTool("approval:resolve".into())),DecisionOutcome::Denied,"Unauthorized approval resolution attempt: event source is not an authenticated owner",event.correlation_id);
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
