//! K4: result extraction, deterministic summaries and the morning report.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use sail_forge::contracts::{
    ForgeJobReport, ForgeTaskState, ReviewResult, ReviewVerdict, TestExecutionRecord,
};
use sail_forge::nightshift::morning::compose_morning;
use sail_forge::nightshift::result::{result_from_report, summary};
use sail_forge::nightshift::{
    Errand, ErrandId, ErrandSource, ErrandState, FileChange, GateResult, MorningOutcome,
    MorningReport, NightItem, NightPlan, NightResult, OwnerNote, RanOn, RanOnKind, ReviewSummary,
    Size, TestSummary, WorkOrder,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("sail-nightshift-morning-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn now() -> DateTime<Utc> {
    "2026-11-18T23:00:00Z".parse().expect("fixed time")
}

fn host() -> RanOn {
    RanOn {
        kind: RanOnKind::Host,
        name: "host".into(),
        model: "local".into(),
    }
}

fn order(title: &str) -> WorkOrder {
    WorkOrder {
        title: title.into(),
        why: "because".into(),
        acceptance: vec!["it works".into()],
        target_files: vec!["crates/demo/src/lib.rs".into()],
        size: Size::S,
        notes_for_solver: String::new(),
    }
}

fn result_with(summary_text: &str) -> NightResult {
    NightResult {
        job_id: "job-1".into(),
        base_commit: "a".repeat(40),
        ran_on: host(),
        files: vec![FileChange {
            path: "crates/demo/src/lib.rs".into(),
            added: 3,
            removed: 1,
        }],
        tests: Some(TestSummary {
            passed: 2,
            failed: 0,
            ignored: 0,
            duration_secs: 1,
            exit_ok: true,
        }),
        gates: vec![GateResult {
            name: "deterministic".into(),
            passed: true,
            blocking: true,
            detail: "ok".into(),
        }],
        review: Some(ReviewSummary {
            passed: true,
            concerns: Vec::new(),
        }),
        summary: summary_text.into(),
        model_note: None,
        patch_bytes: 10,
        finished_at: now(),
    }
}

fn errand(seed: u8, title: &str, state: ErrandState) -> Errand {
    Errand {
        id: ErrandId::new([seed; 6]),
        ask: format!("do the {title} thing"),
        source: ErrandSource::Web,
        created_at: now(),
        updated_at: now(),
        attempts: 1,
        external_used: false,
        notes: Vec::<OwnerNote>::new(),
        state,
    }
}

fn ready(seed: u8, title: &str) -> Errand {
    errand(
        seed,
        title,
        ErrandState::Ready {
            order: order(title),
            result: result_with("Changed 1 file (+3 −1). 2 tests passed."),
        },
    )
}

#[test]
fn pinned_morning_example() {
    let errands = vec![
        ready(1, "calendar fix"),
        ready(2, "search fix"),
        errand(
            3,
            "search fix",
            ErrandState::NeedsYou {
                order: order("search fix"),
                result: result_with("Changed 2 files (+8 −2). 12 tests passed."),
                question: "which tab?".into(),
            },
        ),
    ];
    let plan = NightPlan {
        items: errands
            .iter()
            .map(|errand| NightItem::Errand {
                id: errand.id.clone(),
            })
            .collect(),
        deferred: Vec::new(),
    };
    let report = compose_morning("2026-11-18", &plan, &errands, &BTreeMap::new(), None);
    assert_eq!(
        report.spoken,
        "Overnight I finished 2 of 3 jobs; both are ready to review. The search fix needs a decision from you."
    );
    assert_eq!(report.ready, 2);
    assert_eq!(report.needs_you, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(report.not_reached, 0);
    assert_eq!(report.items.len(), 3);
    assert_eq!(report.items[2].outcome, MorningOutcome::NeedsYou);
    assert!(report.items[2].line.contains("which tab?"));
}

#[test]
fn nothing_ran_has_no_spoken_text() {
    let report = compose_morning(
        "2026-11-18",
        &NightPlan {
            items: Vec::new(),
            deferred: Vec::new(),
        },
        &[],
        &BTreeMap::new(),
        None,
    );
    assert_eq!(report.spoken, "");
    assert_eq!(report.items, Vec::new());
    assert_eq!(report.halted, None);
}

#[test]
fn queued_items_are_not_reached_and_counted() {
    let errands = vec![
        ready(1, "done"),
        errand(
            2,
            "not started",
            ErrandState::Queued {
                order: order("not started"),
                approved_at: now(),
            },
        ),
    ];
    let plan = NightPlan {
        items: errands
            .iter()
            .map(|errand| NightItem::Errand {
                id: errand.id.clone(),
            })
            .collect(),
        deferred: Vec::new(),
    };
    let report = compose_morning("2026-11-18", &plan, &errands, &BTreeMap::new(), None);
    assert_eq!(report.ready, 1);
    assert_eq!(report.not_reached, 1);
    assert_eq!(report.items[1].outcome, MorningOutcome::NotReached);
    assert_eq!(report.items[1].line, "Not reached before the morning.");
    assert_eq!(
        report.spoken,
        "Overnight I finished 1 of 2 jobs; one is ready to review."
    );
}

#[test]
fn halted_night_uses_a_short_second_sentence() {
    let errands = vec![errand(
        1,
        "half done",
        ErrandState::Working {
            order: order("half done"),
            job_id: "job-1".into(),
            stage: "testing".into(),
            started_at: now(),
            ran_on: host(),
        },
    )];
    let plan = NightPlan {
        items: vec![NightItem::Errand {
            id: errands[0].id.clone(),
        }],
        deferred: Vec::new(),
    };
    let report = compose_morning(
        "2026-11-18",
        &plan,
        &errands,
        &BTreeMap::new(),
        Some("the model server stopped responding at 03:12".into()),
    );
    assert_eq!(report.not_reached, 1);
    assert_eq!(
        report.halted.as_deref(),
        Some("the model server stopped responding at 03:12")
    );
    assert_eq!(
        report.items[0].line,
        "Not reached: the night stopped early."
    );
    assert_eq!(report.spoken, "");

    let errands = vec![ready(1, "done")];
    let plan = NightPlan {
        items: vec![NightItem::Errand {
            id: errands[0].id.clone(),
        }],
        deferred: Vec::new(),
    };
    let report = compose_morning(
        "2026-11-18",
        &plan,
        &errands,
        &BTreeMap::new(),
        Some("the model server stopped responding at 03:12".into()),
    );
    assert_eq!(
        report.spoken,
        "Overnight I finished 1 of 1 jobs; one is ready to review. The night stopped early: the model server stopped responding at 03:12."
    );
}

#[test]
fn titles_are_short_and_never_file_paths() {
    let errands = vec![errand(
        1,
        "fix crates/demo/src/lib.rs and the thing",
        ErrandState::NeedsYou {
            order: order("fix crates/demo/src/lib.rs and the thing"),
            result: result_with("Changed 1 file (+1 −0). 1 test passed."),
            question: "ok?".into(),
        },
    )];
    let plan = NightPlan {
        items: vec![NightItem::Errand {
            id: errands[0].id.clone(),
        }],
        deferred: Vec::new(),
    };
    let report = compose_morning("2026-11-18", &plan, &errands, &BTreeMap::new(), None);
    assert!(!report.spoken.contains("crates/"));
    assert!(!report.items[0].title.contains("crates/"));
    assert_eq!(report.items[0].title, "fix and the thing");
    assert_eq!(
        report.spoken,
        "Overnight I finished 0 of 1 jobs. The fix and the thing needs a decision from you."
    );
}

#[test]
fn issues_use_their_titles_and_are_not_reached_without_a_mapping() {
    let plan = NightPlan {
        items: vec![
            NightItem::Issue { number: 125 },
            NightItem::Issue { number: 126 },
        ],
        deferred: Vec::new(),
    };
    let titles = BTreeMap::from([(125u64, "Search crash".to_string())]);
    let report = compose_morning("2026-11-18", &plan, &[], &titles, None);
    assert_eq!(report.items[0].title, "Search crash");
    assert_eq!(report.items[1].title, "Issue #126");
    assert_eq!(report.not_reached, 2);
    assert_eq!(report.spoken, "");
}

#[test]
fn morning_composition_is_deterministic() {
    let errands = vec![ready(1, "one"), ready(2, "two")];
    let plan = NightPlan {
        items: errands
            .iter()
            .map(|errand| NightItem::Errand {
                id: errand.id.clone(),
            })
            .collect(),
        deferred: Vec::new(),
    };
    let first = compose_morning("2026-11-18", &plan, &errands, &BTreeMap::new(), None);
    let second = compose_morning("2026-11-18", &plan, &errands, &BTreeMap::new(), None);
    assert_eq!(first, second);
}

#[test]
fn summary_matches_the_contract_example() {
    let result = NightResult {
        files: vec![
            FileChange {
                path: "a".into(),
                added: 80,
                removed: 10,
            },
            FileChange {
                path: "b".into(),
                added: 3,
                removed: 1,
            },
            FileChange {
                path: "c".into(),
                added: 1,
                removed: 1,
            },
        ],
        tests: Some(TestSummary {
            passed: 41,
            failed: 0,
            ignored: 2,
            duration_secs: 12,
            exit_ok: true,
        }),
        gates: vec![GateResult {
            name: "deterministic".into(),
            passed: true,
            blocking: true,
            detail: "ok".into(),
        }],
        review: Some(ReviewSummary {
            passed: true,
            concerns: Vec::new(),
        }),
        summary: String::new(),
        ..result_with("")
    };
    assert_eq!(
        summary(&result),
        "Changed 3 files (+84 −12). 41 tests passed. All checks passed. Self-review: no concerns."
    );
}

#[test]
fn summary_handles_empty_failed_and_long_results() {
    let result = NightResult {
        files: Vec::new(),
        tests: None,
        gates: Vec::new(),
        review: None,
        ..result_with("")
    };
    assert_eq!(
        summary(&result),
        "Changed no files. Tests did not run. No checks were recorded. Self-review did not run."
    );

    let result = NightResult {
        files: vec![FileChange {
            path: "a".into(),
            added: 1,
            removed: 0,
        }],
        tests: Some(TestSummary {
            passed: 4,
            failed: 2,
            ignored: 0,
            duration_secs: 1,
            exit_ok: false,
        }),
        gates: vec![
            GateResult {
                name: "forbidden_path".into(),
                passed: false,
                blocking: true,
                detail: "x".into(),
            },
            GateResult {
                name: "new_unwrap".into(),
                passed: false,
                blocking: true,
                detail: "y".into(),
            },
        ],
        review: Some(ReviewSummary {
            passed: false,
            concerns: vec!["bug".into()],
        }),
        ..result_with("")
    };
    let text = summary(&result);
    assert!(text.contains("Changed 1 file (+1 −0)."));
    assert!(text.contains("2 tests failed."));
    assert!(text.contains("Checks failed: forbidden_path, new_unwrap."));
    assert!(text.contains("Self-review raised concerns."));

    let mut long = result_with("");
    long.gates = (0..30)
        .map(|i| GateResult {
            name: format!("gate_with_a_long_name_{i:02}"),
            passed: false,
            blocking: true,
            detail: String::new(),
        })
        .collect();
    let text = summary(&long);
    assert!(text.chars().count() <= 300);
    assert!(text.ends_with('…'));
}

fn review(passed: bool, covered: bool, bug: Option<&str>) -> ReviewResult {
    ReviewResult {
        verdict: ReviewVerdict {
            passed,
            requirements_covered: covered,
            untested_edge_cases: Vec::new(),
            detected_bugs: bug.map(|b| vec![b.to_string()]).unwrap_or_default(),
            security_observations: Vec::new(),
            suggested_edits: Vec::new(),
            confidence_score: 0.9,
            raw_feedback: "looks fine".into(),
        },
        cached: false,
        dedup_hash: "h".into(),
        input_tokens: 1,
        output_tokens: 1,
        cost_usd: 0.0,
        provider: "local".into(),
    }
}

fn report(patch: &str, log: Option<PathBuf>) -> ForgeJobReport {
    ForgeJobReport {
        issue: "owner/repo#125".into(),
        base_commit: "b".repeat(40),
        branch: "agent/forge-issue-125".into(),
        model_id: "local".into(),
        model_hash: "h".into(),
        patch_ref: patch.into(),
        tests: vec![TestExecutionRecord {
            command: "cargo test".into(),
            status: "passed".into(),
            duration_ms: 1_500,
            log_ref: "logs/job-7/cargo_test.log".into(),
            exit_code: 0,
            stdout_log_path: log,
            stderr_log_path: None,
        }],
        review_result: Some(review(true, true, None)),
        cost_local_estimate: 0.0,
        api_cost_actual: 0.0,
        remaining_risks: Vec::new(),
        status: ForgeTaskState::Completed,
        ready_to_publish: true,
    }
}

const PATCH: &str = "diff --git a/crates/demo/src/lib.rs b/crates/demo/src/lib.rs\n\
index 1111111..2222222 100644\n\
--- a/crates/demo/src/lib.rs\n\
+++ b/crates/demo/src/lib.rs\n\
@@ -1,2 +1,3 @@\n\
 fn main() {}\n\
-old line\n\
+new line\n\
+another line\n";

#[test]
fn report_ready_with_real_test_counts_and_patch_stats() {
    let tmp = TempDir::new();
    let log = tmp.0.join("cargo_test.log");
    std::fs::write(
        &log,
        "running 41 tests\n\ntest result: ok. 41 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 2.34s\n",
    )
    .expect("log");
    let mut report = report(PATCH, Some(log));
    report.patch_ref = PATCH.into();
    // A diff in patch_ref keeps the diff, so the job id falls back to the issue.
    let (result, outcome, question, patch) = result_from_report(&report, host(), now());
    assert_eq!(outcome, MorningOutcome::Ready);
    assert_eq!(question, None);
    assert_eq!(patch, PATCH);
    assert_eq!(result.files.len(), 1);
    assert_eq!(result.files[0].path, "crates/demo/src/lib.rs");
    assert_eq!(result.files[0].added, 2);
    assert_eq!(result.files[0].removed, 1);
    let tests = result.tests.as_ref().expect("tests");
    assert_eq!((tests.passed, tests.failed, tests.ignored), (41, 0, 0));
    assert_eq!(tests.duration_secs, 2);
    assert!(tests.exit_ok);
    assert_eq!(result.patch_bytes, PATCH.len() as u64);
    assert_eq!(result.summary, summary(&result));
    assert!(result.summary.contains("41 tests passed"));
}

#[test]
fn deleted_and_added_files_get_their_line_counts() {
    let patch = "diff --git a/crates/demo/old.rs b/crates/demo/old.rs\n\
--- a/crates/demo/old.rs\n\
+++ /dev/null\n\
@@ -1,2 +0,0 @@\n\
-line one\n\
-line two\n\
diff --git a/crates/demo/new.rs b/crates/demo/new.rs\n\
new file mode 100644\n\
--- /dev/null\n\
+++ b/crates/demo/new.rs\n\
@@ -0,0 +1 @@\n\
+brand new\n";
    let mut report = report(patch, None);
    report.patch_ref = patch.into();
    let (result, _, _, _) = result_from_report(&report, host(), now());
    let deleted = result
        .files
        .iter()
        .find(|file| file.path.ends_with("old.rs"))
        .expect("deleted file");
    assert_eq!((deleted.added, deleted.removed), (0, 2));
    let added = result
        .files
        .iter()
        .find(|file| file.path.ends_with("new.rs"))
        .expect("added file");
    assert_eq!((added.added, added.removed), (1, 0));
}

#[test]
fn failed_tests_are_failed_with_the_real_reason() {
    let tmp = TempDir::new();
    let log = tmp.0.join("failed.log");
    std::fs::write(
        &log,
        "test result: FAILED. 3 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.5s\n",
    )
    .expect("log");
    let mut report = report(PATCH, Some(log));
    report.tests[0].exit_code = 101;
    report.tests[0].status = "failed (101)".into();
    let (result, outcome, reason, _) = result_from_report(&report, host(), now());
    assert_eq!(outcome, MorningOutcome::Failed);
    assert_eq!(reason.as_deref(), Some("2 tests failed"));
    let tests = result.tests.expect("tests");
    assert!(!tests.exit_ok);
    assert_eq!((tests.passed, tests.failed), (3, 2));
}

#[test]
fn tests_that_never_ran_are_failed() {
    let mut report = report(PATCH, None);
    report.tests.clear();
    let (result, outcome, reason, _) = result_from_report(&report, host(), now());
    assert_eq!(outcome, MorningOutcome::Failed);
    assert_eq!(reason.as_deref(), Some("tests never ran"));
    assert!(result.tests.is_none());
}

#[test]
fn a_blocking_gate_failure_is_failed() {
    let mut report = report(PATCH, None);
    report.patch_ref = PATCH.replace("+another line", "+let value = thing.unwrap();");
    let (result, outcome, reason, _) = result_from_report(&report, host(), now());
    assert_eq!(outcome, MorningOutcome::Failed);
    assert!(
        result
            .gates
            .iter()
            .any(|gate| gate.name == "new_unwrap" && !gate.passed)
    );
    assert!(reason.expect("reason").contains("new_unwrap"));
}

#[test]
fn review_rejection_is_needs_you_with_the_question() {
    let mut report = report(PATCH, None);
    report.review_result = Some(review(false, true, Some("the cache is unbounded")));
    let (result, outcome, question, _) = result_from_report(&report, host(), now());
    assert_eq!(outcome, MorningOutcome::NeedsYou);
    assert!(question.expect("question").contains("cache is unbounded"));
    let review = result.review.expect("review");
    assert!(!review.passed);
    assert!(
        review
            .concerns
            .iter()
            .any(|c| c.contains("cache is unbounded"))
    );
}

#[test]
fn uncovered_requirements_are_needs_you_even_when_the_review_passed() {
    let mut report = report(PATCH, None);
    report.review_result = Some(review(true, false, None));
    let (result, outcome, question, _) = result_from_report(&report, host(), now());
    assert_eq!(outcome, MorningOutcome::NeedsYou);
    assert!(question.expect("question").contains("fully covered"));
    assert!(!result.review.expect("review").passed);
}

#[test]
fn forge_not_ready_is_failed() {
    let mut report = report(PATCH, None);
    report.ready_to_publish = false;
    let (_, outcome, reason, _) = result_from_report(&report, host(), now());
    assert_eq!(outcome, MorningOutcome::Failed);
    assert_eq!(
        reason.as_deref(),
        Some("Forge did not mark the result verified")
    );
}

#[test]
fn no_review_is_allowed_when_tests_and_gates_pass() {
    let mut report = report(PATCH, None);
    report.review_result = None;
    let (result, outcome, _, _) = result_from_report(&report, host(), now());
    assert_eq!(outcome, MorningOutcome::Ready);
    assert!(result.review.is_none());
    assert!(result.summary.contains("Self-review did not run."));
}

#[test]
fn malformed_test_log_falls_back_to_the_exit_status() {
    let tmp = TempDir::new();
    let log = tmp.0.join("broken.log");
    std::fs::write(&log, "cargo output without a result line").expect("log");
    let report = report(PATCH, Some(log));
    let (result, outcome, _, _) = result_from_report(&report, host(), now());
    assert_eq!(outcome, MorningOutcome::Ready);
    let tests = result.tests.expect("tests");
    assert_eq!((tests.passed, tests.failed), (1, 0));
    assert!(tests.exit_ok);
}

#[test]
fn multiple_test_result_lines_are_summed() {
    let tmp = TempDir::new();
    let log = tmp.0.join("multi.log");
    std::fs::write(
        &log,
        "test result: ok. 2 passed; 0 failed; 1 ignored; finished in 1.2s\ntest result: ok. 3 passed; 0 failed; 0 ignored; finished in 0.8s\n",
    )
    .expect("log");
    let report = report(PATCH, Some(log));
    let (result, _, _, _) = result_from_report(&report, host(), now());
    let tests = result.tests.expect("tests");
    assert_eq!((tests.passed, tests.failed, tests.ignored), (5, 0, 1));
    assert_eq!(tests.duration_secs, 2);
}

#[test]
fn every_result_is_deterministic_except_for_the_patch_path_branch() {
    let report = report(PATCH, None);
    let first = result_from_report(&report, host(), now());
    let second = result_from_report(&report, host(), now());
    assert_eq!(first, second);
    let _: MorningReport = compose_morning(
        "2026-11-18",
        &NightPlan {
            items: Vec::new(),
            deferred: Vec::new(),
        },
        &[],
        &BTreeMap::new(),
        None,
    );
}
