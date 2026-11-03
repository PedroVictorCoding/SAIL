//! Forge on SAIL Continuity: what happens to jobs across a daemon restart.
//!
//! A "crash" is simulated by dropping the orchestrator and its Continuity store while a job
//! holds a lease, then building fresh ones over the same files, which starts a new boot.

#![allow(clippy::field_reassign_with_default)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use uuid::Uuid;

use sail_continuity::{Continuity, TaskState};
use sail_forge::contracts::{ForgeTaskState, IssueEnqueueRequest, ReviewPolicyConfig};
use sail_forge::orchestrator::{ForgeOrchestrator, OrchestratorConfig};
use sail_forge::persistence::TaskCheckpointStore;
use sail_forge::reviewer::ReviewBroker;
use sail_guardian::sandbox::SandboxPolicy;

const PATCH: &str = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n pub fn compute() -> i32 { 42 }\n+// Resumed after restart\n";

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("forge_continuity_{name}_{}", Uuid::new_v4()));
        let repo = root.join("repo");
        fs::create_dir_all(repo.join("src")).unwrap();
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
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@test.local"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self { root }
    }

    /// A daemon "boot": a fresh orchestrator over the same checkpoint dir and Continuity DB.
    fn boot(&self) -> Arc<ForgeOrchestrator> {
        let mut config = OrchestratorConfig::default();
        config.workspace_root = self.root.join("repo");
        let continuity = Arc::new(Continuity::open(self.root.join("state/continuity.db")).unwrap());
        Arc::new(
            ForgeOrchestrator::new(
                config,
                Arc::new(TaskCheckpointStore::new(
                    self.root.join("state/checkpoints"),
                )),
                Arc::new(
                    ReviewBroker::new(ReviewPolicyConfig::default())
                        .with_mock_mode(true)
                        .with_ledger_path(self.root.join("ledger.json")),
                ),
                Arc::new(SandboxPolicy::new()),
            )
            .with_continuity(continuity),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn request(issue: u64) -> IssueEnqueueRequest {
    IssueEnqueueRequest {
        repo: "local/test-repo".into(),
        issue_number: issue,
        issue_title: Some("Add a note".into()),
        issue_body: Some("Add a comment to src/lib.rs".into()),
        ..Default::default()
    }
}

/// The workspace the crashed attempt left behind, as a real crash would.
fn worktree_base() -> PathBuf {
    std::env::var("SAIL_FORGE_WORKTREE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("sail_forge_worktrees"))
}

fn leave_crashed_workspace(job_id: &str) {
    let base = worktree_base();
    fs::create_dir_all(&base).unwrap();
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(base.join(job_id)).unwrap();
    fs::set_permissions(base.join(job_id), fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
async fn interrupted_job_resumes_after_restart_and_finishes() {
    let fx = Fixture::new("resume");
    let job_id = {
        let forge = fx.boot();
        let job_id = forge.enqueue_issue(request(11)).unwrap();
        // The worker takes the job, then the daemon dies mid-run.
        forge.continuity().claim(&job_id).unwrap();
        leave_crashed_workspace(&job_id);
        job_id
    };

    let forge = fx.boot();
    assert_eq!(
        forge.continuity().get(&job_id).unwrap().state,
        TaskState::Running,
        "before recovery the store still shows the dead boot's lease"
    );
    let resumed = forge.recover_from_checkpoints().unwrap();
    assert_eq!(resumed, vec![job_id.clone()]);
    let view = forge.get_status(&job_id).unwrap().continuity.unwrap();
    assert_eq!(view.state, "interrupted");
    assert!(view.reason.unwrap().contains("restart"));

    let report = forge.run_job_lifecycle(&job_id, Some(PATCH)).await.unwrap();
    assert_eq!(report.status, ForgeTaskState::Completed);
    let task = forge.continuity().get(&job_id).unwrap();
    assert_eq!(task.state, TaskState::Succeeded);
    assert_eq!(task.attempt, 2, "the re-run is a new attempt");
    let stages: Vec<String> = forge
        .continuity()
        .events(&job_id, 0, 500)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert!(stages.contains(&"interrupted".to_string()));
    assert!(stages.contains(&"planning".to_string()), "{stages:?}");
    let _ = fs::remove_dir(worktree_base().join(&job_id));
}

#[tokio::test]
async fn cancellation_before_a_crash_is_honoured_not_resumed() {
    let fx = Fixture::new("cancel");
    let job_id = {
        let forge = fx.boot();
        let job_id = forge.enqueue_issue(request(12)).unwrap();
        forge.continuity().claim(&job_id).unwrap();
        forge.cancel_job(&job_id, "user pressed stop").unwrap();
        job_id
    };

    let forge = fx.boot();
    assert!(forge.recover_from_checkpoints().unwrap().is_empty());
    let status = forge.get_status(&job_id).unwrap();
    assert_eq!(status.state, ForgeTaskState::Blocked);
    assert_eq!(status.continuity.unwrap().state, "cancelled");
    assert!(forge.run_job_lifecycle(&job_id, Some(PATCH)).await.is_err());
}

#[tokio::test]
async fn a_job_waiting_for_the_slot_can_be_cancelled_after_restart() {
    let fx = Fixture::new("cancel_stored");
    let job_id = {
        let forge = fx.boot();
        forge.enqueue_issue(request(15)).unwrap()
    };
    let forge = fx.boot();
    // Not recovered into memory yet; cancelling still works through the durable record.
    forge.cancel_job("15", "changed my mind").unwrap();
    assert_eq!(
        forge.continuity().get(&job_id).unwrap().state,
        TaskState::Cancelled
    );
    assert!(forge.recover_from_checkpoints().unwrap().is_empty());
}

#[test]
fn duplicate_enqueue_returns_the_active_job() {
    let fx = Fixture::new("dupe");
    let forge = fx.boot();
    let first = forge.enqueue_issue(request(13)).unwrap();
    let second = forge.enqueue_issue(request(13)).unwrap();
    assert_eq!(first, second);
    assert_eq!(forge.list_all_jobs().len(), 1);
    // The nightly runner must not silently share a user's job.
    assert!(forge.enqueue_managed_issue(request(13)).is_err());
    // A different issue is independent.
    assert_ne!(forge.enqueue_issue(request(14)).unwrap(), first);
}

#[test]
fn interrupted_nightly_job_fails_instead_of_running_unattended() {
    let fx = Fixture::new("nightly");
    let (running, queued) = {
        let forge = fx.boot();
        let running = forge.enqueue_managed_issue(request(21)).unwrap();
        forge.continuity().claim(&running).unwrap();
        let queued = forge.enqueue_managed_issue(request(22)).unwrap();
        (running, queued)
    };
    let forge = fx.boot();
    assert!(forge.recover_from_checkpoints().unwrap().is_empty());
    for id in [&running, &queued] {
        let status = forge.get_status(id).unwrap();
        assert_eq!(status.state, ForgeTaskState::Blocked, "{id}");
        let view = status.continuity.unwrap();
        assert_eq!(view.state, "failed");
        assert!(status.failure_reason.is_some());
    }
}

#[test]
fn job_whose_checkpoint_was_lost_fails_explicitly() {
    let fx = Fixture::new("lost");
    let job_id = {
        let forge = fx.boot();
        let job_id = forge.enqueue_issue(request(31)).unwrap();
        forge.continuity().claim(&job_id).unwrap();
        job_id
    };
    fs::remove_file(fx.root.join(format!("state/checkpoints/{job_id}.json"))).unwrap();
    let forge = fx.boot();
    assert!(forge.recover_from_checkpoints().unwrap().is_empty());
    let task = forge.continuity().get(&job_id).unwrap();
    assert_eq!(task.state, TaskState::Failed);
    assert!(task.reason.unwrap().contains("checkpoint missing"));
}

#[test]
fn pre_continuity_checkpoints_are_adopted_and_resumed() {
    let fx = Fixture::new("legacy");
    let job_id = fx.boot().enqueue_issue(request(41)).unwrap();
    // Simulate a store written before Continuity existed.
    fs::remove_file(fx.root.join("state/continuity.db")).unwrap();
    let _ = fs::remove_file(fx.root.join("state/continuity.db-wal"));
    let _ = fs::remove_file(fx.root.join("state/continuity.db-shm"));

    let forge = fx.boot();
    assert_eq!(
        forge.recover_from_checkpoints().unwrap(),
        vec![job_id.clone()]
    );
    assert_eq!(
        forge.continuity().get(&job_id).unwrap().state,
        TaskState::Queued
    );
}

#[tokio::test]
async fn job_cancelled_while_waiting_for_the_slot_never_starts() {
    let fx = Fixture::new("cancel_waiting");
    let forge = fx.boot();
    let job_id = forge.enqueue_issue(request(16)).unwrap();
    forge.cancel_job(&job_id, "not needed").unwrap();
    let err = forge
        .run_job_lifecycle(&job_id, Some(PATCH))
        .await
        .unwrap_err();
    assert!(err.contains("cancelled before it started"), "{err}");
    assert_eq!(
        forge.continuity().get(&job_id).unwrap().state,
        TaskState::Cancelled
    );
}
