//! Deterministic issue admission for autonomous Forge runs.
//!
//! Only issues explicitly marked `forge-ready`, with a machine-checkable acceptance
//! checklist and no blocking labels or open PRs, are selected for unattended work.
//! User-initiated enqueues (e.g. the `forge:enqueue` dialogue tool) bypass this filter.

use serde::{Deserialize, Serialize};

/// Minimal view of a GitHub issue needed for admission.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueCandidate {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub has_open_pr: bool,
}

/// Label and content rules for autonomous selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmissionPolicy {
    pub required_label: String,
    pub blocking_labels: Vec<String>,
    pub require_acceptance_checklist: bool,
}

impl Default for AdmissionPolicy {
    fn default() -> Self {
        Self {
            required_label: "forge-ready".into(),
            blocking_labels: vec![
                "blocked".into(),
                "needs-decision".into(),
                "needs-decomposition".into(),
                "forge-failed".into(),
            ],
            require_acceptance_checklist: true,
        }
    }
}

/// Result of evaluating one candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdmissionDecision {
    Ready { target_files: Vec<String> },
    Rejected { reasons: Vec<String> },
}

/// Find the lines of a `## <heading>` markdown section (case-insensitive prefix match).
fn section_lines<'a>(body: &'a str, heading: &str) -> Vec<&'a str> {
    let heading = heading.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut inside = false;
    for line in body.lines() {
        let t = line.trim();
        if let Some(h) = t.strip_prefix("## ") {
            inside = h.trim().to_ascii_lowercase().starts_with(&heading);
            continue;
        }
        if inside {
            out.push(t);
        }
    }
    out
}

/// Parse the `## Target files` section into repo-relative paths. Entries are bullet
/// items, optionally wrapped in backticks; a trailing `/` denotes a directory.
pub fn parse_target_files(body: &str) -> Vec<String> {
    section_lines(body, "target files")
        .into_iter()
        .filter_map(|l| l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")))
        .filter_map(|item| {
            let path = item
                .split_whitespace()
                .next()?
                .trim_matches('`')
                .trim_start_matches("./");
            (!path.is_empty()).then(|| path.to_string())
        })
        .collect()
}

fn has_acceptance_checklist(body: &str) -> bool {
    section_lines(body, "acceptance")
        .iter()
        .any(|l| l.starts_with("- [ ]") || l.starts_with("- [x]"))
}

impl AdmissionPolicy {
    pub fn evaluate(&self, issue: &IssueCandidate) -> AdmissionDecision {
        let mut reasons = Vec::new();
        if !issue.labels.iter().any(|l| l == &self.required_label) {
            reasons.push(format!("missing `{}` label", self.required_label));
        }
        for label in issue
            .labels
            .iter()
            .filter(|l| self.blocking_labels.contains(l))
        {
            reasons.push(format!("has blocking label `{label}`"));
        }
        if issue.has_open_pr {
            reasons.push("an open pull request already references this issue".into());
        }
        if self.require_acceptance_checklist && !has_acceptance_checklist(&issue.body) {
            reasons.push("no `## Acceptance` checklist".into());
        }

        if reasons.is_empty() {
            AdmissionDecision::Ready {
                target_files: parse_target_files(&issue.body),
            }
        } else {
            AdmissionDecision::Rejected { reasons }
        }
    }

    /// Partition candidates into ready issues and rejections, preserving input order.
    pub fn select<'a>(
        &self,
        issues: &'a [IssueCandidate],
    ) -> (Vec<&'a IssueCandidate>, Vec<(u64, Vec<String>)>) {
        let mut ready = Vec::new();
        let mut rejected = Vec::new();
        for issue in issues {
            match self.evaluate(issue) {
                AdmissionDecision::Ready { .. } => ready.push(issue),
                AdmissionDecision::Rejected { reasons } => rejected.push((issue.number, reasons)),
            }
        }
        (ready, rejected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const READY_BODY: &str = "## Problem\nFlaky.\n\n## Target files\n- `crates/sail-forge/src/solver.rs`\n- crates/sail-forge/tests/ (new test)\n\n## Acceptance\n- [ ] summary printed\n";

    fn issue(number: u64, labels: &[&str], body: &str) -> IssueCandidate {
        IssueCandidate {
            number,
            title: format!("Issue {number}"),
            body: body.into(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            has_open_pr: false,
        }
    }

    #[test]
    fn ready_issue_is_admitted_with_targets() {
        let decision = AdmissionPolicy::default().evaluate(&issue(1, &["forge-ready"], READY_BODY));
        assert_eq!(
            decision,
            AdmissionDecision::Ready {
                target_files: vec![
                    "crates/sail-forge/src/solver.rs".into(),
                    "crates/sail-forge/tests/".into()
                ]
            }
        );
    }

    #[test]
    fn rejects_with_reasons() {
        let policy = AdmissionPolicy::default();
        let mut with_pr = issue(4, &["forge-ready"], READY_BODY);
        with_pr.has_open_pr = true;
        let issues = vec![
            issue(1, &["forge-ready"], READY_BODY),
            issue(2, &[], READY_BODY),
            issue(3, &["forge-ready", "blocked"], READY_BODY),
            with_pr,
            issue(5, &["forge-ready"], "## Acceptance\nworks somehow"),
        ];
        let (ready, rejected) = policy.select(&issues);
        assert_eq!(ready.iter().map(|i| i.number).collect::<Vec<_>>(), vec![1]);
        let ids: Vec<u64> = rejected.iter().map(|(n, _)| *n).collect();
        assert_eq!(ids, vec![2, 3, 4, 5]);
        assert!(rejected[0].1[0].contains("forge-ready"));
        assert!(rejected[1].1[0].contains("blocked"));
        assert!(rejected[2].1[0].contains("pull request"));
        assert!(rejected[3].1[0].contains("Acceptance"));
    }

    #[test]
    fn decisions_are_deterministic() {
        let policy = AdmissionPolicy::default();
        let i = issue(7, &["forge-ready"], READY_BODY);
        assert_eq!(policy.evaluate(&i), policy.evaluate(&i));
    }
}
