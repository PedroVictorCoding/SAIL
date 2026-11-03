#![allow(clippy::field_reassign_with_default)]

use async_trait::async_trait;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use sail_continuity::{Continuity, EffectGate, TaskState};
use sail_forge::admission::IssueCandidate;
use sail_forge::contracts::{ForgeJobReport, ReviewPolicyConfig, ReviewVerdict};
use sail_forge::github::{DraftPr, GithubClient, OpenPullRequest};
use sail_forge::nightly::{ForgeNightlyRunner, JobExecutor, NightlyConfig, NightlyState};
use sail_forge::orchestrator::{EFFECT_PUBLISH, ForgeOrchestrator, OrchestratorConfig};
use sail_forge::persistence::TaskCheckpointStore;
use sail_forge::reviewer::{ReviewBroker, ReviewError};
use sail_forge::self_review::{ReviewPacket, ReviewProvider};
use sail_guardian::sandbox::SandboxPolicy;

const GOOD_PATCH: &str = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n pub fn compute() -> i32 { 42 }\n+// Verified by Forge nightly\n";
const GATE_FAILING_PATCH: &str = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n pub fn compute() -> i32 { 42 }\n+pub fn parse() -> u32 { \"1\".parse().unwrap() }\n";
const READY_BODY: &str = "## Problem\nAdd a note.\n\n## Target files\n- `src/lib.rs`\n\n## Acceptance\n- [ ] note added\n";

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A checkout with a bare `origin`, both on `main`.
fn setup_repo_with_origin(root: &Path) -> (PathBuf, PathBuf) {
    let origin = root.join("origin.git");
    let repo = root.join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--bare", "-b", "main"]);
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"test_pkg\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn compute() -> i32 { 42 }\n#[test]\nfn test_compute() { assert_eq!(compute(), 42); }\n",
    )
    .unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "user.email", "test@test.local"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "init"]);
    git(
        &repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    (repo, origin)
}

fn remote_has_branch(origin: &Path, branch: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .current_dir(origin)
        .output()
        .unwrap()
        .status
        .success()
}

#[derive(Default)]
struct FakeGithub {
    issues: Vec<IssueCandidate>,
    prs: Vec<OpenPullRequest>,
    created: Mutex<Vec<DraftPr>>,
}

impl GithubClient for FakeGithub {
    fn list_open_issues(&self, _repo: &str, _label: &str) -> Result<Vec<IssueCandidate>, String> {
        Ok(self.issues.clone())
    }
    fn open_pull_requests(&self, _repo: &str) -> Result<Vec<OpenPullRequest>, String> {
        Ok(self.prs.clone())
    }
    fn create_draft_pr(&self, _repo: &str, pr: &DraftPr) -> Result<String, String> {
        let mut created = self.created.lock().unwrap();
        created.push(pr.clone());
        Ok(format!("https://example.test/pull/{}", created.len()))
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
            untested_edge_cases: vec![],
            detected_bugs: vec![],
            security_observations: vec![],
            suggested_edits: vec![],
            confidence_score: 0.9,
            raw_feedback: String::new(),
        })
    }
}

/// Applies scripted patches in order (the last one repeats) and records the issue bodies.
struct ScriptedExecutor {
    patches: Vec<&'static str>,
    calls: AtomicUsize,
    bodies: Mutex<Vec<String>>,
}

impl ScriptedExecutor {
    fn new(patches: Vec<&'static str>) -> Arc<Self> {
        Arc::new(Self {
            patches,
            calls: AtomicUsize::new(0),
            bodies: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl JobExecutor for ScriptedExecutor {
    async fn execute(
        &self,
        orchestrator: &ForgeOrchestrator,
        job_id: &str,
    ) -> Result<ForgeJobReport, String> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let cp = orchestrator
            .checkpoint_store()
            .load(job_id)
            .unwrap()
            .unwrap();
        self.bodies
            .lock()
            .unwrap()
            .push(cp.issue_body.unwrap_or_default());
        let patch = self.patches[n.min(self.patches.len() - 1)];
        orchestrator.run_job_lifecycle(job_id, Some(patch)).await
    }
}

struct Harness {
    root: PathBuf,
    repo: PathBuf,
    origin: PathBuf,
    orchestrator: Arc<ForgeOrchestrator>,
    config: NightlyConfig,
}

/// A daemon boot: a fresh orchestrator over the harness's durable files.
fn boot(root: &Path, repo: &Path) -> Arc<ForgeOrchestrator> {
    let mut config = OrchestratorConfig::default();
    config.workspace_root = repo.to_path_buf();
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
            Continuity::open(root.join("continuity.db")).unwrap(),
        )),
    )
}

impl Harness {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("forge_nightly_{name}_{}", Uuid::new_v4()));
        let (repo, origin) = setup_repo_with_origin(&root);
        let orchestrator = boot(&root, &repo);
        let mut nightly = NightlyConfig::default();
        nightly.state_dir = root.join("state");
        Self {
            root,
            repo,
            origin,
            orchestrator,
            config: nightly,
        }
    }

    /// Simulate a daemon restart: new boot, recovery as the Forge subsystem runs it.
    fn restart(&mut self) {
        self.orchestrator = boot(&self.root, &self.repo);
        self.orchestrator.recover_from_checkpoints().unwrap();
    }

    fn runner(
        &self,
        gh: Arc<FakeGithub>,
        exec: Arc<ScriptedExecutor>,
        config: NightlyConfig,
    ) -> ForgeNightlyRunner {
        ForgeNightlyRunner::new(self.orchestrator.clone(), gh, config).with_executor(exec)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn issue(number: u64, labels: &[&str]) -> IssueCandidate {
    IssueCandidate {
        number,
        title: format!("Issue {number}"),
        body: READY_BODY.into(),
        labels: labels.iter().map(|s| s.to_string()).collect(),
        has_open_pr: false,
    }
}

#[tokio::test]
async fn retries_with_feedback_then_publishes_once() {
    let h = Harness::new("publish");
    let gh = Arc::new(FakeGithub {
        issues: vec![
            issue(1, &["forge-ready"]),
            issue(2, &[]),
            issue(3, &["forge-ready"]),
        ],
        prs: vec![OpenPullRequest {
            number: 99,
            head_branch: "agent/forge-issue-3".into(),
            body: String::new(),
        }],
        ..Default::default()
    });
    let exec = ScriptedExecutor::new(vec![GATE_FAILING_PATCH, GOOD_PATCH]);
    let runner = h.runner(gh.clone(), exec.clone(), h.config.clone());

    let digest = runner.run_once(false, false).await;
    assert!(digest.halted.is_none(), "{:?}", digest.halted);
    assert_eq!(exec.calls.load(Ordering::SeqCst), 2);

    let bodies = exec.bodies.lock().unwrap().clone();
    assert!(!bodies[0].contains("Previous attempt feedback"));
    assert!(bodies[1].contains("Previous attempt feedback"));
    assert!(bodies[1].contains("new_unwrap"));

    assert_eq!(digest.entries.len(), 1);
    assert_eq!(digest.entries[0].issue, 1);
    assert_eq!(digest.entries[0].outcome, "published");
    let rejected: Vec<u64> = digest.rejected.iter().map(|(n, _)| *n).collect();
    assert_eq!(rejected, vec![2, 3]);

    assert!(remote_has_branch(&h.origin, "agent/forge-issue-1"));
    {
        let created = gh.created.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].head_branch, "agent/forge-issue-1");
        assert_eq!(created[0].base_branch, "main");
        assert!(created[0].body.contains("Closes #1"));
        assert_eq!(created[0].labels, vec!["forge-generated".to_string()]);
    }
    assert!(
        h.config
            .state_dir
            .join("digests")
            .join(format!("{}.md", digest.date))
            .exists()
    );

    // A restarted runner reads the durable state and does not redo or republish.
    let restarted = h.runner(gh.clone(), exec.clone(), h.config.clone());
    let again = restarted.run_once(false, false).await;
    assert!(again.entries.is_empty());
    assert_eq!(exec.calls.load(Ordering::SeqCst), 2);
    assert_eq!(gh.created.lock().unwrap().len(), 1);

    let state = NightlyState::load(&h.config.state_dir);
    let rec = &state.issues[&1];
    assert_eq!(rec.attempts, 2);
    assert_eq!(rec.pr_url.as_deref(), Some("https://example.test/pull/1"));
}

#[tokio::test]
async fn pr_cap_defers_publication_without_resolving_again() {
    let h = Harness::new("cap");
    let gh = Arc::new(FakeGithub {
        issues: vec![issue(5, &["forge-ready"])],
        ..Default::default()
    });
    let exec = ScriptedExecutor::new(vec![GOOD_PATCH]);

    let mut capped = h.config.clone();
    capped.max_prs_per_day = 0;
    let digest = h
        .runner(gh.clone(), exec.clone(), capped)
        .run_once(false, false)
        .await;
    assert_eq!(digest.entries[0].outcome, "ready");
    assert!(gh.created.lock().unwrap().is_empty());
    assert!(!remote_has_branch(&h.origin, "agent/forge-issue-5"));

    let digest = h
        .runner(gh.clone(), exec.clone(), h.config.clone())
        .run_once(false, false)
        .await;
    assert_eq!(exec.calls.load(Ordering::SeqCst), 1, "must not solve again");
    assert_eq!(digest.entries[0].outcome, "published");
    assert!(remote_has_branch(&h.origin, "agent/forge-issue-5"));
}

#[tokio::test]
async fn kill_switch_halts_before_any_work() {
    let h = Harness::new("pause");
    fs::create_dir_all(&h.config.state_dir).unwrap();
    fs::write(h.config.kill_switch_path(), "").unwrap();
    let gh = Arc::new(FakeGithub {
        issues: vec![issue(1, &["forge-ready"])],
        ..Default::default()
    });
    let exec = ScriptedExecutor::new(vec![GOOD_PATCH]);
    let digest = h
        .runner(gh, exec.clone(), h.config.clone())
        .run_once(false, false)
        .await;
    assert!(digest.halted.unwrap().contains("kill-switch"));
    assert_eq!(exec.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn attempts_are_bounded_and_exhaustion_is_reported() {
    let h = Harness::new("exhaust");
    let gh = Arc::new(FakeGithub {
        issues: vec![issue(8, &["forge-ready"])],
        ..Default::default()
    });
    let exec = ScriptedExecutor::new(vec![GATE_FAILING_PATCH]);
    let digest = h
        .runner(gh.clone(), exec.clone(), h.config.clone())
        .run_once(false, false)
        .await;
    assert_eq!(exec.calls.load(Ordering::SeqCst), 2);
    assert_eq!(digest.entries[0].outcome, "failed after 2 attempts");
    assert!(gh.created.lock().unwrap().is_empty());

    let digest = h
        .runner(gh.clone(), exec.clone(), h.config.clone())
        .run_once(false, false)
        .await;
    assert_eq!(exec.calls.load(Ordering::SeqCst), 2);
    assert!(digest.rejected[0].1[0].contains("attempts exhausted"));
}

#[tokio::test]
async fn dry_run_selects_without_executing_or_writing() {
    let h = Harness::new("dry");
    let gh = Arc::new(FakeGithub {
        issues: vec![issue(1, &["forge-ready"]), issue(2, &[])],
        ..Default::default()
    });
    let exec = ScriptedExecutor::new(vec![GOOD_PATCH]);
    let digest = h
        .runner(gh, exec.clone(), h.config.clone())
        .run_once(true, false)
        .await;
    assert_eq!(digest.entries[0].outcome, "would_attempt");
    assert_eq!(digest.rejected[0].0, 2);
    assert_eq!(exec.calls.load(Ordering::SeqCst), 0);
    assert!(!h.config.state_dir.join("nightly_state.json").exists());
}

struct RejectingReviewer;

#[async_trait]
impl ReviewProvider for RejectingReviewer {
    fn name(&self) -> String {
        "rejecting-reviewer".into()
    }
    async fn review(&self, _packet: &ReviewPacket) -> Result<ReviewVerdict, ReviewError> {
        Ok(sail_forge::self_review::parse_verdict(
            r#"{"passed": false, "requirements_covered": false, "detected_bugs": ["note is in the wrong place"]}"#,
        ))
    }
}

#[tokio::test]
async fn self_review_rejection_blocks_before_external_review() {
    let root = std::env::temp_dir().join(format!("forge_selfreview_{}", Uuid::new_v4()));
    let (repo, _origin) = setup_repo_with_origin(&root);
    let mut config = OrchestratorConfig::default();
    config.workspace_root = repo;
    let broker = Arc::new(
        ReviewBroker::new(ReviewPolicyConfig::default())
            .with_mock_mode(true)
            .with_ledger_path(root.join("ledger.json")),
    );
    let store = Arc::new(TaskCheckpointStore::new(root.join("checkpoints")));
    let orchestrator = ForgeOrchestrator::new(
        config,
        store.clone(),
        broker.clone(),
        Arc::new(SandboxPolicy::new()),
    )
    .with_self_reviewer(Arc::new(RejectingReviewer));

    let job_id = orchestrator
        .enqueue_managed_issue(sail_forge::contracts::IssueEnqueueRequest {
            repo: "PedroVictorCoding/SAIL".into(),
            issue_number: 11,
            issue_body: Some(READY_BODY.into()),
            ..Default::default()
        })
        .unwrap();
    assert!(orchestrator.is_managed(&job_id));

    let err = orchestrator
        .run_job_lifecycle(&job_id, Some(GOOD_PATCH))
        .await
        .unwrap_err();
    assert!(err.contains("wrong place"), "{err}");
    let cp = store.load(&job_id).unwrap().unwrap();
    assert_eq!(
        cp.verification_status.as_deref(),
        Some("self_review_rejected")
    );
    assert!(broker.get_ledger().canonical_issue_attempts.is_empty());
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn publish_refuses_existing_remote_branch() {
    let h = Harness::new("existing");
    // Someone already pushed the Forge branch for this issue.
    let repo = h.orchestrator.config().workspace_root.clone();
    git(
        &repo,
        &[
            "push",
            "-q",
            "origin",
            "main:refs/heads/agent/forge-issue-6",
        ],
    );

    let gh = Arc::new(FakeGithub {
        issues: vec![issue(6, &["forge-ready"])],
        ..Default::default()
    });
    let exec = ScriptedExecutor::new(vec![GOOD_PATCH]);
    let digest = h
        .runner(gh.clone(), exec, h.config.clone())
        .run_once(false, false)
        .await;
    assert_eq!(digest.entries[0].outcome, "publish_failed");
    assert!(digest.entries[0].detail.contains("already exists"));
    assert!(gh.created.lock().unwrap().is_empty());
}

#[tokio::test]
async fn publish_interrupted_by_a_crash_is_never_repeated_blindly() {
    let mut h = Harness::new("crash_publish");
    let gh = Arc::new(FakeGithub {
        issues: vec![issue(6, &["forge-ready"])],
        ..Default::default()
    });
    let exec = ScriptedExecutor::new(vec![GOOD_PATCH]);

    // A verified job held back by the PR cap.
    let mut capped = h.config.clone();
    capped.max_prs_per_day = 0;
    let digest = h
        .runner(gh.clone(), exec.clone(), capped)
        .run_once(false, false)
        .await;
    assert_eq!(digest.entries[0].outcome, "ready");
    let job_id = NightlyState::load(&h.config.state_dir).issues[&6]
        .last_job_id
        .clone()
        .unwrap();
    assert_eq!(
        h.orchestrator.continuity().get(&job_id).unwrap().state,
        TaskState::Succeeded
    );

    // The next publish attempt journals its intent, then the daemon dies before the
    // receipt: whether the push and PR happened is unknown.
    assert_eq!(
        h.orchestrator
            .continuity()
            .begin_effect(&job_id, EFFECT_PUBLISH, "push and open draft PR")
            .unwrap(),
        EffectGate::Proceed
    );
    h.restart();

    let task = h.orchestrator.continuity().get(&job_id).unwrap();
    assert_eq!(task.state, TaskState::NeedsIntervention);
    let digest = h
        .runner(gh.clone(), exec.clone(), h.config.clone())
        .run_once(false, false)
        .await;
    assert_eq!(digest.entries[0].outcome, "needs_intervention");
    assert!(
        gh.created.lock().unwrap().is_empty(),
        "must not open a PR blindly"
    );
    assert!(!remote_has_branch(&h.origin, "agent/forge-issue-6"));
    assert_eq!(exec.calls.load(Ordering::SeqCst), 1, "must not solve again");

    // Later passes keep the issue parked and say why.
    let digest = h
        .runner(gh.clone(), exec.clone(), h.config.clone())
        .run_once(false, false)
        .await;
    assert!(digest.entries.is_empty());
    let (_, why) = digest.rejected.iter().find(|(n, _)| *n == 6).unwrap();
    assert!(why[0].contains("needs intervention"), "{why:?}");
    assert_eq!(exec.calls.load(Ordering::SeqCst), 1);
    assert!(gh.created.lock().unwrap().is_empty());
}

#[tokio::test]
async fn publish_receipt_survives_a_crash_before_the_runner_saves_state() {
    let h = Harness::new("receipt");
    let gh = Arc::new(FakeGithub {
        issues: vec![issue(7, &["forge-ready"])],
        ..Default::default()
    });
    let exec = ScriptedExecutor::new(vec![GOOD_PATCH]);
    let mut capped = h.config.clone();
    capped.max_prs_per_day = 0;
    h.runner(gh.clone(), exec.clone(), capped)
        .run_once(false, false)
        .await;
    let job_id = NightlyState::load(&h.config.state_dir).issues[&7]
        .last_job_id
        .clone()
        .unwrap();
    // The PR was opened and journaled, but the runner died before saving its own state.
    let continuity = h.orchestrator.continuity();
    continuity
        .begin_effect(&job_id, EFFECT_PUBLISH, "push and open draft PR")
        .unwrap();
    continuity
        .commit_effect(&job_id, EFFECT_PUBLISH, "https://example.test/pull/41")
        .unwrap();

    let digest = h
        .runner(gh.clone(), exec.clone(), h.config.clone())
        .run_once(false, false)
        .await;
    assert_eq!(digest.entries[0].outcome, "published");
    assert_eq!(digest.entries[0].detail, "https://example.test/pull/41");
    assert!(
        gh.created.lock().unwrap().is_empty(),
        "receipt reused, no second PR"
    );
    let state = NightlyState::load(&h.config.state_dir);
    assert_eq!(
        state.issues[&7].pr_url.as_deref(),
        Some("https://example.test/pull/41")
    );
}
