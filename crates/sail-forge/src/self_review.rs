//! Tier-0 review: the local model critiques a gated patch against the issue's
//! acceptance criteria before any external reviewer or PR publication.
//!
//! An unreachable model or an unparseable answer is reported as *not passed* with
//! the reason recorded; this module never synthesizes an approval.

use async_trait::async_trait;
use std::time::Duration;

use crate::contracts::{ReviewVerdict, TestExecutionRecord};
use crate::reviewer::ReviewError;
use crate::solver::LocalSolverController;

/// Hard cap on the packet sent to any reviewer (~8k tokens).
pub const MAX_PACKET_CHARS: usize = 32_000;

/// Compact, bounded review input: acceptance criteria, test summary and diff.
#[derive(Debug, Clone)]
pub struct ReviewPacket {
    pub issue_id: String,
    pub acceptance: String,
    pub test_summary: String,
    pub diff: String,
    pub diff_truncated: bool,
}

/// Extract the `## Acceptance...` section of an issue body, or the whole body if absent.
fn acceptance_section(body: &str) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    for line in body.lines() {
        if let Some(h) = line.trim().strip_prefix("## ") {
            inside = h.trim().to_ascii_lowercase().starts_with("acceptance");
            continue;
        }
        if inside {
            out.push(line);
        }
    }
    if out.is_empty() {
        body.trim().to_string()
    } else {
        out.join("\n").trim().to_string()
    }
}

fn truncate_chars(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        (s.to_string(), false)
    } else {
        (s.chars().take(max).collect(), true)
    }
}

impl ReviewPacket {
    pub fn build(
        issue_id: &str,
        issue_body: &str,
        diff: &str,
        tests: &[TestExecutionRecord],
    ) -> Self {
        let (acceptance, _) = truncate_chars(&acceptance_section(issue_body), 4_000);
        let summary_lines: Vec<String> = tests
            .iter()
            .map(|t| {
                format!(
                    "- `{}`: {} (exit {}, {}ms)",
                    t.command, t.status, t.exit_code, t.duration_ms
                )
            })
            .collect();
        let (test_summary, _) = truncate_chars(&summary_lines.join("\n"), 2_000);

        // The diff gets whatever budget remains; it is truncated last.
        let overhead = issue_id.len() + acceptance.len() + test_summary.len() + 512;
        let (diff, diff_truncated) =
            truncate_chars(diff, MAX_PACKET_CHARS.saturating_sub(overhead));

        Self {
            issue_id: issue_id.to_string(),
            acceptance,
            test_summary,
            diff,
            diff_truncated,
        }
    }

    pub fn render(&self) -> String {
        format!(
            "Issue: {}\n\n## Acceptance criteria\n{}\n\n## Test results\n{}\n\n## Unified diff{}\n{}\n",
            self.issue_id,
            self.acceptance,
            self.test_summary,
            if self.diff_truncated {
                " (truncated)"
            } else {
                ""
            },
            self.diff
        )
    }
}

/// A reviewer that judges a packet. External CLIs (Codex, Claude, Gemini) can implement
/// this later; the local model is the only implementation today.
#[async_trait]
pub trait ReviewProvider: Send + Sync {
    fn name(&self) -> String;
    async fn review(&self, packet: &ReviewPacket) -> Result<ReviewVerdict, ReviewError>;
}

fn rejected(reason: String, raw: &str) -> ReviewVerdict {
    ReviewVerdict {
        passed: false,
        requirements_covered: false,
        untested_edge_cases: vec![],
        detected_bugs: vec![reason],
        security_observations: vec![],
        suggested_edits: vec![],
        confidence_score: 0.0,
        raw_feedback: raw.to_string(),
    }
}

fn string_list(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a model answer into a verdict. Accepts a bare JSON object or one embedded in
/// prose/markdown fences. Anything unparseable is a rejection.
pub fn parse_verdict(raw: &str) -> ReviewVerdict {
    let json = match (raw.find('{'), raw.rfind('}')) {
        (Some(start), Some(end)) if end > start => {
            serde_json::from_str::<serde_json::Value>(&raw[start..=end]).ok()
        }
        _ => None,
    };
    let Some(v) = json else {
        return rejected("self-review answer was not valid JSON".into(), raw);
    };
    let Some(passed) = v.get("passed").and_then(|p| p.as_bool()) else {
        return rejected("self-review answer lacked a boolean `passed`".into(), raw);
    };
    let requirements_covered = v
        .get("requirements_covered")
        .and_then(|p| p.as_bool())
        .unwrap_or(false);
    let detected_bugs = string_list(&v, "detected_bugs");
    let confidence_score = v
        .get("confidence")
        .and_then(|c| c.as_f64())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0) as f32;

    ReviewVerdict {
        // A pass must also claim the requirements are covered and list no bugs.
        passed: passed && requirements_covered && detected_bugs.is_empty(),
        requirements_covered,
        untested_edge_cases: string_list(&v, "untested_edge_cases"),
        detected_bugs,
        security_observations: vec![],
        suggested_edits: string_list(&v, "suggested_edits"),
        confidence_score,
        raw_feedback: raw.to_string(),
    }
}

const SYSTEM_PROMPT: &str = "You are a strict code reviewer for a Rust project. Decide whether the diff fully satisfies every acceptance criterion without introducing bugs. Respond with ONLY a JSON object: {\"passed\": bool, \"requirements_covered\": bool, \"detected_bugs\": [string], \"untested_edge_cases\": [string], \"suggested_edits\": [string], \"confidence\": number between 0 and 1}. If unsure, set passed to false.";

/// Self-review using the same local OpenAI-compatible endpoint as the solver.
pub struct LocalModelReviewer {
    endpoints: Vec<String>,
    model: String,
    timeout: Duration,
}

impl LocalModelReviewer {
    pub fn new() -> Self {
        Self {
            endpoints: LocalSolverController::candidate_endpoints(),
            model: std::env::var("SAIL_FORGE_MODEL").unwrap_or_else(|_| "local".into()),
            timeout: Duration::from_secs(300),
        }
    }

    pub fn with_endpoints(mut self, endpoints: Vec<String>) -> Self {
        self.endpoints = endpoints;
        self
    }
}

impl Default for LocalModelReviewer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ReviewProvider for LocalModelReviewer {
    fn name(&self) -> String {
        "local-self-review".into()
    }

    async fn review(&self, packet: &ReviewPacket) -> Result<ReviewVerdict, ReviewError> {
        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| ReviewError::ProviderError(e.to_string()))?;
        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": packet.render()},
            ],
            "temperature": 0.0,
            "max_tokens": 1024,
            "stream": false,
        });

        let mut last_err = String::from("no local model endpoints configured");
        for ep in &self.endpoints {
            match client.post(ep).json(&body).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let v: serde_json::Value = resp
                        .json()
                        .await
                        .map_err(|e| ReviewError::InvalidResponse(e.to_string()))?;
                    let content = v
                        .pointer("/choices/0/message/content")
                        .and_then(|c| c.as_str())
                        .unwrap_or_default();
                    return Ok(parse_verdict(content));
                }
                Ok(resp) => last_err = format!("{ep} returned HTTP {}", resp.status()),
                Err(e) => last_err = format!("{ep}: {e}"),
            }
        }
        Err(ReviewError::ProviderError(last_err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> TestExecutionRecord {
        TestExecutionRecord {
            command: "cargo test".into(),
            status: "passed".into(),
            duration_ms: 1200,
            log_ref: String::new(),
            exit_code: 0,
            stdout_log_path: None,
            stderr_log_path: None,
        }
    }

    #[test]
    fn packet_extracts_acceptance_and_caps_size() {
        let body = "## Problem\nlong story\n## Acceptance\n- [ ] prints summary\n## Hints\nnone";
        let huge_diff = "+x\n".repeat(50_000);
        let p = ReviewPacket::build("o/r#1", body, &huge_diff, &[record()]);
        assert_eq!(p.acceptance, "- [ ] prints summary");
        assert!(p.test_summary.contains("cargo test"));
        assert!(p.diff_truncated);
        assert!(p.render().chars().count() <= MAX_PACKET_CHARS);
    }

    #[test]
    fn small_packet_is_not_truncated() {
        let p = ReviewPacket::build("o/r#1", "## Acceptance\n- [ ] a", "+x\n", &[]);
        assert!(!p.diff_truncated);
        assert!(p.render().contains("+x"));
    }

    #[test]
    fn parses_fenced_json_pass() {
        let raw = "Sure:\n```json\n{\"passed\": true, \"requirements_covered\": true, \"detected_bugs\": [], \"confidence\": 0.9}\n```";
        let v = parse_verdict(raw);
        assert!(v.passed);
        assert!((v.confidence_score - 0.9).abs() < 1e-6);
    }

    #[test]
    fn pass_with_bugs_or_uncovered_requirements_is_rejected() {
        let v = parse_verdict(
            r#"{"passed": true, "requirements_covered": true, "detected_bugs": ["off by one"]}"#,
        );
        assert!(!v.passed);
        let v = parse_verdict(r#"{"passed": true, "requirements_covered": false}"#);
        assert!(!v.passed);
    }

    #[test]
    fn junk_is_rejected() {
        assert!(!parse_verdict("looks good to me").passed);
        assert!(!parse_verdict("{\"verdict\": \"ok\"}").passed);
    }

    #[tokio::test]
    async fn unreachable_model_is_an_error_not_an_approval() {
        let r = LocalModelReviewer::new()
            .with_endpoints(vec!["http://127.0.0.1:9/v1/chat/completions".into()]);
        let packet = ReviewPacket::build("o/r#1", "", "+x", &[]);
        assert!(r.review(&packet).await.is_err());
    }
}
