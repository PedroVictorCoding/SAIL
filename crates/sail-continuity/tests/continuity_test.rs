use std::path::PathBuf;

use sail_continuity::{
    CancelOutcome, Continuity, ContinuityError, EffectGate, MAX_CHECKPOINT_BYTES,
    MAX_EVENTS_PER_TASK, Outcome, Resolution, ResumePolicy, TaskFilter, TaskSpec, TaskState,
};

fn db() -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("sail_continuity_{}", uuid::Uuid::new_v4()));
    (dir.join("continuity.db"), dir)
}

fn spec(id: &str, resume: ResumePolicy) -> TaskSpec {
    TaskSpec::new("sail-forge", "test.kind", format!("task {id}"))
        .with_id(id)
        .with_resume(resume)
}

#[test]
fn task_lifecycle_is_durable_across_reopen() {
    let (path, dir) = db();
    {
        let c = Continuity::open(&path).unwrap();
        assert!(c.is_durable());
        c.submit(spec("a", ResumePolicy::Restart)).unwrap();
        let lease = c.claim("a").unwrap();
        c.progress(&lease, "testing", "cargo test").unwrap();
        c.save_checkpoint(&lease, &serde_json::json!({"step": 3}))
            .unwrap();
        c.finish(&lease, Outcome::Succeeded).unwrap();
    }
    let c = Continuity::open(&path).unwrap();
    let task = c.get("a").unwrap();
    assert_eq!(task.state, TaskState::Succeeded);
    assert_eq!(task.stage.as_deref(), Some("testing"));
    assert_eq!(task.checkpoint["step"], 3);
    assert_eq!(task.attempt, 1);
    let kinds: Vec<String> = c
        .events("a", 0, 50)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, ["submitted", "claimed", "testing", "succeeded"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn recovery_applies_each_resume_policy() {
    let (path, dir) = db();
    {
        let c = Continuity::open(&path).unwrap();
        for (id, policy) in [
            ("restart", ResumePolicy::Restart),
            ("abandon", ResumePolicy::Abandon),
            ("manual", ResumePolicy::Manual),
            ("cancelled", ResumePolicy::Restart),
        ] {
            c.submit(spec(id, policy)).unwrap();
            c.claim(id).unwrap();
        }
        assert_eq!(
            c.request_cancel("cancelled", "user stop").unwrap(),
            CancelOutcome::Requested
        );
        c.submit(spec("queued", ResumePolicy::Restart)).unwrap();
        // Process dies here: nothing is finished.
    }
    let c = Continuity::open(&path).unwrap();
    let report = c.recover().unwrap();
    let ids = |tasks: &[sail_continuity::Task]| -> Vec<String> {
        tasks.iter().map(|t| t.id.clone()).collect()
    };
    assert_eq!(ids(&report.resumable), ["restart", "queued"]);
    assert_eq!(ids(&report.needs_intervention), ["manual"]);
    assert_eq!(ids(&report.abandoned), ["abandon"]);
    assert_eq!(ids(&report.cancelled), ["cancelled"]);
    assert_eq!(c.get("restart").unwrap().state, TaskState::Interrupted);
    assert_eq!(c.get("abandon").unwrap().state, TaskState::Failed);
    assert_eq!(c.get("cancelled").unwrap().state, TaskState::Cancelled);

    // The interrupted task is claimed again as a new attempt.
    let lease = c.claim("restart").unwrap();
    assert_eq!(lease.attempt(), 2);
    // A second recovery in the same boot leaves the running task alone.
    let again = c.recover().unwrap();
    assert!(again.abandoned.is_empty() && again.cancelled.is_empty());
    assert_eq!(c.get("restart").unwrap().state, TaskState::Running);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn stale_lease_from_an_earlier_boot_is_rejected() {
    let (path, dir) = db();
    let old_lease = {
        let c = Continuity::open(&path).unwrap();
        c.submit(spec("t", ResumePolicy::Restart)).unwrap();
        c.claim("t").unwrap()
    };
    let c = Continuity::open(&path).unwrap();
    c.recover().unwrap();
    let new_lease = c.claim("t").unwrap();
    assert!(matches!(
        c.progress(&old_lease, "coding", "late write"),
        Err(ContinuityError::StaleLease { .. })
    ));
    assert!(matches!(
        c.finish(&old_lease, Outcome::Succeeded),
        Err(ContinuityError::StaleLease { .. })
    ));
    c.finish(&new_lease, Outcome::Failed("tests failed".into()))
        .unwrap();
    let task = c.get("t").unwrap();
    assert_eq!(task.state, TaskState::Failed);
    assert_eq!(task.reason.as_deref(), Some("tests failed"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn duplicate_submit_returns_the_active_task() {
    let c = Continuity::open_in_memory().unwrap();
    let first = c
        .submit(spec("job-1", ResumePolicy::Restart).with_dedupe_key("o/r#7"))
        .unwrap();
    assert!(first.created);
    let second = c
        .submit(spec("job-2", ResumePolicy::Restart).with_dedupe_key("o/r#7"))
        .unwrap();
    assert!(!second.created);
    assert_eq!(second.task.id, "job-1");
    assert!(matches!(c.get("job-2"), Err(ContinuityError::NotFound(_))));

    // Once finished, the key is free again.
    let lease = c.claim("job-1").unwrap();
    c.finish(&lease, Outcome::Succeeded).unwrap();
    let third = c
        .submit(spec("job-3", ResumePolicy::Restart).with_dedupe_key("o/r#7"))
        .unwrap();
    assert!(third.created);
}

#[test]
fn cancellation_paths() {
    let c = Continuity::open_in_memory().unwrap();
    c.submit(spec("queued", ResumePolicy::Restart)).unwrap();
    assert_eq!(
        c.request_cancel("queued", "no longer needed").unwrap(),
        CancelOutcome::Cancelled
    );
    assert!(matches!(
        c.claim("queued"),
        Err(ContinuityError::InvalidState { .. })
    ));

    c.submit(spec("running", ResumePolicy::Restart)).unwrap();
    let lease = c.claim("running").unwrap();
    assert_eq!(
        c.request_cancel("running", "stop").unwrap(),
        CancelOutcome::Requested
    );
    assert!(c.is_cancel_requested("running").unwrap());
    c.finish(&lease, Outcome::Cancelled("stopped by worker".into()))
        .unwrap();
    assert_eq!(
        c.request_cancel("running", "again").unwrap(),
        CancelOutcome::AlreadyFinished(TaskState::Cancelled)
    );
}

#[test]
fn committed_effect_is_not_replayed_after_restart() {
    let (path, dir) = db();
    {
        let c = Continuity::open(&path).unwrap();
        c.submit(spec("t", ResumePolicy::Restart)).unwrap();
        assert_eq!(
            c.begin_effect("t", "publish", "open draft PR").unwrap(),
            EffectGate::Proceed
        );
        c.commit_effect("t", "publish", "https://example.test/pull/1")
            .unwrap();
    }
    let c = Continuity::open(&path).unwrap();
    c.recover().unwrap();
    assert_eq!(
        c.begin_effect("t", "publish", "open draft PR").unwrap(),
        EffectGate::AlreadyDone {
            receipt: "https://example.test/pull/1".into()
        }
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn effect_interrupted_by_a_crash_needs_intervention() {
    let (path, dir) = db();
    {
        let c = Continuity::open(&path).unwrap();
        c.submit(spec("t", ResumePolicy::Restart)).unwrap();
        let lease = c.claim("t").unwrap();
        c.finish(&lease, Outcome::Succeeded).unwrap();
        assert_eq!(
            c.begin_effect("t", "publish", "push branch and open draft PR")
                .unwrap(),
            EffectGate::Proceed
        );
        // Same boot: a concurrent second attempt is refused.
        assert!(matches!(
            c.begin_effect("t", "publish", "push branch and open draft PR"),
            Err(ContinuityError::EffectInProgress { .. })
        ));
        // Crash before commit_effect.
    }
    let c = Continuity::open(&path).unwrap();
    let report = c.recover().unwrap();
    assert_eq!(report.needs_intervention.len(), 1);
    let task = c.get("t").unwrap();
    assert_eq!(task.state, TaskState::NeedsIntervention);
    assert!(
        task.reason
            .unwrap()
            .contains("push branch and open draft PR")
    );
    assert!(matches!(
        c.begin_effect("t", "publish", "push branch and open draft PR"),
        Err(ContinuityError::EffectInDoubt { .. })
    ));
    // A late commit from the dead boot cannot sneak in either.
    assert!(c.commit_effect("t", "publish", "late").is_err());

    // The user checks and approves a retry; the effect may be attempted again.
    assert_eq!(
        c.resolve("t", Resolution::Retry).unwrap().state,
        TaskState::Queued
    );
    assert_eq!(
        c.begin_effect("t", "publish", "push branch and open draft PR")
            .unwrap(),
        EffectGate::Proceed
    );
    c.fail_effect("t", "publish", "remote rejected").unwrap();
    assert_eq!(
        c.begin_effect("t", "publish", "push branch and open draft PR")
            .unwrap(),
        EffectGate::Proceed
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn submit_refuses_duplicate_of_task_needing_intervention() {
    let (path, dir) = db();
    {
        let c = Continuity::open(&path).unwrap();
        c.submit(spec("old", ResumePolicy::Manual).with_dedupe_key("k"))
            .unwrap();
        c.claim("old").unwrap();
    }
    let c = Continuity::open(&path).unwrap();
    c.recover().unwrap();
    assert!(matches!(
        c.submit(spec("new", ResumePolicy::Manual).with_dedupe_key("k")),
        Err(ContinuityError::DuplicateNeedsIntervention { .. })
    ));
    c.resolve("old", Resolution::Cancel).unwrap();
    assert!(
        c.submit(spec("new", ResumePolicy::Manual).with_dedupe_key("k"))
            .unwrap()
            .created
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn history_and_checkpoints_are_bounded() {
    let c = Continuity::open_in_memory().unwrap();
    c.submit(spec("t", ResumePolicy::Restart)).unwrap();
    let lease = c.claim("t").unwrap();
    for i in 0..(MAX_EVENTS_PER_TASK + 50) {
        c.progress(&lease, "coding", &format!("step {i} {}", "x".repeat(5_000)))
            .unwrap();
    }
    let events = c.events("t", 0, 1_000).unwrap();
    assert_eq!(events.len() as i64, MAX_EVENTS_PER_TASK);
    assert!(events.iter().all(|e| e.detail.chars().count() <= 2_001));
    assert!(
        events
            .last()
            .unwrap()
            .detail
            .starts_with(&format!("step {}", MAX_EVENTS_PER_TASK + 49))
    );

    let big = serde_json::json!({"blob": "y".repeat(MAX_CHECKPOINT_BYTES)});
    assert!(matches!(
        c.save_checkpoint(&lease, &big),
        Err(ContinuityError::CheckpointTooLarge { .. })
    ));
}

#[test]
fn list_filters_and_remove() {
    let c = Continuity::open_in_memory().unwrap();
    c.submit(spec("a", ResumePolicy::Restart)).unwrap();
    c.submit(TaskSpec::new("sail-flow", "x", "other").with_id("b"))
        .unwrap();
    let lease = c.claim("a").unwrap();
    assert!(matches!(
        c.remove("a"),
        Err(ContinuityError::InvalidState { .. })
    ));
    c.finish(&lease, Outcome::Succeeded).unwrap();

    let forge_active = c
        .list(&TaskFilter {
            owner: Some("sail-forge".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(forge_active.is_empty());
    let all = c
        .list(&TaskFilter {
            include_terminal: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(all.len(), 2);
    c.remove("a").unwrap();
    c.remove("a").unwrap();
    assert!(matches!(c.get("a"), Err(ContinuityError::NotFound(_))));
}

#[test]
fn unclaimed_task_can_be_failed_by_its_owner() {
    let c = Continuity::open_in_memory().unwrap();
    c.submit(spec("t", ResumePolicy::Restart)).unwrap();
    let task = c.fail_unclaimed("t", "checkpoint lost").unwrap();
    assert_eq!(task.state, TaskState::Failed);
    assert!(c.fail_unclaimed("t", "again").is_err());
}

#[test]
fn legacy_store_migrates_with_its_wal_and_keeps_a_backup() {
    let (old, old_dir) = db();
    let new = old_dir.join("data/state/continuity.db");
    {
        // Leave committed rows in the WAL (no checkpoint) to prove VACUUM INTO picks them up.
        let store = Continuity::open(&old).unwrap();
        store
            .submit(spec("migrated-task", ResumePolicy::Manual))
            .unwrap();
        std::mem::forget(store);
    }
    assert!(sail_continuity::migrate_legacy_store(&old, &new).unwrap());
    let reopened = Continuity::open(&new).unwrap();
    assert!(reopened.get("migrated-task").is_ok());
    assert!(!old.exists());
    assert!(old_dir.join("continuity.db.migrated-to-sail-data").exists());
    // Already migrated: nothing happens, and an existing store is never replaced.
    assert!(!sail_continuity::migrate_legacy_store(&old, &new).unwrap());
    let _ = std::fs::remove_dir_all(&old_dir);
}
