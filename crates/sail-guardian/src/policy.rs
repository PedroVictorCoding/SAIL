use std::collections::HashMap;
use std::sync::RwLock;

use crate::approval::ApprovalBroker;
use crate::audit::{AuditLedger, DecisionOutcome};
use crate::capability::{Capability, FsCapability, RiskLevel, ToolCapability};
use crate::sandbox::SandboxPolicy;
use sail_common::types::SubsystemId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizationDecision {
    Allowed,
    Denied {
        reason: String,
    },
    RequiresUserApproval {
        session_id: uuid::Uuid,
        rationale: String,
    },
}

pub struct PolicyEngine {
    sandbox: SandboxPolicy,
    explicit_grants: RwLock<
        HashMap<(SubsystemId, Capability, Option<uuid::Uuid>), chrono::DateTime<chrono::Utc>>,
    >,
}

impl PolicyEngine {
    pub fn new() -> Self {
        Self {
            sandbox: SandboxPolicy::new(),
            explicit_grants: RwLock::new(HashMap::new()),
        }
    }

    /// Grant a capability explicitly to a subsystem
    pub fn grant_capability(&self, subsystem: SubsystemId, capability: Capability) {
        self.grant_scoped(subsystem, capability, None, chrono::Duration::seconds(60));
    }
    /// Trusted owner/device-consent setup only; even native grants expire.
    pub fn grant_scoped(
        &self,
        subsystem: SubsystemId,
        capability: Capability,
        correlation: Option<uuid::Uuid>,
        ttl: chrono::Duration,
    ) {
        if ttl <= chrono::Duration::zero() {
            return;
        }
        let mut grants = self.explicit_grants.write().unwrap();
        let now = chrono::Utc::now();
        grants.retain(|_, expires| *expires > now);
        if grants.len() < 4096 {
            grants.insert(
                (subsystem, capability, correlation),
                now + ttl.min(chrono::Duration::seconds(60)),
            );
        }
    }
    pub fn is_explicitly_granted(&self, subsystem: SubsystemId, capability: &Capability) -> bool {
        self.is_explicitly_granted_for(subsystem, capability, None)
    }
    pub fn is_explicitly_granted_for(
        &self,
        subsystem: SubsystemId,
        capability: &Capability,
        correlation: Option<uuid::Uuid>,
    ) -> bool {
        self.explicit_grants
            .read()
            .unwrap()
            .get(&(subsystem, capability.clone(), correlation))
            .is_some_and(|expires| *expires > chrono::Utc::now())
    }

    /// Revoke a capability
    pub fn revoke_capability(&self, subsystem: SubsystemId, capability: &Capability) {
        let mut guard = self.explicit_grants.write().unwrap();
        guard.retain(|(actor, cap, _), _| *actor != subsystem || cap != capability);
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
                let reason = format!(
                    "Filesystem access denied by sandbox policy: {:?}",
                    target_path
                );
                audit.record(
                    requester,
                    capability.clone(),
                    DecisionOutcome::Denied,
                    &reason,
                );
                return AuthorizationDecision::Denied { reason };
            }
        }

        if let Capability::Tool(ToolCapability::CliCommand(cmd)) = capability {
            if !self.sandbox.is_command_safe(cmd) {
                let reason =
                    format!("Prohibited command contains unsafe privilege escalation: {cmd}");
                audit.record(
                    requester,
                    capability.clone(),
                    DecisionOutcome::Denied,
                    &reason,
                );
                return AuthorizationDecision::Denied { reason };
            }
        }

        // 3. Check explicit capability grants
        {
            if self.is_explicitly_granted(requester, capability) {
                audit.record(
                    requester,
                    capability.clone(),
                    DecisionOutcome::Granted,
                    "Explicitly granted",
                );
                return AuthorizationDecision::Allowed;
            }
        }

        // 4. Default policy by risk tier
        match capability.risk_level() {
            // Low risk (e.g. os:status telemetry) is auto-granted to recognized subsystems
            RiskLevel::Low => {
                audit.record(
                    requester,
                    capability.clone(),
                    DecisionOutcome::Granted,
                    "Low-risk telemetry auto-approved",
                );
                AuthorizationDecision::Allowed
            }
            // Medium risk (proc:list, internal fs queries)
            // High or Critical risk always requires user approval
            RiskLevel::Medium | RiskLevel::High | RiskLevel::Critical => {
                let session =
                    approval_broker.create_session(requester, capability.clone(), justification);
                AuthorizationDecision::RequiresUserApproval {
                    session_id: session.session_id,
                    rationale: format!(
                        "High risk operation requires explicit user grant: {:?}",
                        capability
                    ),
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
