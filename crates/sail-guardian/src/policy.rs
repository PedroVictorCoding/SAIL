use std::collections::HashSet;
use std::sync::RwLock;

use crate::approval::ApprovalBroker;
use crate::audit::{AuditLedger, DecisionOutcome};
use crate::capability::{Capability, FsCapability, RiskLevel, ToolCapability};
use crate::sandbox::SandboxPolicy;
use sail_common::types::SubsystemId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizationDecision {
    Allowed,
    Denied { reason: String },
    RequiresUserApproval { session_id: uuid::Uuid, rationale: String },
}

pub struct PolicyEngine {
    sandbox: SandboxPolicy,
    explicit_grants: RwLock<HashSet<(SubsystemId, Capability)>>,
}

impl PolicyEngine {
    pub fn new() -> Self {
        Self {
            sandbox: SandboxPolicy::new(),
            explicit_grants: RwLock::new(HashSet::new()),
        }
    }

    /// Grant a capability explicitly to a subsystem
    pub fn grant_capability(&self, subsystem: SubsystemId, capability: Capability) {
        let mut guard = self.explicit_grants.write().unwrap();
        guard.insert((subsystem, capability));
    }

    /// Revoke a capability
    pub fn revoke_capability(&self, subsystem: SubsystemId, capability: &Capability) {
        let mut guard = self.explicit_grants.write().unwrap();
        guard.remove(&(subsystem, capability.clone()));
    }

    /// Evaluate an authorization request under deny-by-default rules
    pub fn evaluate(
        &self,
        requester: SubsystemId,
        capability: &Capability,
        justification: &str,
        audit: &AuditLedger,
        approval_broker: &ApprovalBroker,
    ) -> AuthorizationDecision {
        // 1. Check sandbox violations
        if let Capability::Filesystem(fs) = capability {
            let (target_path, is_write) = match fs {
                FsCapability::Read { path } => (path, false),
                FsCapability::Write { path } => (path, true),
                FsCapability::Execute { path } => (path, true),
            };

            if !self.sandbox.is_path_allowed(target_path, is_write) {
                let reason = format!("Filesystem access denied by sandbox policy: {:?}", target_path);
                audit.record(requester, capability.clone(), DecisionOutcome::Denied, &reason);
                return AuthorizationDecision::Denied { reason };
            }
        }

        if let Capability::Tool(ToolCapability::CliCommand(cmd)) = capability {
            if !self.sandbox.is_command_safe(cmd) {
                let reason = format!("Prohibited command contains unsafe privilege escalation: {cmd}");
                audit.record(requester, capability.clone(), DecisionOutcome::Denied, &reason);
                return AuthorizationDecision::Denied { reason };
            }
        }

        // 2. Reject untrusted model-requested privilege escalation
        if justification.to_lowercase().contains("grant root")
            || justification.to_lowercase().contains("elevate privileges")
            || justification.to_lowercase().contains("bypass guardian")
        {
            let reason = "Untrusted model privilege change request strictly rejected".to_string();
            audit.record(requester, capability.clone(), DecisionOutcome::Denied, &reason);
            return AuthorizationDecision::Denied { reason };
        }

        // 3. Check explicit capability grants
        {
            let grants = self.explicit_grants.read().unwrap();
            if grants.contains(&(requester, capability.clone())) {
                audit.record(requester, capability.clone(), DecisionOutcome::Granted, "Explicitly granted");
                return AuthorizationDecision::Allowed;
            }
        }

        // 4. Default policy by risk tier
        match capability.risk_level() {
            // Low risk (e.g. os:status telemetry) is auto-granted to recognized subsystems
            RiskLevel::Low => {
                audit.record(requester, capability.clone(), DecisionOutcome::Granted, "Low-risk telemetry auto-approved");
                AuthorizationDecision::Allowed
            }
            // Medium risk (proc:list, internal fs queries)
            RiskLevel::Medium => {
                // If requested by Core or Dialogue during interactive user session, allow
                if requester == SubsystemId::Core || requester == SubsystemId::Dialogue || requester == SubsystemId::Bridge {
                    audit.record(requester, capability.clone(), DecisionOutcome::Granted, "Interactive session default grant");
                    AuthorizationDecision::Allowed
                } else {
                    let session = approval_broker.create_session(requester, capability.clone(), justification);
                    AuthorizationDecision::RequiresUserApproval {
                        session_id: session.session_id,
                        rationale: format!("Medium risk operation requires authorization: {:?}", capability),
                    }
                }
            }
            // High or Critical risk always requires user approval
            RiskLevel::High | RiskLevel::Critical => {
                let session = approval_broker.create_session(requester, capability.clone(), justification);
                AuthorizationDecision::RequiresUserApproval {
                    session_id: session.session_id,
                    rationale: format!("High risk operation requires explicit user grant: {:?}", capability),
                }
            }
        }
    }
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self::new()
    }
}
