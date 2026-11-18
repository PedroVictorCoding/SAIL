//! K4: build a [`NightResult`] from a Forge job report and summarize it.
//!
//! The Night Shift runner hands the patch text to this module through
//! `ForgeJobReport::patch_ref` (the existing Forge field normally holds
//! `"patches/<job>.patch"`); a real path or a diff is also accepted. Nothing here calls a
//! model: files, tests, gates and the outcome come from the report and the patch.
//!
//! The K4 contract signature has no checkpoint argument. This is the interpretation
//! recorded as a question in the Log; it keeps the signature unchanged and lets K5 pass
//! the checkpoint's patch.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};

use crate::contracts::ForgeJobReport;
use crate::gates::DiffGatePolicy;
use crate::nightshift::{FileChange, GateResult, ReviewSummary, TestSummary};

use super::{MorningOutcome, NightResult, RanOn};

/// Review questions shown to the owner are capped at 500 characters (fixed a priori to
/// keep the morning card readable). The full concerns stay in `review.concerns`.
const MAX_QUESTION_CHARS: usize = 500;

/// Extract the reviewable result from a Forge report. The patch is returned separately so
/// the caller can store it in the errand store. Not pure: it may read a test log or a
/// patch file for the real counts.
pub fn result_from_report(
    report: &ForgeJobReport,
    ran_on: RanOn,
    now: DateTime<Utc>,
) -> (NightResult, MorningOutcome, Option<String>, String) {
    let patch = patch_text(&report.patch_ref);
    let mut result = NightResult {
        job_id: job_id(report),
        base_commit: report.base_commit.clone(),
        ran_on,
        files: file_changes(&patch),
        tests: test_summary(&report.tests),
        gates: gate_results(&patch),
        review: review_summary(report),
        summary: String::new(),
        model_note: None,
        patch_bytes: patch.len() as u64,
        finished_at: now,
    };
    result.summary = summary(&result);

    let tests_ok = result
        .tests
        .as_ref()
        .is_some_and(|tests| tests.exit_ok && tests.failed == 0);
    let gates_ok = result.gates.iter().all(|gate| gate.passed);
    if !tests_ok {
        let reason = match &result.tests {
            Some(tests) if tests.failed > 0 => format!("{} tests failed", tests.failed),
            Some(_) => "the real test run failed".to_string(),
            None => "tests never ran".to_string(),
        };
        return (result, MorningOutcome::Failed, Some(reason), patch);
    }
    if !gates_ok {
        let failed: Vec<&GateResult> = result.gates.iter().filter(|g| !g.passed).collect();
        let reason = failed
            .first()
            .map(|gate| format!("the {} check failed: {}", gate.name, gate.detail))
            .unwrap_or_else(|| "a blocking check failed".into());
        return (result, MorningOutcome::Failed, Some(reason), patch);
    }

    let review_rejected = report
        .review_result
        .as_ref()
        .is_some_and(|review| !review.verdict.passed || !review.verdict.requirements_covered);
    if review_rejected {
        return (
            result.clone(),
            MorningOutcome::NeedsYou,
            Some(review_question(report)),
            patch,
        );
    }
    if !report.ready_to_publish {
        return (
            result,
            MorningOutcome::Failed,
            Some("Forge did not mark the result verified".into()),
            patch,
        );
    }
    (result, MorningOutcome::Ready, None, patch)
}

/// Take the patch text from the report. A diff is used as-is; a relative, non-traversing
/// path is read; anything else is empty (the caller reports the missing patch).
fn patch_text(patch_ref: &str) -> String {
    let trimmed = patch_ref.trim();
    if trimmed.starts_with("diff --git ") || trimmed.starts_with("--- a/") {
        return patch_ref.to_string();
    }
    if trimmed.is_empty() || trimmed.contains("..") {
        return String::new();
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return String::new();
    }
    std::fs::read_to_string(path).unwrap_or_default()
}

fn job_id(report: &ForgeJobReport) -> String {
    let trimmed = report.patch_ref.trim();
    if let Some(id) = trimmed
        .strip_prefix("patches/")
        .and_then(|rest| rest.strip_suffix(".patch"))
    {
        if !id.is_empty() {
            return id.to_string();
        }
    }
    report.issue.clone()
}

fn file_changes(patch: &str) -> Vec<FileChange> {
    let mut changes: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut pending_minus: Option<String> = None;
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            pending_minus = None;
            current = rest
                .split_whitespace()
                .find_map(|part| part.strip_prefix("b/"))
                .map(str::to_string);
            if let Some(path) = &current {
                changes.entry(path.clone()).or_insert((0, 0));
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            let path = rest
                .strip_prefix("a/")
                .unwrap_or(rest)
                .split('\t')
                .next()
                .unwrap_or(rest);
            if path != "/dev/null" && !path.is_empty() {
                pending_minus = Some(path.to_string());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            let path = rest
                .strip_prefix("b/")
                .unwrap_or(rest)
                .split('\t')
                .next()
                .unwrap_or(rest);
            if path == "/dev/null" {
                current = pending_minus.take();
            } else if !path.is_empty() {
                current = Some(path.to_string());
            }
            if let Some(path) = &current {
                changes.entry(path.clone()).or_insert((0, 0));
            }
            continue;
        }
        if line.starts_with("@@") || line.starts_with("diff ") {
            continue;
        }
        if let Some(path) = &current {
            let entry = changes.entry(path.clone()).or_insert((0, 0));
            if line.starts_with('+') && !line.starts_with("+++") {
                entry.0 = entry.0.saturating_add(1);
            } else if line.starts_with('-') && !line.starts_with("---") {
                entry.1 = entry.1.saturating_add(1);
            }
        }
    }
    changes
        .into_iter()
        .map(|(path, (added, removed))| FileChange {
            path,
            added,
            removed,
        })
        .collect()
}

fn test_summary(records: &[crate::contracts::TestExecutionRecord]) -> Option<TestSummary> {
    if records.is_empty() {
        return None;
    }
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut ignored = 0u32;
    let mut duration_secs = 0u64;
    let mut parsed_any = false;
    let mut exit_ok = true;
    for record in records {
        exit_ok &= record.exit_code == 0 && record.status == "passed";
        if let Some(path) = &record.stdout_log_path
            && let Ok(text) = std::fs::read_to_string(path)
            && let Some(counts) = parse_test_counts(&text)
        {
            passed += counts.0;
            failed += counts.1;
            ignored += counts.2;
            duration_secs += counts.3;
            parsed_any = true;
        }
    }
    if !parsed_any {
        for record in records {
            if record.exit_code == 0 && record.status == "passed" {
                passed += 1;
            } else {
                failed += 1;
            }
            duration_secs += record.duration_ms / 1_000;
        }
    }
    exit_ok &= failed == 0;
    Some(TestSummary {
        passed,
        failed,
        ignored,
        duration_secs,
        exit_ok,
    })
}

/// Parse cargo's `test result: ok. 41 passed; 0 failed; 0 ignored; … finished in 2.34s`.
fn parse_test_counts(text: &str) -> Option<(u32, u32, u32, u64)> {
    let mut total = (0u32, 0u32, 0u32, 0u64);
    let mut found = false;
    for line in text
        .lines()
        .filter(|line| line.trim_start().starts_with("test result:"))
    {
        let mut passed = None;
        let mut failed = None;
        let mut ignored = None;
        for part in line.split(';') {
            let words: Vec<&str> = part.split_whitespace().collect();
            if let Some((number, label)) = words
                .iter()
                .zip(words.iter().skip(1))
                .find(|(_, label)| matches!(**label, "passed" | "failed" | "ignored"))
                .map(|(number, label)| (*number, *label))
            {
                let value: u32 = number.parse().ok()?;
                match label {
                    "passed" => passed = Some(value),
                    "failed" => failed = Some(value),
                    "ignored" => ignored = Some(value),
                    _ => {}
                }
            }
        }
        let duration = line
            .split("finished in ")
            .nth(1)
            .and_then(|rest| rest.trim_end_matches('s').trim().parse::<f64>().ok())
            .unwrap_or(0.0);
        total.0 = total.0.saturating_add(passed?);
        total.1 = total.1.saturating_add(failed?);
        total.2 = total.2.saturating_add(ignored?);
        total.3 = total.3.saturating_add(duration.max(0.0).round() as u64);
        found = true;
    }
    found.then_some(total)
}

fn gate_results(patch: &str) -> Vec<GateResult> {
    let violations = DiffGatePolicy::default().evaluate(patch, &[]);
    if violations.is_empty() {
        return vec![GateResult {
            name: "deterministic".into(),
            passed: true,
            blocking: true,
            detail: "Pre-review gates passed".into(),
        }];
    }
    violations
        .into_iter()
        .map(|violation| GateResult {
            name: violation.rule,
            passed: false,
            blocking: true,
            detail: violation.detail,
        })
        .collect()
}

fn review_summary(report: &ForgeJobReport) -> Option<ReviewSummary> {
    let review = report.review_result.as_ref()?;
    let verdict = &review.verdict;
    let mut concerns = Vec::new();
    concerns.extend(
        verdict
            .detected_bugs
            .iter()
            .map(|bug| format!("bug: {bug}")),
    );
    concerns.extend(
        verdict
            .untested_edge_cases
            .iter()
            .map(|case| format!("edge case: {case}")),
    );
    concerns.extend(
        verdict
            .security_observations
            .iter()
            .map(|observation| format!("security: {observation}")),
    );
    concerns.extend(
        verdict
            .suggested_edits
            .iter()
            .map(|edit| format!("suggested edit: {edit}")),
    );
    Some(ReviewSummary {
        passed: verdict.passed && verdict.requirements_covered,
        concerns,
    })
}

fn review_question(report: &ForgeJobReport) -> String {
    let Some(review) = report.review_result.as_ref() else {
        return "the reviewer rejected the patch".into();
    };
    let verdict = &review.verdict;
    let mut parts: Vec<String> = verdict
        .detected_bugs
        .iter()
        .chain(verdict.untested_edge_cases.iter())
        .chain(verdict.security_observations.iter())
        .take(3)
        .filter(|part| !part.trim().is_empty())
        .cloned()
        .collect();
    if parts.is_empty() && verdict.requirements_covered {
        let feedback = verdict.raw_feedback.trim();
        if !feedback.is_empty() {
            parts.push(feedback.to_string());
        }
    }
    let detail = parts.join("; ");
    let mut question = if verdict.requirements_covered {
        if detail.is_empty() {
            "the reviewer rejected the patch".to_string()
        } else {
            format!("the reviewer raised concerns: {detail}")
        }
    } else if detail.is_empty() {
        "the reviewer questioned whether the request was fully covered".to_string()
    } else {
        format!("the reviewer questioned whether the request was fully covered: {detail}")
    };
    if question.chars().count() > MAX_QUESTION_CHARS {
        question = question.chars().take(MAX_QUESTION_CHARS - 1).collect();
        question.push('…');
    }
    question
}

/// Deterministic summary, at most 300 characters. Pure.
pub fn summary(r: &NightResult) -> String {
    let added: u64 = r.files.iter().map(|f| u64::from(f.added)).sum();
    let removed: u64 = r.files.iter().map(|f| u64::from(f.removed)).sum();
    let mut parts = Vec::new();
    parts.push(if r.files.is_empty() {
        "Changed no files.".to_string()
    } else {
        format!(
            "Changed {} file{} (+{added} −{removed}).",
            r.files.len(),
            if r.files.len() == 1 { "" } else { "s" }
        )
    });
    match &r.tests {
        Some(tests) if tests.exit_ok && tests.failed == 0 => parts.push(format!(
            "{} test{} passed.",
            tests.passed,
            if tests.passed == 1 { "" } else { "s" }
        )),
        Some(tests) if tests.failed > 0 => parts.push(format!("{} tests failed.", tests.failed)),
        Some(_) => parts.push("The test run failed.".into()),
        None => parts.push("Tests did not run.".into()),
    }
    if r.gates.is_empty() {
        parts.push("No checks were recorded.".into());
    } else if r.gates.iter().all(|gate| gate.passed) {
        parts.push("All checks passed.".into());
    } else {
        let names: Vec<&str> = r
            .gates
            .iter()
            .filter(|gate| !gate.passed)
            .map(|gate| gate.name.as_str())
            .collect();
        parts.push(format!("Checks failed: {}.", names.join(", ")));
    }
    match &r.review {
        Some(review) if review.passed => parts.push("Self-review: no concerns.".into()),
        Some(_) => parts.push("Self-review raised concerns.".into()),
        None => parts.push("Self-review did not run.".into()),
    }
    let text = parts.join(" ");
    if text.chars().count() > 300 {
        let mut shortened: String = text.chars().take(299).collect();
        shortened.push('…');
        shortened
    } else {
        text
    }
}
