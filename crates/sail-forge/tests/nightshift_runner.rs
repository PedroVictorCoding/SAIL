//! K5: Night Shift runner integration (errands, planning, endpoints, morning).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sail_continuity::Continuity;
use sail_forge::admission::IssueCandidate;
use sail_forge::contracts::{
    ForgeJobReport, ReviewPolicyConfig, ReviewResult, ReviewVerdict, TestExecutionRecord,
};
use sail_forge::github::{DraftPr, GithubClient, OpenPullRequest};
use sail_forge::nightly::{ForgeNightlyRunner, JobExecutor, NightlyConfig, NightlyState};
use sail_forge::nightshift::plan::{PlanErrand, PlanIssue, SizeDurations};
use sail_forge::nightshift::{
    Endpoint, EndpointPicker, ErrandSource, ErrandState, ErrandStore, ExternalSolver,
    NightShiftSettings, RanOn, RanOnKind, Size, Transition, WorkOrder,
};
use sail_forge::orchestrator::{ForgeOrchestrator, OrchestratorConfig};
use sail_forge::persistence::TaskCheckpointStore;
use sail_forge::reviewer::ReviewBroker;
use sail_guardian::sandbox::SandboxPolicy;

const PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs\n\
index 1111111..2222222 100644\n\
--- a/src/lib.rs\n\
+++ b/src/lib.rs\n\
@@ -1,2 +1,3 @@\n\
 pub fn compute() -> i32 { 42 }\n\
+// Generated overnight.\n";

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "sail-nightshift-runner-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn boot(root: &Path) -> Arc<ForgeOrchestrator> {
    let config = OrchestratorConfig {
        workspace_root: root.join("workspace"),
        ..OrchestratorConfig::default()
    };
    let _ = std::fs::create_dir_all(config.workspace_root.join("src"));
    let _ = std::fs::write(
        config.workspace_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 1 }\n",
    );
    let _ = std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&config.workspace_root)
        .output();
    for args in [
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@test.local"],
        vec!["add", "."],
        vec!["commit", "-m", "init"],
    ] {
        let _ = std::process::Command::new("git")
            .args(args)
            .current_dir(&config.workspace_root)
            .output();
    }
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
        .with_continuity(Arc::new(
            Continuity::open(root.join("continuity.db")).expect("continuity"),
        )),
    )
}

fn host() -> RanOn {
    RanOn {
        kind: RanOnKind::Host,
        name: "host".into(),
        model: "local".into(),
    }
}

fn mesh() -> RanOn {
    RanOn {
        kind: RanOnKind::Mesh,
        name: "windows".into(),
        model: "pc-model".into(),
    }
}

struct FakePicker {
    mesh_calls: AtomicUsize,
    host_calls: AtomicUsize,
}

impl FakePicker {
    fn new() -> Self {
        Self {
            mesh_calls: AtomicUsize::new(0),
            host_calls: AtomicUsize::new(0),
        }
    }
}

impl EndpointPicker for FakePicker {
    fn pick(&self, _use_mesh: bool) -> Endpoint {
        self.mesh_calls.fetch_add(1, Ordering::SeqCst);
        Endpoint {
            url: "http://mesh.test:8080".into(),
            ran_on: mesh(),
            client: None,
        }
    }

    fn host(&self) -> Endpoint {
        self.host_calls.fetch_add(1, Ordering::SeqCst);
        Endpoint {
            url: "http://host.test:8080".into(),
            ran_on: host(),
            client: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Ready,
    NeedsYou,
}

#[derive(Clone)]
enum Reply {
    Report(Outcome),
    Error(String),
}

struct FakeExecutor {
    replies: Mutex<VecDeque<Reply>>,
    calls: AtomicUsize,
    bodies: Mutex<Vec<String>>,
}

impl FakeExecutor {
    fn new(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            calls: AtomicUsize::new(0),
            bodies: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl JobExecutor for FakeExecutor {
    async fn execute(
        &self,
        orchestrator: &ForgeOrchestrator,
        job_id: &str,
    ) -> Result<ForgeJobReport, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let checkpoint = orchestrator
            .checkpoint_store()
            .load(job_id)
            .expect("load checkpoint")
            .expect("checkpoint exists");
        self.bodies
            .lock()
            .expect("bodies")
            .push(checkpoint.issue_body.unwrap_or_default());
        let reply = self
            .replies
            .lock()
            .expect("replies")
            .pop_front()
            .unwrap_or(Reply::Report(Outcome::Ready));
        match reply {
            Reply::Error(reason) => Err(reason),
            Reply::Report(outcome) => Ok(report(outcome, job_id, &checkpoint.repo)),
        }
    }
}

fn review(passed: bool) -> ReviewResult {
    ReviewResult {
        verdict: ReviewVerdict {
            passed,
            requirements_covered: passed,
            untested_edge_cases: Vec::new(),
            detected_bugs: Vec::new(),
            security_observations: Vec::new(),
            suggested_edits: Vec::new(),
            confidence_score: 0.9,
            raw_feedback: String::new(),
        },
        cached: false,
        dedup_hash: "h".into(),
        input_tokens: 1,
        output_tokens: 1,
        cost_usd: 0.0,
        provider: "test".into(),
    }
}

fn report(outcome: Outcome, job_id: &str, repo: &str) -> ForgeJobReport {
    let needs_you = outcome == Outcome::NeedsYou;
    ForgeJobReport {
        issue: format!("{repo}#1"),
        base_commit: "HEAD".into(),
        branch: format!("agent/forge-issue-1-{job_id}"),
        model_id: "test".into(),
        model_hash: "h".into(),
        patch_ref: PATCH.into(),
        tests: vec![TestExecutionRecord {
            command: "cargo test".into(),
            status: "passed".into(),
            duration_ms: 1_000,
            log_ref: String::new(),
            exit_code: 0,
            stdout_log_path: None,
            stderr_log_path: None,
        }],
        review_result: Some(review(!needs_you)),
        cost_local_estimate: 0.0,
        api_cost_actual: 0.0,
        remaining_risks: Vec::new(),
        status: sail_forge::contracts::ForgeTaskState::Completed,
        ready_to_publish: true,
    }
}

#[derive(Default)]
struct FakeExternal {
    calls: AtomicUsize,
}

#[async_trait]
impl ExternalSolver for FakeExternal {
    fn ran_on(&self) -> RanOn {
        RanOn {
            kind: RanOnKind::External,
            name: "Claude Code".into(),
            model: "claude-sonnet".into(),
        }
    }
    async fn solve(
        &self,
        _system: &str,
        _prompt: &str,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err("the CLI is unavailable in this test".into())
    }
}

#[derive(Default)]
struct FakeGithub {
    issues: Vec<IssueCandidate>,
    created: Mutex<Vec<DraftPr>>,
}

impl GithubClient for FakeGithub {
    fn list_open_issues(&self, _repo: &str, _label: &str) -> Result<Vec<IssueCandidate>, String> {
        Ok(self.issues.clone())
    }
    fn open_pull_requests(&self, _repo: &str) -> Result<Vec<OpenPullRequest>, String> {
        Ok(Vec::new())
    }
    fn create_draft_pr(&self, _repo: &str, pr: &DraftPr) -> Result<String, String> {
        self.created.lock().expect("created").push(pr.clone());
        Ok("https://example.test/pull/1".into())
    }
}

fn order() -> WorkOrder {
    WorkOrder {
        title: "Show the next dividend date".into(),
        why: "the owner asked".into(),
        acceptance: vec!["the card shows it".into()],
        target_files: vec!["src/lib.rs".into()],
        size: Size::S,
        notes_for_solver: String::new(),
    }
}

fn approve(store: &ErrandStore) -> sail_forge::nightshift::Errand {
    let errand = store
        .add(
            "Make the Capital Today tab show the next dividend date.",
            ErrandSource::Web,
            chrono::Utc::now(),
            [1, 2, 3, 4, 5, 6],
        )
        .expect("add");
    store
        .transition(
            &errand.id,
            chrono::Utc::now(),
            Transition::Proposed(order()),
        )
        .expect("propose");
    store
        .transition(&errand.id, chrono::Utc::now(), Transition::Approve(None))
        .expect("approve")
}

fn settings(include_issues: bool) -> NightShiftSettings {
    NightShiftSettings {
        enabled: true,
        include_forge_ready_issues: include_issues,
        ..NightShiftSettings::default()
    }
}

fn runner(
    root: &Path,
    orchestrator: Arc<ForgeOrchestrator>,
    gh: Arc<FakeGithub>,
    exec: Arc<FakeExecutor>,
    store: Arc<ErrandStore>,
    picker: Arc<FakePicker>,
    settings: NightShiftSettings,
) -> ForgeNightlyRunner {
    let config = NightlyConfig {
        state_dir: root.join("state"),
        repo_slug: "local/test-repo".into(),
        base_branch: "main".into(),
        publish: false,
        max_issues_per_run: 5,
        voice_quiet: std::time::Duration::ZERO,
        ..NightlyConfig::default()
    };
    store.set_settings(settings).expect("settings valid");
    ForgeNightlyRunner::new(orchestrator, gh, config)
        .with_executor(exec)
        .with_night_shift(store)
        .with_endpoint_picker(picker)
}

#[tokio::test]
async fn errand_runs_on_the_picked_endpoint_and_owns_its_patch() {
    let tmp = TempDir::new("ready");
    let orchestrator = boot(&tmp.0);
    let gh = Arc::new(FakeGithub::default());
    let exec = FakeExecutor::new(vec![Reply::Report(Outcome::Ready)]);
    let store = Arc::new(ErrandStore::open(&tmp.0.join("state")).expect("store"));
    let errand = approve(&store);
    let picker = Arc::new(FakePicker::new());
    let runner = runner(
        &tmp.0,
        orchestrator.clone(),
        gh,
        exec.clone(),
        store.clone(),
        picker.clone(),
        settings(false),
    );

    let digest = runner.run_once(false, false).await;
    assert_eq!(digest.halted, None);
    let finished = store.get(&errand.id).expect("errand");
    assert!(matches!(finished.state, ErrandState::Ready { .. }));
    assert_eq!(finished.attempts, 1);
    assert_eq!(store.patch(&errand.id).as_deref(), Some(PATCH));

    let state = NightlyState::load(&tmp.0.join("state"));
    let record = state
        .errands
        .get(errand.id.as_str())
        .expect("errand record");
    assert_eq!(record.last_outcome, "ready");
    assert_eq!(record.attempts, 1);
    let job_id = record.last_job_id.as_deref().expect("job id");
    let checkpoint = orchestrator
        .checkpoint_store()
        .load(job_id)
        .expect("checkpoint")
        .expect("checkpoint exists");
    assert_eq!(
        checkpoint.model_endpoint.as_deref(),
        Some("http://mesh.test:8080")
    );
    assert_eq!(picker.mesh_calls.load(Ordering::SeqCst), 1);
    assert_eq!(picker.host_calls.load(Ordering::SeqCst), 0);
    match finished.state {
        ErrandState::Ready { result, .. } => assert_eq!(result.ran_on.model, "test"),
        other => panic!("expected ready, got {other:?}"),
    }

    let morning: sail_forge::nightshift::MorningReport = serde_json::from_str(
        &std::fs::read_to_string(
            tmp.0
                .join("state/nightshift/morning")
                .join(format!("{}.json", chrono::Local::now().format("%Y-%m-%d"))),
        )
        .expect("morning file"),
    )
    .expect("morning json");
    assert_eq!(morning.ready, 1);
    assert_eq!(
        morning.spoken,
        "Overnight I finished 1 of 1 jobs; one is ready to review."
    );
}

#[tokio::test]
async fn mesh_transport_failure_retries_once_on_host_without_spending_an_attempt() {
    let tmp = TempDir::new("fallback");
    let orchestrator = boot(&tmp.0);
    let gh = Arc::new(FakeGithub::default());
    let exec = FakeExecutor::new(vec![
        Reply::Error("Model transport failure at http://mesh.test:8080".into()),
        Reply::Report(Outcome::Ready),
    ]);
    let store = Arc::new(ErrandStore::open(&tmp.0.join("state")).expect("store"));
    let errand = approve(&store);
    let picker = Arc::new(FakePicker::new());
    let runner = runner(
        &tmp.0,
        orchestrator.clone(),
        gh,
        exec.clone(),
        store.clone(),
        picker.clone(),
        settings(false),
    );

    let digest = runner.run_once(false, false).await;
    assert_eq!(digest.halted, None);
    let finished = store.get(&errand.id).expect("errand");
    assert!(matches!(finished.state, ErrandState::Ready { .. }));
    assert_eq!(finished.attempts, 1);
    assert_eq!(exec.calls.load(Ordering::SeqCst), 2);
    assert_eq!(picker.mesh_calls.load(Ordering::SeqCst), 1);
    assert_eq!(picker.host_calls.load(Ordering::SeqCst), 1);

    let state = NightlyState::load(&tmp.0.join("state"));
    let job_id = state.errands[errand.id.as_str()]
        .last_job_id
        .as_deref()
        .expect("job");
    let checkpoint = orchestrator
        .checkpoint_store()
        .load(job_id)
        .expect("checkpoint")
        .expect("checkpoint exists");
    assert_eq!(
        checkpoint.model_endpoint.as_deref(),
        Some("http://host.test:8080")
    );
}

#[tokio::test]
async fn host_failure_requeues_without_spending_the_attempt_and_halts() {
    let tmp = TempDir::new("host-fail");
    let orchestrator = boot(&tmp.0);
    let gh = Arc::new(FakeGithub::default());
    let exec = FakeExecutor::new(vec![
        Reply::Error("Cannot connect to model endpoint http://host.test:8080".into()),
        Reply::Error("Cannot connect to model endpoint http://host.test:8080".into()),
    ]);
    let store = Arc::new(ErrandStore::open(&tmp.0.join("state")).expect("store"));
    let errand = approve(&store);
    let runner = runner(
        &tmp.0,
        orchestrator,
        gh,
        exec,
        store.clone(),
        Arc::new(FakePicker::new()),
        settings(false),
    );

    let digest = runner.run_once(false, false).await;
    assert!(
        digest
            .halted
            .as_deref()
            .is_some_and(|h| h.contains("infrastructure failure"))
    );
    let requeued = store.get(&errand.id).expect("errand");
    assert!(matches!(requeued.state, ErrandState::Queued { .. }));
    assert_eq!(requeued.attempts, 0);
    let state = NightlyState::load(&tmp.0.join("state"));
    assert!(!state.errands.contains_key(errand.id.as_str()));
}

#[tokio::test]
async fn live_voice_requeues_and_retries_without_spending_the_attempt() {
    let tmp = TempDir::new("voice");
    let orchestrator = boot(&tmp.0);
    let exec = FakeExecutor::new(vec![
        Reply::Error("live voice is active; background work paused".into()),
        Reply::Report(Outcome::Ready),
    ]);
    let store = Arc::new(ErrandStore::open(&tmp.0.join("state")).expect("store"));
    let errand = approve(&store);
    let runner = runner(
        &tmp.0,
        orchestrator,
        Arc::new(FakeGithub::default()),
        exec.clone(),
        store.clone(),
        Arc::new(FakePicker::new()),
        settings(false),
    );

    let digest = runner.run_once(false, false).await;
    assert_eq!(digest.halted, None);
    let finished = store.get(&errand.id).expect("errand");
    assert!(matches!(finished.state, ErrandState::Ready { .. }));
    assert_eq!(finished.attempts, 1);
    assert_eq!(exec.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn redo_note_becomes_owner_feedback_for_the_next_solver() {
    let tmp = TempDir::new("redo");
    let orchestrator = boot(&tmp.0);
    let gh = Arc::new(FakeGithub::default());
    let first_exec = FakeExecutor::new(vec![Reply::Report(Outcome::NeedsYou)]);
    let store = Arc::new(ErrandStore::open(&tmp.0.join("state")).expect("store"));
    let errand = approve(&store);
    let runner1 = runner(
        &tmp.0,
        orchestrator.clone(),
        gh.clone(),
        first_exec,
        store.clone(),
        Arc::new(FakePicker::new()),
        settings(false),
    );
    runner1.run_once(false, false).await;
    assert!(matches!(
        store.get(&errand.id).expect("errand").state,
        ErrandState::NeedsYou { .. }
    ));

    store
        .transition(
            &errand.id,
            chrono::Utc::now(),
            Transition::Redo(sail_forge::nightshift::OwnerNote {
                at: chrono::Utc::now(),
                text: "Please keep the label short.".into(),
            }),
        )
        .expect("redo");

    let second_exec = FakeExecutor::new(vec![Reply::Report(Outcome::Ready)]);
    let runner2 = runner(
        &tmp.0,
        boot(&tmp.0.join("night2")),
        gh,
        second_exec.clone(),
        store.clone(),
        Arc::new(FakePicker::new()),
        settings(false),
    );
    let digest2 = runner2.run_once(false, false).await;
    assert_eq!(digest2.halted, None);
    let bodies = second_exec.bodies.lock().expect("bodies").clone();
    assert_eq!(bodies.len(), 1);
    assert!(bodies[0].contains("## Owner feedback"));
    assert!(bodies[0].contains("Please keep the label short."));
    let finished = store.get(&errand.id).expect("errand");
    assert!(matches!(finished.state, ErrandState::Ready { .. }));
    assert_eq!(finished.attempts, 2);
}

#[tokio::test]
async fn ready_issue_waits_for_approval_when_publishing_is_off() {
    let tmp = TempDir::new("issue-review");
    let orchestrator = boot(&tmp.0);
    let gh = Arc::new(FakeGithub {
        issues: vec![IssueCandidate {
            number: 125,
            title: "Search crash".into(),
            body: "## Acceptance\n- [ ] fixed\n\n## Target files\n- `src/lib.rs`\n".into(),
            labels: vec!["forge-ready".into()],
            has_open_pr: false,
        }],
        created: Mutex::new(Vec::new()),
    });
    let exec = FakeExecutor::new(vec![Reply::Report(Outcome::Ready)]);
    let store = Arc::new(ErrandStore::open(&tmp.0.join("state")).expect("store"));
    let runner = runner(
        &tmp.0,
        orchestrator,
        gh.clone(),
        exec,
        store,
        Arc::new(FakePicker::new()),
        settings(true),
    );

    let digest = runner.run_once(false, false).await;
    assert_eq!(digest.halted, None);
    let state = NightlyState::load(&tmp.0.join("state"));
    assert_eq!(state.issues[&125].last_outcome, "ready");
    assert!(state.issues[&125].pr_url.is_none());
    assert!(gh.created.lock().expect("created").is_empty());
}

#[tokio::test]
async fn external_budget_allows_exactly_one_call_for_two_failed_errands() {
    let tmp = TempDir::new("budget");
    let orchestrator = boot(&tmp.0);
    let exec = FakeExecutor::new(vec![
        Reply::Report(Outcome::NeedsYou),
        Reply::Report(Outcome::NeedsYou),
    ]);
    let store = Arc::new(ErrandStore::open(&tmp.0.join("state")).expect("store"));
    let first = approve(&store);
    let second = store
        .add("Another job", ErrandSource::Web, chrono::Utc::now(), [7; 6])
        .expect("add");
    store
        .transition(
            &second.id,
            chrono::Utc::now(),
            Transition::Proposed(order()),
        )
        .expect("propose");
    store
        .transition(&second.id, chrono::Utc::now(), Transition::Approve(None))
        .expect("approve");
    let solver = Arc::new(FakeExternal {
        calls: AtomicUsize::new(0),
    });
    let mut budget = settings(false);
    budget.external_solves_per_night = 1;
    budget.external_provider = Some("claude_cli".into());
    let runner = runner(
        &tmp.0,
        orchestrator,
        Arc::new(FakeGithub::default()),
        exec,
        store.clone(),
        Arc::new(FakePicker::new()),
        budget,
    )
    .with_external_solver(solver.clone());

    let digest = runner.run_once(false, false).await;
    assert_eq!(digest.halted, None);
    assert_eq!(solver.calls.load(Ordering::SeqCst), 1);
    let state = NightlyState::load(&tmp.0.join("state"));
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    assert_eq!(state.external_used.get(&today), Some(&1));
    let first = store.get(&first.id).expect("first");
    let second = store.get(&second.id).expect("second");
    assert!(first.external_used);
    assert!(!second.external_used);
}

#[tokio::test]
async fn the_runner_window_comes_from_live_store_settings() {
    let tmp = TempDir::new("window");
    let orchestrator = boot(&tmp.0);
    let exec = FakeExecutor::new(Vec::new());
    let store = Arc::new(ErrandStore::open(&tmp.0.join("state")).expect("store"));
    let mut windowless = settings(false);
    windowless.window_start_hour = 0;
    windowless.window_end_hour = 0;
    store.set_settings(windowless).expect("settings");
    let runner = runner(
        &tmp.0,
        orchestrator,
        Arc::new(FakeGithub::default()),
        exec,
        store,
        Arc::new(FakePicker::new()),
        settings(false),
    );
    let digest = runner.run_once(false, true).await;
    assert_eq!(
        digest.halted.as_deref(),
        Some("outside the configured run window")
    );
}

#[tokio::test]
async fn dry_run_plans_without_executing_or_saving_a_result() {
    let tmp = TempDir::new("dry");
    let orchestrator = boot(&tmp.0);
    let gh = Arc::new(FakeGithub::default());
    let exec = FakeExecutor::new(vec![Reply::Report(Outcome::Ready)]);
    let store = Arc::new(ErrandStore::open(&tmp.0.join("state")).expect("store"));
    let errand = approve(&store);
    let runner = runner(
        &tmp.0,
        orchestrator,
        gh,
        exec.clone(),
        store.clone(),
        Arc::new(FakePicker::new()),
        settings(false),
    );

    let digest = runner.run_once(true, false).await;
    assert_eq!(digest.entries[0].outcome, "would_attempt");
    assert_eq!(exec.calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        store.get(&errand.id).expect("errand").state,
        ErrandState::Queued { .. }
    ));
}

#[tokio::test]
async fn disabled_settings_leave_the_night_idle() {
    let tmp = TempDir::new("off");
    let orchestrator = boot(&tmp.0);
    let exec = FakeExecutor::new(Vec::new());
    let store = Arc::new(ErrandStore::open(&tmp.0.join("state")).expect("store"));
    let errand = approve(&store);
    let mut off = settings(false);
    off.enabled = false;
    let runner = runner(
        &tmp.0,
        orchestrator,
        Arc::new(FakeGithub::default()),
        exec.clone(),
        store.clone(),
        Arc::new(FakePicker::new()),
        off,
    );

    let digest = runner.run_once(false, false).await;
    assert_eq!(
        digest.halted.as_deref(),
        Some("night shift is disabled in Settings")
    );
    assert_eq!(exec.calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        store.get(&errand.id).expect("errand").state,
        ErrandState::Queued { .. }
    ));
}

#[test]
fn old_state_files_without_errands_still_load() {
    let tmp = TempDir::new("state");
    std::fs::create_dir_all(tmp.0.join("state")).expect("state");
    std::fs::write(
        tmp.0.join("state/nightly_state.json"),
        r#"{"issues":{},"prs_opened":{},"last_run_date":"2026-11-17"}"#,
    )
    .expect("old state");
    let state = NightlyState::load(&tmp.0.join("state"));
    assert!(state.errands.is_empty());
    assert_eq!(state.last_run_date.as_deref(), Some("2026-11-17"));
}

#[test]
fn planning_inputs_round_trip_to_the_public_plan_types() {
    let id = sail_forge::nightshift::ErrandId::new([9; 6]);
    let errand = PlanErrand {
        id,
        size: Size::S,
        approved_at: chrono::Utc::now(),
        has_redo_note: true,
        attempts: 0,
    };
    let issue = PlanIssue { number: 125 };
    let plan = sail_forge::nightshift::plan::plan_night(
        &[errand],
        &[issue],
        12 * 3_600,
        5,
        &SizeDurations::default(),
    );
    assert_eq!(plan.items.len(), 2);
}

#[test]
fn endpoints_compare_by_url_and_machine_and_never_print_the_client() {
    let a = Endpoint {
        url: "https://mesh.test".into(),
        ran_on: mesh(),
        client: Some(reqwest::Client::new()),
    };
    let b = Endpoint {
        url: "https://mesh.test".into(),
        ran_on: mesh(),
        client: None,
    };
    assert_eq!(a, b);
    let debug = format!("{a:?}");
    assert!(debug.contains("client: \"<set>\""));
    assert!(!debug.contains("bearer"));
    let plain = format!("{b:?}");
    assert!(plain.contains("client: \"<none>\""));

    let controller = sail_forge::solver::LocalSolverController::new(1, 10, None)
        .with_job_endpoint("https://mesh.test".into(), a.client.clone());
    assert_eq!(
        controller.endpoint_override,
        Some(vec!["https://mesh.test".to_string()])
    );
    assert!(controller.endpoint_client.is_some());
    let probing = sail_forge::solver::LocalSolverController::new(1, 10, None)
        .with_job_endpoint(String::new(), None);
    assert!(probing.endpoint_override.is_none());
}

#[test]
fn ship_patch_rejects_empty_patches_and_bad_branches_before_touching_git() {
    let tmp = TempDir::new("ship");
    let config = sail_forge::publish::PublishConfig {
        repo_root: tmp.0.clone(),
        repo_slug: "local/test-repo".into(),
        scratch_dir: tmp.0.join("scratch"),
        base_branch: "main".into(),
        labels: vec!["forge-generated".into()],
    };
    assert_eq!(
        sail_forge::publish::ship_patch(&config, "nightshift/e-1", "t", "b", "  "),
        Err("the stored patch is empty".into())
    );
    assert!(
        sail_forge::publish::ship_patch(&config, "../bad", "t", "b", PATCH)
            .expect_err("bad branch")
            .contains("invalid Night Shift branch")
    );
    assert!(
        sail_forge::publish::ship_patch(&config, "-bad", "t", "b", PATCH)
            .expect_err("bad branch")
            .contains("invalid Night Shift branch")
    );
}
