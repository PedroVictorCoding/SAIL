//! Deterministic pre-review gates for Forge patches.
//!
//! These checks run after the local solver's tests pass and before any external
//! reviewer is invoked, so patches that violate cheap, machine-checkable rules never
//! spend review quota.

use serde::{Deserialize, Serialize};

/// Policy for the deterministic diff gates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffGatePolicy {
    /// Maximum number of added plus removed lines across the whole diff.
    pub max_changed_lines: usize,
    /// Path prefixes Forge may never modify autonomously (Guardian, Forge authority, CI, deploy).
    pub forbidden_path_prefixes: Vec<String>,
    /// Reject diffs that delete test functions or add `#[ignore]`.
    pub reject_test_weakening: bool,
    /// Reject new `.unwrap()` calls in non-test code (`.lock().unwrap()` is tolerated).
    pub reject_new_unwrap: bool,
}

impl Default for DiffGatePolicy {
    fn default() -> Self {
        Self {
            max_changed_lines: 400,
            forbidden_path_prefixes: vec![
                ".github/".into(),
                "deploy/".into(),
                "crates/sail-guardian/".into(),
                "crates/sail-forge/src/gates.rs".into(),
                "crates/sail-forge/src/admission.rs".into(),
                "crates/sail-forge/src/reviewer.rs".into(),
                "AGENTS.md".into(),
                "CLAUDE.md".into(),
                "GEMINI.md".into(),
            ],
            reject_test_weakening: true,
            reject_new_unwrap: true,
        }
    }
}

/// A single failed gate with a human-readable reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateViolation {
    pub rule: String,
    pub detail: String,
}

impl GateViolation {
    fn new(rule: &str, detail: String) -> Self {
        Self {
            rule: rule.into(),
            detail,
        }
    }
}

impl std::fmt::Display for GateViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.rule, self.detail)
    }
}

fn is_test_path(path: &str) -> bool {
    path.contains("/tests/") || path.starts_with("tests/") || path.ends_with("_test.rs")
}

fn is_test_context(hunk_context: &str) -> bool {
    hunk_context.contains("mod tests")
        || hunk_context.contains("fn test_")
        || hunk_context.contains("cfg(test)")
}

fn path_allowed(path: &str, allowed_paths: &[String]) -> bool {
    allowed_paths.is_empty()
        || is_test_path(path)
        || allowed_paths
            .iter()
            .any(|a| path == a || (a.ends_with('/') && path.starts_with(a.as_str())))
}

impl DiffGatePolicy {
    /// Evaluate a unified diff. `allowed_paths` comes from the issue's target-file
    /// hypothesis; when empty, any non-forbidden path is accepted.
    pub fn evaluate(&self, diff: &str, allowed_paths: &[String]) -> Vec<GateViolation> {
        let mut violations = Vec::new();
        let mut changed_lines = 0usize;
        let mut current_file = String::new();
        let mut hunk_context = String::new();
        let mut files: Vec<String> = Vec::new();

        for line in diff.lines() {
            if let Some(p) = line.strip_prefix("--- a/") {
                current_file = p.trim().to_string();
                continue;
            }
            if let Some(p) = line.strip_prefix("+++ b/") {
                current_file = p.trim().to_string();
                continue;
            }
            if line.starts_with("+++") || line.starts_with("---") {
                continue;
            }
            if let Some(rest) = line.strip_prefix("@@") {
                hunk_context = rest.to_string();
                if !current_file.is_empty() && !files.contains(&current_file) {
                    files.push(current_file.clone());
                }
                continue;
            }

            let in_test = is_test_path(&current_file) || is_test_context(&hunk_context);
            if let Some(added) = line.strip_prefix('+') {
                changed_lines += 1;
                let t = added.trim();
                if self.reject_test_weakening && t.starts_with("#[ignore") {
                    violations.push(GateViolation::new(
                        "test_weakening",
                        format!("{current_file}: adds `{t}`"),
                    ));
                }
                if self.reject_new_unwrap
                    && !in_test
                    && t.contains(".unwrap()")
                    && !t.contains(".lock().unwrap()")
                    && !t.starts_with("//")
                {
                    violations.push(GateViolation::new(
                        "new_unwrap",
                        format!("{current_file}: `{t}`"),
                    ));
                }
            } else if let Some(removed) = line.strip_prefix('-') {
                changed_lines += 1;
                let t = removed.trim();
                if self.reject_test_weakening
                    && (t.starts_with("#[test]") || t.starts_with("#[tokio::test"))
                {
                    violations.push(GateViolation::new(
                        "test_weakening",
                        format!("{current_file}: removes a test (`{t}`)"),
                    ));
                }
            }
        }

        if changed_lines > self.max_changed_lines {
            violations.push(GateViolation::new(
                "diff_too_large",
                format!(
                    "{changed_lines} changed lines exceeds limit of {}",
                    self.max_changed_lines
                ),
            ));
        }

        for file in &files {
            if let Some(prefix) = self
                .forbidden_path_prefixes
                .iter()
                .find(|p| file.starts_with(p.as_str()))
            {
                violations.push(GateViolation::new(
                    "forbidden_path",
                    format!("{file} is under protected path `{prefix}`"),
                ));
            } else if !path_allowed(file, allowed_paths) {
                violations.push(GateViolation::new(
                    "outside_target_files",
                    format!("{file} is not in the issue's target files {allowed_paths:?}"),
                ));
            }
        }

        violations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff(path: &str, hunk_ctx: &str, body: &str) -> String {
        format!("--- a/{path}\n+++ b/{path}\n@@ -1,3 +1,3 @@{hunk_ctx}\n{body}\n")
    }

    #[test]
    fn clean_diff_passes() {
        let d = diff(
            "crates/sail-mind/src/lib.rs",
            " fn ok()",
            "-    let x = 1;\n+    let x = 2;",
        );
        assert!(DiffGatePolicy::default().evaluate(&d, &[]).is_empty());
    }

    #[test]
    fn forbidden_path_is_rejected() {
        let d = diff(".github/workflows/rust-ci.yml", "", "-a\n+b");
        let v = DiffGatePolicy::default().evaluate(&d, &[]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "forbidden_path");
    }

    #[test]
    fn oversized_diff_is_rejected() {
        let body = (0..500).map(|i| format!("+line {i}")).collect::<Vec<_>>();
        let d = diff("src/lib.rs", "", &body.join("\n"));
        let v = DiffGatePolicy::default().evaluate(&d, &[]);
        assert!(v.iter().any(|g| g.rule == "diff_too_large"));
    }

    #[test]
    fn test_removal_and_ignore_are_rejected() {
        let d = diff("src/lib.rs", " mod tests", "-    #[test]\n+    #[ignore]");
        let rules: Vec<_> = DiffGatePolicy::default()
            .evaluate(&d, &[])
            .into_iter()
            .map(|g| g.rule)
            .collect();
        assert_eq!(rules, vec!["test_weakening", "test_weakening"]);
    }

    #[test]
    fn unwrap_rejected_only_outside_tests() {
        let policy = DiffGatePolicy::default();
        let prod = diff(
            "src/lib.rs",
            " fn parse()",
            "+    let v = s.parse::<u32>().unwrap();",
        );
        assert_eq!(policy.evaluate(&prod, &[])[0].rule, "new_unwrap");

        let in_test_mod = diff("src/lib.rs", " mod tests", "+    let v = f().unwrap();");
        assert!(policy.evaluate(&in_test_mod, &[]).is_empty());

        let test_file = diff("crates/x/tests/a.rs", "", "+    let v = f().unwrap();");
        assert!(policy.evaluate(&test_file, &[]).is_empty());

        let lock = diff("src/lib.rs", "", "+    let g = m.lock().unwrap();");
        assert!(policy.evaluate(&lock, &[]).is_empty());
    }

    #[test]
    fn files_outside_targets_are_rejected() {
        let policy = DiffGatePolicy::default();
        let targets = vec!["crates/sail-forge/src/solver.rs".to_string()];
        let other = diff("crates/sail-mind/src/lib.rs", "", "+x");
        assert_eq!(
            policy.evaluate(&other, &targets)[0].rule,
            "outside_target_files"
        );

        let target = diff("crates/sail-forge/src/solver.rs", "", "+x");
        assert!(policy.evaluate(&target, &targets).is_empty());

        let dir_targets = vec!["crates/sail-mind/".to_string()];
        assert!(policy.evaluate(&other, &dir_targets).is_empty());
    }
}
