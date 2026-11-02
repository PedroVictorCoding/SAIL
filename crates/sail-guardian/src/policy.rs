use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;
use uuid::Uuid;

use crate::approval::ApprovalBroker;
use crate::audit::{AuditLedger, DecisionOutcome};
use crate::capability::{Capability, FsCapability, RiskLevel, ToolCapability};
use crate::sandbox::SandboxPolicy;
use sail_common::types::SubsystemId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum LearnedPattern {
    AnyToolInvocation { tool_name: String },
    KernelTool { tool_name: String },
    FilesystemReadPrefix { prefix: PathBuf },
    FilesystemWritePrefix { prefix: PathBuf },
    SystemOperation { operation: String },
    ExactCapability(Capability),
}

impl LearnedPattern {
    pub fn matches(&self, capability: &Capability) -> bool {
        match (self, capability) {
            (LearnedPattern::ExactCapability(expected), actual) => expected == actual,
            (
                LearnedPattern::AnyToolInvocation { tool_name },
                Capability::Tool(ToolCapability::Invocation { tool, .. }),
            ) => tool == tool_name || tool_name == "*",
            (
                LearnedPattern::KernelTool { tool_name },
                Capability::Tool(ToolCapability::KernelTool(name)),
            ) => name == tool_name || tool_name == "*",
            (
                LearnedPattern::FilesystemReadPrefix { prefix },
                Capability::Filesystem(FsCapability::Read { path }),
            ) => path.starts_with(prefix),
            (
                LearnedPattern::FilesystemWritePrefix { prefix },
                Capability::Filesystem(FsCapability::Write { path }),
            ) => path.starts_with(prefix),
            (LearnedPattern::SystemOperation { operation }, Capability::System(sys)) => match sys {
                crate::capability::SystemCapability::ProcessInspect => {
                    operation == "process_inspect"
                }
                crate::capability::SystemCapability::OsTelemetry => operation == "os_telemetry",
                crate::capability::SystemCapability::NetworkAccess { host } => {
                    operation == host || operation == "*"
                }
                _ => false,
            },
            _ => false,
        }
    }

    pub fn from_capability(capability: &Capability) -> Self {
        match capability {
            Capability::Tool(ToolCapability::Invocation { tool, .. }) => {
                LearnedPattern::AnyToolInvocation {
                    tool_name: tool.clone(),
                }
            }
            Capability::Tool(ToolCapability::KernelTool(name)) => LearnedPattern::KernelTool {
                tool_name: name.clone(),
            },
            Capability::Filesystem(FsCapability::Read { path }) => {
                let prefix = if path.is_file() {
                    path.parent().unwrap_or(path).to_path_buf()
                } else {
                    path.clone()
                };
                LearnedPattern::FilesystemReadPrefix { prefix }
            }
            Capability::Filesystem(FsCapability::Write { path }) => {
                let prefix = if path.is_file() {
                    path.parent().unwrap_or(path).to_path_buf()
                } else {
                    path.clone()
                };
                LearnedPattern::FilesystemWritePrefix { prefix }
            }
            Capability::System(crate::capability::SystemCapability::ProcessInspect) => {
                LearnedPattern::SystemOperation {
                    operation: "process_inspect".into(),
                }
            }
            Capability::System(crate::capability::SystemCapability::OsTelemetry) => {
                LearnedPattern::SystemOperation {
                    operation: "os_telemetry".into(),
                }
            }
            other => LearnedPattern::ExactCapability(other.clone()),
        }
    }

    pub fn display_label(&self) -> String {
        match self {
            LearnedPattern::AnyToolInvocation { tool_name } => format!("Tool: {tool_name}"),
            LearnedPattern::KernelTool { tool_name } => format!("Tool: {tool_name}"),
            LearnedPattern::FilesystemReadPrefix { prefix } => {
                format!("Read: {}", prefix.display())
            }
            LearnedPattern::FilesystemWritePrefix { prefix } => {
                format!("Write: {}", prefix.display())
            }
            LearnedPattern::SystemOperation { operation } => format!("System: {operation}"),
            LearnedPattern::ExactCapability(cap) => format!("{cap:?}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LearnedApprovalRule {
    pub id: Uuid,
    pub requester: SubsystemId,
    pub pattern: LearnedPattern,
    pub description: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub auto_approved_count: u64,
}

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
    autonomy_preset: RwLock<crate::runtime::AutonomyPreset>,
    learned_rules: RwLock<Vec<LearnedApprovalRule>>,
    persistence_path: Option<PathBuf>,
}

impl PolicyEngine {
    pub fn new() -> Self {
        let path = std::env::var("SAIL_LEARNED_RULES_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(".local/guardian_learned_rules.json"));
        Self::with_persistence(path)
    }

    pub fn with_persistence(persistence_path: PathBuf) -> Self {
        let learned = if persistence_path.exists() {
            std::fs::read_to_string(&persistence_path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Vec<LearnedApprovalRule>>(&raw).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        Self {
            sandbox: SandboxPolicy::new(),
            explicit_grants: RwLock::new(HashMap::new()),
            autonomy_preset: RwLock::new(crate::runtime::AutonomyPreset::Balanced),
            learned_rules: RwLock::new(learned),
            persistence_path: Some(persistence_path),
        }
    }

    pub fn autonomy_preset(&self) -> crate::runtime::AutonomyPreset {
        *self.autonomy_preset.read().unwrap()
    }

    pub fn set_autonomy_preset(&self, preset: crate::runtime::AutonomyPreset) {
        *self.autonomy_preset.write().unwrap() = preset;
    }

    /// Learn a persistent approval rule so the user does not have to approve the same action repeatedly.
    pub fn learn_approval(
        &self,
        requester: SubsystemId,
        capability: &Capability,
        description: String,
    ) -> LearnedApprovalRule {
        let pattern = LearnedPattern::from_capability(capability);
        let rule = LearnedApprovalRule {
            id: Uuid::new_v4(),
            requester,
            pattern,
            description,
            created_at: chrono::Utc::now(),
            auto_approved_count: 0,
        };

        {
            let mut rules = self.learned_rules.write().unwrap();
            rules.retain(|r| !(r.requester == requester && r.pattern == rule.pattern));
            rules.push(rule.clone());
        }

        self.save_learned_rules();
        rule
    }

    pub fn check_learned_approval(
        &self,
        requester: SubsystemId,
        capability: &Capability,
    ) -> Option<String> {
        let mut rules = self.learned_rules.write().unwrap();
        for rule in rules.iter_mut() {
            if rule.requester == requester && rule.pattern.matches(capability) {
                rule.auto_approved_count += 1;
                let desc = format!("{} ({})", rule.pattern.display_label(), rule.description);
                return Some(desc);
            }
        }
        None
    }

    pub fn is_learned_approved(&self, requester: SubsystemId, capability: &Capability) -> bool {
        self.check_learned_approval(requester, capability).is_some()
    }

    pub fn list_learned_rules(&self) -> Vec<LearnedApprovalRule> {
        self.learned_rules.read().unwrap().clone()
    }

    pub fn revoke_learned_rule(&self, rule_id: Uuid) -> bool {
        let mut rules = self.learned_rules.write().unwrap();
        let init_len = rules.len();
        rules.retain(|r| r.id != rule_id);
        let changed = rules.len() != init_len;
        drop(rules);
        if changed {
            self.save_learned_rules();
        }
        changed
    }

    pub fn clear_learned_rules(&self) {
        let mut rules = self.learned_rules.write().unwrap();
        rules.clear();
        drop(rules);
        self.save_learned_rules();
    }

    fn save_learned_rules(&self) {
        if let Some(ref path) = self.persistence_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(rules) = self.learned_rules.read() {
                if let Ok(json) = serde_json::to_string_pretty(&*rules) {
                    let _ = std::fs::write(path, json);
                }
            }
        }
    }

    /// Grant a capability explicitly to a subsystem
    pub fn grant_capability(&self, subsystem: SubsystemId, capability: Capability) {
        self.grant_scoped(subsystem, capability, None, chrono::Duration::hours(24));
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
                now + ttl.min(chrono::Duration::days(7)),
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

        // 2. Check learned approval rules
        if let Some(rule_desc) = self.check_learned_approval(requester, capability) {
            audit.record(
                requester,
                capability.clone(),
                DecisionOutcome::Granted,
                format!("Auto-approved by learned approval rule: {rule_desc}"),
            );
            return AuthorizationDecision::Allowed;
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

        // 4. Default policy by risk tier and autonomy preset
        let preset = self.autonomy_preset();
        let risk = capability.risk_level();

        match preset {
            crate::runtime::AutonomyPreset::Careful => {
                if risk == RiskLevel::Low {
                    audit.record(
                        requester,
                        capability.clone(),
                        DecisionOutcome::Granted,
                        "Low-risk telemetry auto-approved in Careful mode",
                    );
                    AuthorizationDecision::Allowed
                } else {
                    let session = approval_broker.create_session(
                        requester,
                        capability.clone(),
                        justification,
                    );
                    AuthorizationDecision::RequiresUserApproval {
                        session_id: session.session_id,
                        rationale: format!(
                            "Careful preset requires explicit owner confirmation for {:?}: {justification}",
                            capability
                        ),
                    }
                }
            }
            crate::runtime::AutonomyPreset::Balanced => match risk {
                RiskLevel::Low => {
                    audit.record(
                        requester,
                        capability.clone(),
                        DecisionOutcome::Granted,
                        "Low-risk telemetry auto-approved in Balanced mode",
                    );
                    AuthorizationDecision::Allowed
                }
                RiskLevel::Medium | RiskLevel::High | RiskLevel::Critical => {
                    let session = approval_broker.create_session(
                        requester,
                        capability.clone(),
                        justification,
                    );
                    AuthorizationDecision::RequiresUserApproval {
                        session_id: session.session_id,
                        rationale: format!(
                            "High risk operation requires explicit user grant: {:?}",
                            capability
                        ),
                    }
                }
            },
            crate::runtime::AutonomyPreset::Autonomous => match risk {
                RiskLevel::Low | RiskLevel::Medium | RiskLevel::High => {
                    audit.record(
                        requester,
                        capability.clone(),
                        DecisionOutcome::Granted,
                        "Autonomous preset auto-approved within sandbox boundary",
                    );
                    AuthorizationDecision::Allowed
                }
                RiskLevel::Critical => {
                    let session = approval_broker.create_session(
                        requester,
                        capability.clone(),
                        justification,
                    );
                    AuthorizationDecision::RequiresUserApproval {
                        session_id: session.session_id,
                        rationale: format!(
                            "Critical operation requires explicit user grant: {:?}",
                            capability
                        ),
                    }
                }
            },
        }
    }
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{FsCapability, ToolCapability};

    #[test]
    fn test_learned_approval_rules_lifecycle() {
        let temp_dir = std::env::temp_dir().join(format!("sail-learned-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let rule_path = temp_dir.join("learned_rules.json");

        let policy = PolicyEngine::with_persistence(rule_path.clone());
        let cap_github = Capability::Tool(ToolCapability::KernelTool("github:issues".into()));
        let cap_other = Capability::Tool(ToolCapability::KernelTool("command:execute".into()));

        assert!(!policy.is_learned_approved(SubsystemId::Dialogue, &cap_github));

        let rule = policy.learn_approval(
            SubsystemId::Dialogue,
            &cap_github,
            "Always allow GitHub issues".into(),
        );

        assert!(policy.is_learned_approved(SubsystemId::Dialogue, &cap_github));
        assert!(!policy.is_learned_approved(SubsystemId::Dialogue, &cap_other));
        assert!(!policy.is_learned_approved(SubsystemId::Forge, &cap_github));

        let rules = policy.list_learned_rules();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].auto_approved_count >= 1);

        // Verify persistence across engine reloads
        let policy_reloaded = PolicyEngine::with_persistence(rule_path);
        assert!(policy_reloaded.is_learned_approved(SubsystemId::Dialogue, &cap_github));

        // Verify revocation
        assert!(policy_reloaded.revoke_learned_rule(rule.id));
        assert!(!policy_reloaded.is_learned_approved(SubsystemId::Dialogue, &cap_github));
    }

    #[test]
    fn test_learned_pattern_filesystem_matching() {
        let policy = PolicyEngine::new();
        let read_cap = Capability::Filesystem(FsCapability::Read {
            path: PathBuf::from("crates/sail-guardian/Cargo.toml"),
        });
        let outside_cap = Capability::Filesystem(FsCapability::Read {
            path: PathBuf::from("/etc/shadow"),
        });

        policy.learn_approval(
            SubsystemId::Dialogue,
            &read_cap,
            "Allow reading crates/".into(),
        );

        assert!(policy.is_learned_approved(SubsystemId::Dialogue, &read_cap));
        assert!(!policy.is_learned_approved(SubsystemId::Dialogue, &outside_cap));
    }
}
