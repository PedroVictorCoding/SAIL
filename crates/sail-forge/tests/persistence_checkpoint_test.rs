use std::fs;
use uuid::Uuid;

use sail_forge::contracts::{ForgeBudgetConfig, ForgeTaskState, ReviewPolicyConfig};
use sail_forge::persistence::{TaskCheckpoint, TaskCheckpointStore};

#[test]
fn test_durable_checkpoint_save_and_reload() {
    let temp_dir = std::env::temp_dir().join(format!("test_checkpoints_{}", Uuid::new_v4()));
    let store = TaskCheckpointStore::new(temp_dir.clone());

    let job_id = format!("job-cp-{}", Uuid::new_v4());
    let mut cp = TaskCheckpoint::new(
        job_id.clone(),
        "PedroVictorCoding/SAIL".into(),
        90,
        "83b5762".into(),
        "agent/forge-issue-90".into(),
        ForgeBudgetConfig::default(),
        ReviewPolicyConfig::default(),
    );

    cp.record_event("planning", "Analyzing repository AST and issue requirements", "key:step1");
    cp.transition(ForgeTaskState::Planning, "Moved to planning");

    store.save(&cp).unwrap();

    // Reload from store
    let loaded = store.load(&job_id).unwrap().expect("Checkpoint must exist");
    assert_eq!(loaded.job_id, job_id);
    assert_eq!(loaded.repo, "PedroVictorCoding/SAIL");
    assert_eq!(loaded.issue_number, 90);
    assert_eq!(loaded.state, ForgeTaskState::Planning);
    assert_eq!(loaded.events.len(), 2);
    assert!(loaded.has_executed("key:step1"));

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_idempotency_keys_prevent_duplicate_operations() {
    let mut cp = TaskCheckpoint::new(
        "job-idem".into(),
        "PedroVictorCoding/SAIL".into(),
        90,
        "83b5762".into(),
        "agent/forge-issue-90".into(),
        ForgeBudgetConfig::default(),
        ReviewPolicyConfig::default(),
    );

    let key = "tool_call:apply_patch:commit_1";
    assert!(!cp.has_executed(key));

    cp.record_event("coding", "Applied patch 1", key);
    assert!(cp.has_executed(key));
}

#[test]
fn test_daemon_restart_recovery_lists_uncompleted_tasks() {
    let temp_dir = std::env::temp_dir().join(format!("test_checkpoints_restart_{}", Uuid::new_v4()));
    let store = TaskCheckpointStore::new(temp_dir.clone());

    // Task 1: in progress (Coding)
    let mut cp1 = TaskCheckpoint::new(
        "job-active-1".into(),
        "PedroVictorCoding/SAIL".into(),
        90,
        "83b5762".into(),
        "agent/forge-90".into(),
        ForgeBudgetConfig::default(),
        ReviewPolicyConfig::default(),
    );
    cp1.transition(ForgeTaskState::Coding, "Writing test fix");
    store.save(&cp1).unwrap();

    // Task 2: completed
    let mut cp2 = TaskCheckpoint::new(
        "job-done-2".into(),
        "PedroVictorCoding/SAIL".into(),
        91,
        "83b5762".into(),
        "agent/forge-91".into(),
        ForgeBudgetConfig::default(),
        ReviewPolicyConfig::default(),
    );
    cp2.transition(ForgeTaskState::Completed, "All checks passed");
    store.save(&cp2).unwrap();

    // Simulate daemon restart: create new store instance pointing to same directory
    let reboot_store = TaskCheckpointStore::new(temp_dir.clone());
    let all = reboot_store.list_all().unwrap();

    assert_eq!(all.len(), 2);
    let active = all.iter().filter(|c| c.state != ForgeTaskState::Completed).collect::<Vec<_>>();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].job_id, "job-active-1");
    assert_eq!(active[0].state, ForgeTaskState::Coding);

    let _ = fs::remove_dir_all(&temp_dir);
}

