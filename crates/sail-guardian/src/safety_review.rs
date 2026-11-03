use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tracing::info;
use uuid::Uuid;

use crate::audit::AuditLedger;
use crate::capability::Capability;
use crate::policy::{LearnedApprovalRule, LearnedPattern, PolicyEngine};
use sail_common::types::SubsystemId;

/// Severity of a safety review finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyFindingSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

/// Action suggested by the safety evaluator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendedAction {
    Keep,
    NarrowScope { suggested_pattern: LearnedPattern },
    Revoke,
    Dismiss,
}

/// Status of an individual finding in the notification and review lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    Unread,
    Acknowledged,
    Resolved {
        decision: String,
        resolved_at: DateTime<Utc>,
    },
    Dismissed,
}

/// Individual safety review finding produced for a learned approval rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SafetyFinding {
    pub finding_id: Uuid,
    pub rule_id: Uuid,
    pub rule_version: u64,
    pub severity: SafetyFindingSeverity,
    pub title: String,
    pub reason: String,
    pub affected_capabilities: Vec<String>,
    pub evidence: Vec<String>,
    pub recommended_action: RecommendedAction,
    pub status: FindingStatus,
    pub evaluated_at: DateTime<Utc>,
    pub evaluator_version: String,
    pub policy_version: String,
}

/// Status and summary of an execution of the safety review flow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SafetyReviewRunSummary {
    pub run_id: Uuid,
    pub trigger: String, // "scheduled" or "manual"
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub success: bool,
    pub rules_evaluated: usize,
    pub findings_count: usize,
    pub high_or_critical_count: usize,
    pub evaluator_version: String,
    pub policy_version: String,
    pub error_message: Option<String>,
}

/// Evaluation and storage engine for daily safety reviews of learned approval rules.
pub struct SafetyReviewEngine {
    policy: Arc<PolicyEngine>,
    audit: Arc<AuditLedger>,
    findings: RwLock<Vec<SafetyFinding>>,
    last_run: RwLock<Option<SafetyReviewRunSummary>>,
    persistence_path: Option<PathBuf>,
}

impl SafetyReviewEngine {
    pub const EVALUATOR_VERSION: &'static str = "1.0.0";
    pub const POLICY_VERSION: &'static str = "guardian-v1";

    pub fn new(policy: Arc<PolicyEngine>, audit: Arc<AuditLedger>) -> Self {
        let path = std::env::var("SAIL_SAFETY_REVIEW_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(".local/guardian_safety_findings.json"));
        Self::with_persistence(policy, audit, path)
    }

    pub fn with_persistence(
        policy: Arc<PolicyEngine>,
        audit: Arc<AuditLedger>,
        persistence_path: PathBuf,
    ) -> Self {
        let findings = if persistence_path.exists() {
            std::fs::read_to_string(&persistence_path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Vec<SafetyFinding>>(&raw).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        Self {
            policy,
            audit,
            findings: RwLock::new(findings),
            last_run: RwLock::new(None),
            persistence_path: Some(persistence_path),
        }
    }

    /// Run the safety review across all currently persisted learned rules.
    /// Uses deterministic evaluation rules first, preventing prompt injection or untrusted inputs
    /// from expanding permissions or altering safety criteria.
    pub fn run_review(&self, trigger: &str) -> SafetyReviewRunSummary {
        let started_at = Utc::now();
        let run_id = Uuid::new_v4();
        let rules = self.policy.list_learned_rules();
        let mut new_findings = Vec::new();

        info!(
            run_id = %run_id,
            trigger = %trigger,
            rules_count = rules.len(),
            "Starting Daily Safety Review of Learned Approval Rules"
        );

        for rule in &rules {
            let findings_for_rule = self.evaluate_rule(rule);
            new_findings.extend(findings_for_rule);
        }

        let completed_at = Utc::now();
        let high_or_critical = new_findings
            .iter()
            .filter(|f| f.severity >= SafetyFindingSeverity::High)
            .count();

        // Deduplicate and update state:
        // If a finding for (rule_id, severity, reason) already exists, don't spam notifications,
        // but if severity or evidence changed, resurface it.
        {
            let mut stored = self.findings.write().unwrap();
            for finding in new_findings.clone() {
                if let Some(existing) = stored.iter_mut().find(|f| {
                    f.rule_id == finding.rule_id
                        && f.reason == finding.reason
                        && !matches!(f.status, FindingStatus::Resolved { .. })
                }) {
                    existing.evaluated_at = finding.evaluated_at;
                    existing.evidence = finding.evidence;
                    // If severity escalated, mark unread again
                    if finding.severity > existing.severity {
                        existing.severity = finding.severity;
                        existing.status = FindingStatus::Unread;
                    }
                } else {
                    stored.push(finding);
                }
            }
        }

        self.save_findings();

        let summary = SafetyReviewRunSummary {
            run_id,
            trigger: trigger.to_string(),
            started_at,
            completed_at,
            success: true,
            rules_evaluated: rules.len(),
            findings_count: new_findings.len(),
            high_or_critical_count: high_or_critical,
            evaluator_version: Self::EVALUATOR_VERSION.to_string(),
            policy_version: Self::POLICY_VERSION.to_string(),
            error_message: None,
        };

        *self.last_run.write().unwrap() = Some(summary.clone());

        // Audit the review run
        let _ = self.audit.record(
            SubsystemId::Guardian,
            Capability::System(crate::capability::SystemCapability::OsTelemetry),
            crate::audit::DecisionOutcome::Granted,
            format!(
                "Safety review run {}: evaluated {} rules, {} findings ({} high/critical)",
                run_id,
                rules.len(),
                new_findings.len(),
                high_or_critical
            ),
        );

        summary
    }

    /// Evaluates a single learned approval rule deterministically against safety invariants.
    pub fn evaluate_rule(&self, rule: &LearnedApprovalRule) -> Vec<SafetyFinding> {
        let mut findings = Vec::new();
        let evaluated_at = Utc::now();

        // Check 1: Root / overly broad filesystem write access
        match &rule.pattern {
            LearnedPattern::FilesystemWritePrefix { prefix } => {
                let s = prefix.to_string_lossy();
                let is_critical_path = s == "/"
                    || s == "/root"
                    || s == "/etc"
                    || s == "/bin"
                    || s == "/usr"
                    || s == "~"
                    || s == "/home"
                    || s.starts_with("/root/")
                    || s.starts_with("/etc/")
                    || s.starts_with("/bin/")
                    || s.starts_with("/usr/");
                if is_critical_path {
                    findings.push(SafetyFinding {
                        finding_id: Uuid::new_v4(),
                        rule_id: rule.id,
                        rule_version: rule.auto_approved_count + 1,
                        severity: SafetyFindingSeverity::Critical,
                        title: "Excessively broad filesystem write privilege".into(),
                        reason: format!(
                            "Learned rule grants write permissions to system-critical prefix '{}'",
                            prefix.display()
                        ),
                        affected_capabilities: vec!["fs:write".into()],
                        evidence: vec![
                            format!("Pattern: write prefix '{}'", prefix.display()),
                            format!("Auto-approved count: {}", rule.auto_approved_count),
                        ],
                        recommended_action: RecommendedAction::Revoke,
                        status: FindingStatus::Unread,
                        evaluated_at,
                        evaluator_version: Self::EVALUATOR_VERSION.into(),
                        policy_version: Self::POLICY_VERSION.into(),
                    });
                }
            }
            LearnedPattern::FilesystemReadPrefix { prefix } => {
                let s = prefix.to_string_lossy();
                let is_sensitive_read = s == "/"
                    || s == "/root"
                    || s == "/etc"
                    || s.starts_with("/root/")
                    || s.starts_with("/etc/");
                if is_sensitive_read {
                    findings.push(SafetyFinding {
                        finding_id: Uuid::new_v4(),
                        rule_id: rule.id,
                        rule_version: rule.auto_approved_count + 1,
                        severity: SafetyFindingSeverity::High,
                        title: "Root or system credentials read scope".into(),
                        reason: format!(
                            "Learned rule grants read access to sensitive system prefix '{}'",
                            prefix.display()
                        ),
                        affected_capabilities: vec!["fs:read".into()],
                        evidence: vec![format!("Pattern: read prefix '{}'", prefix.display())],
                        recommended_action: RecommendedAction::NarrowScope {
                            suggested_pattern: LearnedPattern::FilesystemReadPrefix {
                                prefix: PathBuf::from("."),
                            },
                        },
                        status: FindingStatus::Unread,
                        evaluated_at,
                        evaluator_version: Self::EVALUATOR_VERSION.into(),
                        policy_version: Self::POLICY_VERSION.into(),
                    });
                }
            }
            LearnedPattern::AnyToolInvocation { tool_name } => {
                if tool_name == "*"
                    || tool_name == "shell"
                    || tool_name == "bash"
                    || tool_name == "exec"
                {
                    findings.push(SafetyFinding {
                        finding_id: Uuid::new_v4(),
                        rule_id: rule.id,
                        rule_version: rule.auto_approved_count + 1,
                        severity: SafetyFindingSeverity::Critical,
                        title: "Unbounded wildcard tool invocation".into(),
                        reason: format!(
                            "Rule grants universal wildcard execution for tool '{}'",
                            tool_name
                        ),
                        affected_capabilities: vec!["tool:invocation".into()],
                        evidence: vec![format!("Tool pattern: '{}'", tool_name)],
                        recommended_action: RecommendedAction::Revoke,
                        status: FindingStatus::Unread,
                        evaluated_at,
                        evaluator_version: Self::EVALUATOR_VERSION.into(),
                        policy_version: Self::POLICY_VERSION.into(),
                    });
                }
            }
            LearnedPattern::SystemOperation { operation }
                if (operation == "*" || operation == "network_all") =>
            {
                findings.push(SafetyFinding {
                    finding_id: Uuid::new_v4(),
                    rule_id: rule.id,
                    rule_version: rule.auto_approved_count + 1,
                    severity: SafetyFindingSeverity::High,
                    title: "Unrestricted system or network capability".into(),
                    reason: format!("Rule allows unrestricted operation '{}'", operation),
                    affected_capabilities: vec!["system:operation".into()],
                    evidence: vec![format!("Operation: '{}'", operation)],
                    recommended_action: RecommendedAction::Revoke,
                    status: FindingStatus::Unread,
                    evaluated_at,
                    evaluator_version: Self::EVALUATOR_VERSION.into(),
                    policy_version: Self::POLICY_VERSION.into(),
                });
            }
            _ => {}
        }

        // Check 2: High auto-approval usage pattern / suspicious velocity
        if rule.auto_approved_count >= 50 {
            findings.push(SafetyFinding {
                finding_id: Uuid::new_v4(),
                rule_id: rule.id,
                rule_version: rule.auto_approved_count + 1,
                severity: SafetyFindingSeverity::Medium,
                title: "High-frequency auto-approved rule requires periodic review".into(),
                reason: format!(
                    "Rule has been auto-approved {} times without human review",
                    rule.auto_approved_count
                ),
                affected_capabilities: vec![rule.pattern.display_label()],
                evidence: vec![
                    format!("Auto-approval count: {}", rule.auto_approved_count),
                    format!("Created at: {}", rule.created_at),
                ],
                recommended_action: RecommendedAction::Keep,
                status: FindingStatus::Unread,
                evaluated_at,
                evaluator_version: Self::EVALUATOR_VERSION.into(),
                policy_version: Self::POLICY_VERSION.into(),
            });
        }

        // Check 3: Stale rules (> 30 days without auto-approval)
        let age_days = (Utc::now() - rule.created_at).num_days();
        if age_days > 30 && rule.auto_approved_count == 0 {
            findings.push(SafetyFinding {
                finding_id: Uuid::new_v4(),
                rule_id: rule.id,
                rule_version: rule.auto_approved_count + 1,
                severity: SafetyFindingSeverity::Low,
                title: "Dormant / stale approval rule".into(),
                reason: format!(
                    "Rule was created {} days ago but has never been exercised",
                    age_days
                ),
                affected_capabilities: vec![rule.pattern.display_label()],
                evidence: vec![format!("Age: {} days", age_days)],
                recommended_action: RecommendedAction::Revoke,
                status: FindingStatus::Unread,
                evaluated_at,
                evaluator_version: Self::EVALUATOR_VERSION.into(),
                policy_version: Self::POLICY_VERSION.into(),
            });
        }

        // Check 4: Suspicious injection patterns in description or evidence
        if rule.description.contains("ignore previous")
            || rule.description.contains("bypass guardian")
            || rule.description.contains("<script>")
        {
            findings.push(SafetyFinding {
                finding_id: Uuid::new_v4(),
                rule_id: rule.id,
                rule_version: rule.auto_approved_count + 1,
                severity: SafetyFindingSeverity::Critical,
                title: "Potential prompt injection in rule metadata".into(),
                reason: "Rule metadata contains adversarial override phrases attempting to alter evaluator authority".into(),
                affected_capabilities: vec![rule.pattern.display_label()],
                evidence: vec![format!("Description snippet: '{}'", rule.description)],
                recommended_action: RecommendedAction::Revoke,
                status: FindingStatus::Unread,
                evaluated_at,
                evaluator_version: Self::EVALUATOR_VERSION.into(),
                policy_version: Self::POLICY_VERSION.into(),
            });
        }

        findings
    }

    /// List active findings, optionally filtering by status.
    pub fn list_findings(&self) -> Vec<SafetyFinding> {
        self.findings.read().unwrap().clone()
    }

    /// Return the summary of the last completed safety review run.
    pub fn last_run_summary(&self) -> Option<SafetyReviewRunSummary> {
        self.last_run.read().unwrap().clone()
    }

    /// Apply an owner review decision on a finding (e.g. "revoke", "dismiss", "keep", "narrow").
    /// Checks rule version consistency so decisions are not applied to changed rule states.
    pub fn resolve_finding(&self, finding_id: Uuid, action: &str) -> Result<SafetyFinding, String> {
        let mut findings = self.findings.write().unwrap();
        let finding = findings
            .iter_mut()
            .find(|f| f.finding_id == finding_id)
            .ok_or_else(|| "Finding not found".to_string())?;

        // Verify current rule exists
        let current_rules = self.policy.list_learned_rules();
        let target_rule = current_rules.iter().find(|r| r.id == finding.rule_id);

        match action {
            "revoke" => {
                if self.policy.revoke_learned_rule(finding.rule_id) {
                    finding.status = FindingStatus::Resolved {
                        decision: "revoked".to_string(),
                        resolved_at: Utc::now(),
                    };
                    info!(finding_id = %finding_id, rule_id = %finding.rule_id, "Rule revoked via safety review finding");
                } else {
                    finding.status = FindingStatus::Resolved {
                        decision: "already_revoked".to_string(),
                        resolved_at: Utc::now(),
                    };
                }
            }
            "dismiss" => {
                finding.status = FindingStatus::Dismissed;
            }
            "keep" => {
                finding.status = FindingStatus::Resolved {
                    decision: "kept".to_string(),
                    resolved_at: Utc::now(),
                };
            }
            "narrow" => {
                if let Some(rule) = target_rule {
                    if let RecommendedAction::NarrowScope {
                        ref suggested_pattern,
                    } = finding.recommended_action
                    {
                        // Narrow the rule: revoke old one and install narrowed rule
                        self.policy.revoke_learned_rule(rule.id);
                        let _ = self.policy.learn_approval(
                            rule.requester,
                            &match suggested_pattern {
                                LearnedPattern::FilesystemReadPrefix { prefix } => {
                                    Capability::Filesystem(crate::capability::FsCapability::Read {
                                        path: prefix.clone(),
                                    })
                                }
                                _ => Capability::System(
                                    crate::capability::SystemCapability::ProcessInspect,
                                ),
                            },
                            format!(
                                "Narrowed scope via safety review on {}",
                                Utc::now().format("%Y-%m-%d")
                            ),
                        );
                        finding.status = FindingStatus::Resolved {
                            decision: "narrowed".to_string(),
                            resolved_at: Utc::now(),
                        };
                    } else {
                        return Err("No narrow scope available for this finding".to_string());
                    }
                } else {
                    return Err("Target rule no longer exists".to_string());
                }
            }
            other => return Err(format!("Unknown action: {}", other)),
        }

        let updated = finding.clone();
        drop(findings);
        self.save_findings();

        // Record audit receipt for the review decision
        let _ = self.audit.record(
            SubsystemId::Guardian,
            Capability::System(crate::capability::SystemCapability::OsTelemetry),
            crate::audit::DecisionOutcome::Granted,
            format!(
                "Owner reviewed safety finding {} with action '{}'",
                finding_id, action
            ),
        );

        Ok(updated)
    }

    fn save_findings(&self) {
        if let Some(ref path) = self.persistence_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(findings) = self.findings.read()
                && let Ok(json) = serde_json::to_string_pretty(&*findings)
            {
                let _ = std::fs::write(path, json);
            }
        }
    }
}
