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
    AnyToolInvocation {
        tool_name: String,
    },
    KernelTool {
        tool_name: String,
    },
    FilesystemReadPrefix {
        prefix: PathBuf,
    },
    FilesystemWritePrefix {
        prefix: PathBuf,
    },
    SystemOperation {
        operation: String,
    },
    /// One fixed sandbox operation, for any input: e.g. "read PDF text" (`pdf-text-v1`),
    /// whatever the document. Only protocols in [`STANDING_SANDBOX_PROTOCOLS`] qualify.
    SandboxOperation {
        protocol: String,
    },
    ExactCapability(Capability),
}

/// Sandbox protocols an owner may allow once for every input. Each is a fixed operation
/// chosen by SAIL, not by the requester: no executable, argv, network, mount or credential
/// in the request, only the input digest/size, ids and a bounded operation. The job still
/// runs in the dedicated-UID runner sandbox; the rule only removes the per-file prompt.
pub const STANDING_SANDBOX_PROTOCOLS: &[(&str, &str)] =
    &[("pdf-text-v1", "Read PDF text in the sandbox")];

/// The fixed protocol of a sandbox request, if it is one of
/// [`STANDING_SANDBOX_PROTOCOLS`] and has exactly that protocol's request shape. Any extra
/// field (a smuggled executable, path or network setting) disqualifies it.
pub fn standing_sandbox_protocol(request: &str) -> Option<&'static str> {
    let value: serde_json::Value = serde_json::from_str(request).ok()?;
    let object = value.as_object()?;
    let protocol = object.get("protocol")?.as_str()?;
    let (known, _) = STANDING_SANDBOX_PROTOCOLS
        .iter()
        .find(|(name, _)| *name == protocol)?;
    const PDF_FIELDS: [&str; 6] = [
        "protocol",
        "request_id",
        "correlation_id",
        "input_bytes",
        "input_sha256",
        "operation",
    ];
    let shape_ok = object.len() == PDF_FIELDS.len()
        && PDF_FIELDS.iter().all(|field| object.contains_key(*field))
        && object["input_sha256"]
            .as_str()
            .is_some_and(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
        && object["input_bytes"].is_u64();
    shape_ok.then_some(*known)
}

impl LearnedPattern {
    pub fn matches(&self, capability: &Capability) -> bool {
        if matches!(capability, Capability::Tool(ToolCapability::Invocation { tool, .. }) if tool == "external:infer")
        {
            // A historical wildcard learned for ordinary tools cannot authorize egress.
            return matches!(self, Self::ExactCapability(expected) if expected == capability);
        }
        match (self, capability) {
            (LearnedPattern::ExactCapability(expected), actual) => expected == actual,
            (
                LearnedPattern::SandboxOperation { protocol },
                Capability::Tool(ToolCapability::SandboxCommand { request, .. }),
            ) => standing_sandbox_protocol(request) == Some(protocol.as_str()),
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
            Capability::Tool(ToolCapability::Invocation { tool, .. })
                if tool == "external:infer" =>
            {
                Self::ExactCapability(capability.clone())
            }
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
            Capability::Tool(ToolCapability::SandboxCommand { request, .. })
                if standing_sandbox_protocol(request).is_some() =>
            {
                LearnedPattern::SandboxOperation {
                    protocol: standing_sandbox_protocol(request)
                        .unwrap_or_default()
                        .to_string(),
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
            LearnedPattern::SandboxOperation { protocol } => STANDING_SANDBOX_PROTOCOLS
                .iter()
                .find(|(name, _)| name == protocol)
                .map(|(_, label)| format!("{label} ({protocol})"))
                .unwrap_or_else(|| format!("Sandbox: {protocol}")),
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

/// An explicit grant: who, which capability, and optionally which correlated task.
type GrantKey = (SubsystemId, Capability, Option<uuid::Uuid>);

pub struct PolicyEngine {
    sandbox: SandboxPolicy,
    explicit_grants: RwLock<HashMap<GrantKey, chrono::DateTime<chrono::Utc>>>,
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
        self.learn_pattern(
            requester,
            LearnedPattern::from_capability(capability),
            description,
        )
    }

    /// Store an owner-approved standing rule. Callers must have authenticated the owner.
    pub fn learn_pattern(
        &self,
        requester: SubsystemId,
        pattern: LearnedPattern,
        description: String,
    ) -> LearnedApprovalRule {
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

    /// Whether a standing rule allows `protocol` for `requester`, without counting a use.
    /// Background work checks this first so it never raises an approval prompt per file.
    pub fn allows_sandbox_operation(&self, requester: SubsystemId, protocol: &str) -> bool {
        let wanted = LearnedPattern::SandboxOperation {
            protocol: protocol.to_string(),
        };
        self.learned_rules
            .read()
            .map(|rules| {
                rules
                    .iter()
                    .any(|r| r.requester == requester && r.pattern == wanted)
            })
            .unwrap_or(false)
    }

    /// Remove the standing rule for `protocol`, if any. Returns whether one was removed.
    pub fn revoke_sandbox_operation(&self, requester: SubsystemId, protocol: &str) -> bool {
        let wanted = LearnedPattern::SandboxOperation {
            protocol: protocol.to_string(),
        };
        let ids: Vec<Uuid> = self
            .learned_rules
            .read()
            .map(|rules| {
                rules
                    .iter()
                    .filter(|r| r.requester == requester && r.pattern == wanted)
                    .map(|r| r.id)
                    .collect()
            })
            .unwrap_or_default();
        let mut any = false;
        for id in ids {
            any |= self.revoke_learned_rule(id);
        }
        any
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
            if let Ok(rules) = self.learned_rules.read()
                && let Ok(json) = serde_json::to_string_pretty(&*rules)
            {
                let _ = std::fs::write(path, json);
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

        if let Capability::Tool(ToolCapability::CliCommand(cmd)) = capability
            && !self.sandbox.is_command_safe(cmd)
        {
            let reason = format!("Prohibited command contains unsafe privilege escalation: {cmd}");
            audit.record(
                requester,
                capability.clone(),
                DecisionOutcome::Denied,
                &reason,
            );
            return AuthorizationDecision::Denied { reason };
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

    fn pdf_request(digest: &str, extra: Option<(&str, serde_json::Value)>) -> Capability {
        let mut request = serde_json::json!({
            "protocol": "pdf-text-v1",
            "request_id": Uuid::new_v4(),
            "correlation_id": Uuid::new_v4(),
            "input_bytes": 1234,
            "input_sha256": digest,
            "operation": {"pages": {"first": 1, "last": 20}},
        });
        if let Some((key, value)) = extra {
            request[key] = value;
        }
        Capability::Tool(ToolCapability::SandboxCommand {
            request_digest: "0".repeat(64),
            request: request.to_string(),
        })
    }

    #[test]
    fn one_pdf_approval_covers_every_pdf_but_nothing_else() {
        let temp_dir = std::env::temp_dir().join(format!("sail-pdf-rule-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let policy = PolicyEngine::with_persistence(temp_dir.join("learned_rules.json"));
        let first = pdf_request(&"a".repeat(64), None);
        let other_file = pdf_request(&"b".repeat(64), None);
        assert!(!policy.allows_sandbox_operation(SubsystemId::Arena, "pdf-text-v1"));
        assert!(!policy.is_learned_approved(SubsystemId::Arena, &other_file));

        // "Always allow" on one PDF prompt learns the operation, not the exact file.
        let rule = policy.learn_approval(SubsystemId::Arena, &first, "always".into());
        assert_eq!(
            rule.pattern,
            LearnedPattern::SandboxOperation {
                protocol: "pdf-text-v1".into()
            }
        );
        assert!(rule.pattern.display_label().contains("Read PDF text"));
        assert!(policy.allows_sandbox_operation(SubsystemId::Arena, "pdf-text-v1"));
        assert!(policy.is_learned_approved(SubsystemId::Arena, &other_file));
        assert!(!policy.is_learned_approved(SubsystemId::Forge, &other_file));

        // A smuggled field, another protocol or a command plan is never covered.
        let smuggled = pdf_request(&"c".repeat(64), Some(("executable", "/bin/sh".into())));
        assert!(!policy.is_learned_approved(SubsystemId::Arena, &smuggled));
        let mut other_protocol = pdf_request(&"d".repeat(64), None);
        if let Capability::Tool(ToolCapability::SandboxCommand { request, .. }) =
            &mut other_protocol
        {
            *request = request.replace("pdf-text-v1", "command-v1");
        }
        assert!(!policy.is_learned_approved(SubsystemId::Arena, &other_protocol));
        let bad_digest = pdf_request("not-a-digest", None);
        assert!(!policy.is_learned_approved(SubsystemId::Arena, &bad_digest));

        // The rule survives a restart and can be revoked by operation.
        let reloaded = PolicyEngine::with_persistence(temp_dir.join("learned_rules.json"));
        assert!(reloaded.allows_sandbox_operation(SubsystemId::Arena, "pdf-text-v1"));
        assert!(reloaded.revoke_sandbox_operation(SubsystemId::Arena, "pdf-text-v1"));
        assert!(!reloaded.is_learned_approved(SubsystemId::Arena, &other_file));
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

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
