//! K6: external solve guards, prompt bounds, SEARCH/REPLACE and verification.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sail_continuity::Continuity;
use sail_forge::contracts::{ReviewPolicyConfig, ReviewVerdict};
use sail_forge::nightshift::external::external_attempt;
use sail_forge::nightshift::{
    Errand, ErrandSource, ErrandState, ErrandStore, ExternalSolver, MorningOutcome, NightResult,
    RanOn, RanOnKind, Size, TestSummary, Transition, WorkOrder,
};
use sail_forge::orchestrator::{ForgeOrchestrator, OrchestratorConfig};
use sail_forge::persistence::TaskCheckpointStore;
use sail_forge::reviewer::{ReviewBroker, ReviewError};
use sail_forge::self_review::{ReviewPacket, ReviewProvider};
use sail_guardian::sandbox::SandboxPolicy;
use tokio_util::sync::CancellationToken;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("sail-nightshift-external-{}", uuid::Uuid::new_v4()));
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
    "2026-11-18T23:30:00Z".parse().expect("fixed time")
}

fn host() -> RanOn {
    RanOn {
        kind: RanOnKind::Host,
        name: "host".into(),
        model: "local".into(),
    }
}

fn external() -> RanOn {
    RanOn {
        kind: RanOnKind::External,
        name: "Claude Code".into(),
        model: "claude-sonnet".into(),
    }
}

struct PassingReviewer;

#[async_trait]
impl ReviewProvider for PassingReviewer {
    fn name(&self) -> String {
        "test-reviewer".into()
    }
    async fn review(&self, _packet: &ReviewPacket) -> Result<ReviewVerdict, ReviewError> {
        Ok(ReviewVerdict {
            passed: true,
            requirements_covered: true,
            untested_edge_cases: Vec::new(),
            detected_bugs: Vec::new(),
            security_observations: Vec::new(),
            suggested_edits: Vec::new(),
            confidence_score: 0.9,
            raw_feedback: String::new(),
        })
    }
}

fn boot(root: &Path, repo: &Path) -> Arc<ForgeOrchestrator> {
    let mut allowed_repos = std::collections::HashSet::new();
    allowed_repos.insert("local/test-repo".into());
    let config = OrchestratorConfig {
        workspace_root: repo.to_path_buf(),
        allowed_repos,
        ..OrchestratorConfig::default()
    };
    let broker = Arc::new(
        ReviewBroker::new(ReviewPolicyConfig::default())
            .with_mock_mode(true)
            .with_ledger_path(root.join("ledger.json")),
    );
    Arc::new(
        ForgeOrchestrator::new(
            config,
            Arc::new(TaskCheckpointStore::new(root.join("checkpoints"))),
            broker,
            Arc::new(SandboxPolicy::new()),
        )
        .with_self_reviewer(Arc::new(PassingReviewer))
        .with_continuity(Arc::new(
            Continuity::open(root.join("continuity.db")).expect("continuity"),
        )),
    )
}

fn setup_repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("src")).expect("repo");
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"nightshift_ext_demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("cargo");
    std::fs::write(repo.join("src/lib.rs"), "pub fn value() -> u32 { 1 }\n").expect("lib");
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "user.email", "test@test.local"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "init"]);
    repo
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn head(repo: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .expect("git");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn order(path: &str) -> WorkOrder {
    WorkOrder {
        title: "Change the value".into(),
        why: "the owner asked".into(),
        acceptance: vec!["the value is 2".into()],
        target_files: vec![path.into()],
        size: Size::S,
        notes_for_solver: String::new(),
    }
}

fn failed_result(base_commit: &str) -> NightResult {
    NightResult {
        job_id: "job-local".into(),
        base_commit: base_commit.into(),
        ran_on: host(),
        files: Vec::new(),
        tests: Some(TestSummary {
            passed: 0,
            failed: 1,
            ignored: 0,
            duration_secs: 1,
            exit_ok: false,
        }),
        gates: Vec::new(),
        review: None,
        summary: "1 test failed.".into(),
        model_note: None,
        patch_bytes: 0,
        finished_at: now(),
    }
}

fn failed_errand(store: &ErrandStore, path: &str, base_commit: &str, seed: u8) -> Errand {
    let errand = store
        .add("Make the value two.", ErrandSource::Web, now(), [seed; 6])
        .expect("add");
    store
        .transition(&errand.id, now(), Transition::Proposed(order(path)))
        .expect("propose");
    store
        .transition(&errand.id, now(), Transition::Approve(None))
        .expect("approve");
    store
        .transition(
            &errand.id,
            now(),
            Transition::Working {
                job_id: "job-local".into(),
                stage: "testing".into(),
                ran_on: host(),
            },
        )
        .expect("work");
    store
        .transition(
            &errand.id,
            now(),
            Transition::Finished {
                result: failed_result(base_commit),
                outcome: MorningOutcome::Failed,
                question_or_reason: Some("the local tests failed".into()),
            },
        )
        .expect("failed")
}

struct FakeSolver {
    reply: Result<String, String>,
    calls: AtomicUsize,
    prompts: Mutex<Vec<String>>,
}

impl FakeSolver {
    fn new(reply: Result<String, String>) -> Self {
        Self {
            reply,
            calls: AtomicUsize::new(0),
            prompts: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ExternalSolver for FakeSolver {
    fn ran_on(&self) -> RanOn {
        external()
    }
    async fn solve(
        &self,
        _system: &str,
        prompt: &str,
        _cancel: CancellationToken,
    ) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompts
            .lock()
            .expect("prompts")
            .push(prompt.to_string());
        self.reply.clone()
    }
}

const EDIT: &str = "src/lib.rs\n<<<<<<< SEARCH\npub fn value() -> u32 { 1 }\n=======\npub fn value() -> u32 { 2 }\n>>>>>>> REPLACE\n";

#[tokio::test]
async fn only_failed_or_needs_you_errands_without_a_previous_external_try() {
    let tmp = TempDir::new();
    let repo = setup_repo(&tmp.0);
    let orchestrator = boot(&tmp.0, &repo);
    let store = ErrandStore::open(&tmp.0.join("state")).expect("store");
    let errand = failed_errand(&store, "src/lib.rs", &head(&repo), 1);
    let solver = FakeSolver::new(Ok(EDIT.into()));

    let marked = store
        .transition(&errand.id, now(), Transition::MarkExternalUsed)
        .expect("mark");
    let error = external_attempt(
        &orchestrator,
        &store,
        &solver,
        "local/test-repo",
        &marked,
        CancellationToken::new(),
    )
    .await
    .expect_err("already external");
    assert!(error.contains("already went"));
    assert_eq!(solver.calls.load(Ordering::SeqCst), 0);

    let queued = store
        .add("another ask", ErrandSource::Web, now(), [9; 6])
        .expect("add");
    let error = external_attempt(
        &orchestrator,
        &store,
        &solver,
        "local/test-repo",
        &queued,
        CancellationToken::new(),
    )
    .await
    .expect_err("not failed");
    assert!(error.contains("failed or needs-you"));
}

#[tokio::test]
async fn an_oversized_prompt_is_skipped_before_marking_or_sending() {
    let tmp = TempDir::new();
    let repo = setup_repo(&tmp.0);
    std::fs::write(repo.join("big.rs"), "x".repeat(30_000)).expect("big");
    git(&repo, &["add", "big.rs"]);
    git(&repo, &["commit", "-m", "big"]);
    let orchestrator = boot(&tmp.0, &repo);
    let store = ErrandStore::open(&tmp.0.join("state")).expect("store");
    let errand = failed_errand(&store, "big.rs", &head(&repo), 2);
    let solver = FakeSolver::new(Ok(EDIT.into()));

    let error = external_attempt(
        &orchestrator,
        &store,
        &solver,
        "local/test-repo",
        &errand,
        CancellationToken::new(),
    )
    .await
    .expect_err("too large");
    assert_eq!(error, "too large to send");
    assert_eq!(solver.calls.load(Ordering::SeqCst), 0);
    assert!(!store.get(&errand.id).expect("errand").external_used);
}

#[tokio::test]
async fn a_failed_cli_call_or_an_unparseable_reply_marks_external_used() {
    let tmp = TempDir::new();
    let repo = setup_repo(&tmp.0);
    let orchestrator = boot(&tmp.0, &repo);
    let store = ErrandStore::open(&tmp.0.join("state")).expect("store");
    let errand = failed_errand(&store, "src/lib.rs", &head(&repo), 3);
    let solver = FakeSolver::new(Err("cli exited 1".into()));

    let error = external_attempt(
        &orchestrator,
        &store,
        &solver,
        "local/test-repo",
        &errand,
        CancellationToken::new(),
    )
    .await
    .expect_err("cli error");
    assert!(error.contains("cli exited 1"));
    assert!(store.get(&errand.id).expect("errand").external_used);

    let errand = failed_errand(&store, "src/lib.rs", &head(&repo), 4);
    let solver = FakeSolver::new(Ok("no blocks here".into()));
    let error = external_attempt(
        &orchestrator,
        &store,
        &solver,
        "local/test-repo",
        &errand,
        CancellationToken::new(),
    )
    .await
    .expect_err("no blocks");
    assert!(error.contains("SEARCH/REPLACE"));
    assert!(store.get(&errand.id).expect("errand").external_used);
}

#[tokio::test]
async fn a_verified_external_patch_replaces_the_failed_result() {
    let tmp = TempDir::new();
    let repo = setup_repo(&tmp.0);
    let orchestrator = boot(&tmp.0, &repo);
    let store = ErrandStore::open(&tmp.0.join("state")).expect("store");
    let errand = failed_errand(&store, "src/lib.rs", &head(&repo), 5);
    let solver = FakeSolver::new(Ok(EDIT.into()));

    external_attempt(
        &orchestrator,
        &store,
        &solver,
        "local/test-repo",
        &errand,
        CancellationToken::new(),
    )
    .await
    .expect("verified externally");

    let finished = store.get(&errand.id).expect("errand");
    assert!(finished.external_used);
    assert!(matches!(finished.state, ErrandState::Ready { .. }));
    let patch = store.patch(&errand.id).expect("patch");
    assert!(patch.contains("pub fn value() -> u32 { 2 }"));
    match finished.state {
        ErrandState::Ready { result, .. } => {
            assert_eq!(result.ran_on.kind, RanOnKind::External);
            assert_eq!(result.ran_on.name, "Claude Code");
            assert!(result.tests.expect("tests").exit_ok);
        }
        other => panic!("expected ready, got {other:?}"),
    }
    let prompts = solver.prompts.lock().expect("prompts");
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("Last local attempt failed"));
    assert!(prompts[0].contains("the local tests failed"));
    assert!(prompts[0].contains("src/lib.rs"));
}

#[tokio::test]
async fn the_prompt_reads_target_files_at_the_base_not_the_dirty_checkout() {
    let tmp = TempDir::new();
    let repo = setup_repo(&tmp.0);
    let base = head(&repo);
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn value() -> u32 { 999 } // dirty working copy\n",
    )
    .expect("dirty");
    let orchestrator = boot(&tmp.0, &repo);
    let store = ErrandStore::open(&tmp.0.join("state")).expect("store");
    let errand = failed_errand(&store, "src/lib.rs", &base, 7);
    let solver = FakeSolver::new(Ok("no blocks".into()));

    assert!(
        external_attempt(
            &orchestrator,
            &store,
            &solver,
            "local/test-repo",
            &errand,
            CancellationToken::new(),
        )
        .await
        .is_err()
    );
    let prompts = solver.prompts.lock().expect("prompts");
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("pub fn value() -> u32 { 1 }"));
    assert!(!prompts[0].contains("999"));
    assert!(!prompts[0].contains("dirty working copy"));
}

#[tokio::test]
async fn a_cancelled_token_stops_before_the_cli() {
    let tmp = TempDir::new();
    let repo = setup_repo(&tmp.0);
    let orchestrator = boot(&tmp.0, &repo);
    let store = ErrandStore::open(&tmp.0.join("state")).expect("store");
    let errand = failed_errand(&store, "src/lib.rs", &head(&repo), 6);
    let solver = FakeSolver::new(Ok(EDIT.into()));
    let cancel = CancellationToken::new();
    cancel.cancel();

    assert!(
        external_attempt(
            &orchestrator,
            &store,
            &solver,
            "local/test-repo",
            &errand,
            cancel,
        )
        .await
        .is_err()
    );
    assert_eq!(solver.calls.load(Ordering::SeqCst), 0);
    assert!(!store.get(&errand.id).expect("errand").external_used);
}
