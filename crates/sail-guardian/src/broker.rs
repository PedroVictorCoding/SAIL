use std::sync::{Arc, RwLock};
use uuid::Uuid;

use crate::approval::{ApprovalBroker, TokenConsumptionError};
use crate::audit::{AuditLedger, DecisionOutcome};
use crate::capability::{Capability, FsCapability, RiskLevel, ToolCapability};
use crate::policy::PolicyEngine;
use crate::sandbox::{SandboxPolicy, SandboxViolation};
use sail_common::types::SubsystemId;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExecutionDenied {
    #[error(
        "Filesystem operations require descriptor-bound access; host shell execution is unavailable"
    )]
    DescriptorRequired,
    #[error("Guardian execution broker is uninitialized or unavailable (fail-closed)")]
    GuardianUnavailable,
    #[error("Execution requires explicit user approval and valid capability token: {rationale}")]
    ApprovalRequired { session_id: Uuid, rationale: String },
    #[error("Capability token rejected: {0}")]
    InvalidToken(#[from] TokenConsumptionError),
    #[error("Sandbox boundary violation: {0}")]
    SandboxViolation(#[from] SandboxViolation),
    #[error("Policy evaluation denied access: {reason}")]
    PolicyDenied { reason: String },
    #[error("Privilege escalation attempt in justification or arguments strictly rejected")]
    PrivilegeEscalationAttempt,
}

pub struct GuardianExecutionBroker {
    policy: Arc<PolicyEngine>,
    audit: Arc<AuditLedger>,
    approval: Arc<ApprovalBroker>,
    sandbox: Arc<SandboxPolicy>,
}

impl GuardianExecutionBroker {
    pub fn new(
        policy: Arc<PolicyEngine>,
        audit: Arc<AuditLedger>,
        approval: Arc<ApprovalBroker>,
        sandbox: Arc<SandboxPolicy>,
    ) -> Self {
        Self {
            policy,
            audit,
            approval,
            sandbox,
        }
    }

    pub fn policy(&self) -> &Arc<PolicyEngine> {
        &self.policy
    }

    pub fn audit(&self) -> &Arc<AuditLedger> {
        &self.audit
    }

    pub fn approval(&self) -> &Arc<ApprovalBroker> {
        &self.approval
    }

    pub fn sandbox(&self) -> &Arc<SandboxPolicy> {
        &self.sandbox
    }

    /// Point-of-use gatekeeper: enforces policy, sandbox boundaries, token consumption,
    /// and tamper-evident audit logging. Fails closed with zero side-effects if any check fails.
    pub fn authorize_and_execute<F, R>(
        &self,
        caller: SubsystemId,
        capability: Capability,
        token_id: Option<Uuid>,
        correlation_id: Option<Uuid>,
        execute_fn: F,
    ) -> Result<R, ExecutionDenied>
    where
        F: FnOnce() -> R,
    {
        if matches!(
            capability,
            Capability::Filesystem(_) | Capability::Tool(ToolCapability::CliCommand(_))
        ) {
            self.audit.record_with_correlation(
                caller,
                capability,
                DecisionOutcome::Denied,
                "Use a descriptor-bound file operation or isolated command runner",
                correlation_id,
            );
            return Err(ExecutionDenied::DescriptorRequired);
        }
        self.authorize_inner(caller, capability, token_id, correlation_id, execute_fn)
    }

    pub fn authorize_read<F, R>(
        &self,
        caller: SubsystemId,
        path: &std::path::Path,
        token_id: Option<Uuid>,
        correlation_id: Option<Uuid>,
        execute_fn: F,
    ) -> Result<R, ExecutionDenied>
    where
        F: FnOnce(crate::confined::ConfinedFile) -> R,
    {
        let file = match self.sandbox.open_read(path) {
            Ok(file) => file,
            Err(error) => {
                self.audit.record_with_correlation(
                    caller,
                    Capability::Filesystem(FsCapability::Read { path: path.into() }),
                    DecisionOutcome::Denied,
                    format!("Descriptor sandbox denied: {error}"),
                    correlation_id,
                );
                return Err(error.into());
            }
        };
        let capability = Capability::Filesystem(FsCapability::Read {
            path: file.path().to_path_buf(),
        });
        self.authorize_inner(caller, capability, token_id, correlation_id, || {
            execute_fn(file)
        })
    }

    fn authorize_inner<F, R>(
        &self,
        caller: SubsystemId,
        capability: Capability,
        token_id: Option<Uuid>,
        correlation_id: Option<Uuid>,
        execute_fn: F,
    ) -> Result<R, ExecutionDenied>
    where
        F: FnOnce() -> R,
    {
        // 1. Sandbox validation at the execution boundary
        if let Capability::Filesystem(fs_cap) = &capability {
            let (path, is_write) = match fs_cap {
                FsCapability::Read { path } => (path, false),
                FsCapability::Write { path } => (path, true),
                FsCapability::Execute { path } => (path, true),
            };

            if let Err(violation) = self.sandbox.validate_path(path, is_write) {
                let reason = format!("Sandbox filesystem violation: {violation:?}");
                self.audit.record_with_correlation(
                    caller,
                    capability.clone(),
                    DecisionOutcome::Denied,
                    &reason,
                    correlation_id,
                );
                return Err(ExecutionDenied::SandboxViolation(violation));
            }
        }

        if let Capability::Tool(ToolCapability::CliCommand(cmd)) = &capability {
            if let Err(violation) = self.sandbox.validate_command(cmd) {
                let reason = format!("Sandbox CLI violation: {violation:?}");
                self.audit.record_with_correlation(
                    caller,
                    capability.clone(),
                    DecisionOutcome::Denied,
                    &reason,
                    correlation_id,
                );
                return Err(ExecutionDenied::SandboxViolation(violation));
            }
        }

        // 2. Risk evaluation and CapabilityToken consumption
        if self
            .policy
            .is_explicitly_granted_for(caller, &capability, correlation_id)
        {
            // Explicitly granted capability (e.g. active device consent)
        } else {
            match capability.risk_level() {
                RiskLevel::Low => {
                    // Auto-approved low risk (e.g. os:status telemetry)
                }
                RiskLevel::Medium | RiskLevel::High | RiskLevel::Critical => {
                    // High/Critical MUST have a valid, unconsumed, unexpired token bound to this actor and capability
                    let tid = match token_id {
                        Some(id) => id,
                        None => {
                            let session = self.approval.create_session_for(
                                correlation_id,
                                caller,
                                capability.clone(),
                                "Execution boundary gate check requires user approval",
                            );
                            let reason = format!(
                                "Approval required for high-risk operation: {:?}",
                                capability
                            );
                            self.audit.record_with_correlation(
                                caller,
                                capability.clone(),
                                DecisionOutcome::Denied,
                                &reason,
                                correlation_id,
                            );
                            return Err(ExecutionDenied::ApprovalRequired {
                                session_id: session.session_id,
                                rationale: reason,
                            });
                        }
                    };

                    if let Err(e) =
                        self.approval
                            .consume_token_for(tid, caller, &capability, correlation_id)
                    {
                        let reason = format!("Token consumption rejected: {e}");
                        self.audit.record_with_correlation(
                            caller,
                            capability.clone(),
                            DecisionOutcome::Denied,
                            &reason,
                            correlation_id,
                        );
                        return Err(ExecutionDenied::InvalidToken(e));
                    }
                }
            }
        }

        // 3. Execution at the point of use
        let result = execute_fn();

        // 4. Record execution success in audit ledger
        self.audit.record_with_correlation(
            caller,
            capability,
            DecisionOutcome::Granted,
            "Authorization accepted at Guardian execution boundary",
            correlation_id,
        );

        Ok(result)
    }
}

static GLOBAL_BROKER: RwLock<Option<Arc<GuardianExecutionBroker>>> = RwLock::new(None);

pub fn register_global_broker(broker: Arc<GuardianExecutionBroker>) {
    let mut guard = GLOBAL_BROKER.write().unwrap();
    *guard = Some(broker);
}

pub fn clear_global_broker() {
    let mut guard = GLOBAL_BROKER.write().unwrap();
    *guard = None;
}

pub fn global_broker() -> Result<Arc<GuardianExecutionBroker>, ExecutionDenied> {
    let guard = GLOBAL_BROKER.read().unwrap();
    guard.clone().ok_or(ExecutionDenied::GuardianUnavailable)
}
