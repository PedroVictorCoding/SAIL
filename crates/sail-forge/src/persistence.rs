use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::info;
use uuid::Uuid;

use crate::contracts::{
    ForgeActivityEntry, ForgeBudgetConfig, ForgeJobReport, ForgeTaskEvent, ForgeTaskState,
    ReviewPolicyConfig, ReviewResult, TestExecutionRecord,
};

/// Bounds for the persisted activity log (keeps checkpoints and UI payloads small).
pub const MAX_ACTIVITY_ENTRIES: usize = 400;
pub const MAX_ACTIVITY_BODY_CHARS: usize = 16_000;
pub const MAX_ACTIVITY_TOTAL_CHARS: usize = 1_000_000;

/// Durable checkpoint for a `forge:issue_solve` task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCheckpoint {
    pub job_id: String,
    pub repo: String,
    pub issue_number: u64,
    pub base_commit: String,
    pub branch: String,
    pub state: ForgeTaskState,
    pub iteration: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub budget: ForgeBudgetConfig,
    pub review_policy: ReviewPolicyConfig,
    pub tokens_used: u64,
    pub wall_time_secs: u64,
    pub patch: Option<String>,
    pub diff_stat: Option<String>,
    pub events: Vec<ForgeTaskEvent>,
    pub test_history: Vec<TestExecutionRecord>,
    pub review_result: Option<ReviewResult>,
    pub completed_idempotency_keys: Vec<String>,
    pub final_report: Option<ForgeJobReport>,
    #[serde(default)]
    pub issue_title: Option<String>,
    #[serde(default)]
    pub issue_body: Option<String>,
    #[serde(default)]
    pub target_file: Option<String>,
    #[serde(default)]
    pub verification_status: Option<String>,
    #[serde(default)]
    pub failure_reason: Option<String>,
    #[serde(default)]
    pub model_endpoint: Option<String>,
    #[serde(default)]
    pub modified_files: Option<Vec<String>>,
    #[serde(default)]
    pub live_output: Option<String>,
    #[serde(default)]
    pub model_action: Option<String>,
    /// Complete, bounded record of model replies, tool results and pipeline stages.
    #[serde(default)]
    pub activity: Vec<ForgeActivityEntry>,
}

impl TaskCheckpoint {
    pub fn new(
        job_id: String,
        repo: String,
        issue_number: u64,
        base_commit: String,
        branch: String,
        budget: ForgeBudgetConfig,
        review_policy: ReviewPolicyConfig,
    ) -> Self {
        let now = Utc::now();
        Self {
            job_id,
            repo,
            issue_number,
            base_commit,
            branch,
            state: ForgeTaskState::Queued,
            iteration: 0,
            created_at: now,
            updated_at: now,
            budget,
            review_policy,
            tokens_used: 0,
            wall_time_secs: 0,
            patch: None,
            diff_stat: None,
            events: Vec::new(),
            test_history: Vec::new(),
            review_result: None,
            completed_idempotency_keys: Vec::new(),
            final_report: None,
            issue_title: None,
            issue_body: None,
            target_file: None,
            verification_status: None,
            failure_reason: None,
            model_endpoint: None,
            modified_files: None,
            live_output: None,
            model_action: None,
            activity: Vec::new(),
        }
    }

    /// Append to the activity log. Long bodies are truncated, and once the log exceeds its
    /// bounds the oldest entries after the first (the issue context) are dropped.
    pub fn push_activity(&mut self, mut entry: ForgeActivityEntry) {
        let len = entry.body.chars().count();
        if len > MAX_ACTIVITY_BODY_CHARS {
            let kept: String = entry.body.chars().take(MAX_ACTIVITY_BODY_CHARS).collect();
            entry.body = format!(
                "{kept}\n… [truncated {} of {len} chars]",
                len - MAX_ACTIVITY_BODY_CHARS
            );
        }
        self.activity.push(entry);
        let total = |a: &[ForgeActivityEntry]| a.iter().map(|e| e.body.len()).sum::<usize>();
        while self.activity.len() > 2
            && (self.activity.len() > MAX_ACTIVITY_ENTRIES
                || total(&self.activity) > MAX_ACTIVITY_TOTAL_CHARS)
        {
            self.activity.remove(1);
        }
        self.updated_at = Utc::now();
    }

    /// Add event to task timeline.
    pub fn record_event(&mut self, stage: &str, detail: &str, idempotency_key: &str) {
        self.events.push(ForgeTaskEvent {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            stage: stage.to_string(),
            detail: detail.to_string(),
            idempotency_key: idempotency_key.to_string(),
        });
        self.completed_idempotency_keys
            .push(idempotency_key.to_string());
        self.updated_at = Utc::now();
    }

    /// Check if an operation has already executed (idempotency).
    pub fn has_executed(&self, idempotency_key: &str) -> bool {
        self.completed_idempotency_keys
            .iter()
            .any(|k| k == idempotency_key)
    }

    /// Transition task state.
    pub fn transition(&mut self, new_state: ForgeTaskState, detail: &str) {
        let key = format!("transition:{:?}:{}", new_state, self.iteration);
        self.state = new_state;
        self.record_event(
            &format!("state_transition -> {:?}", new_state),
            detail,
            &key,
        );
    }
}

/// Durable file-based checkpoint store.
#[derive(Debug, Clone)]
pub struct TaskCheckpointStore {
    base_dir: PathBuf,
}

impl TaskCheckpointStore {
    pub fn new(base_dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&base_dir);
        Self { base_dir }
    }

    /// `SAIL_FORGE_CHECKPOINT_DIR`, else `forge-checkpoints` in Continuity's durable state
    /// directory. Checkpoints left in the old temp-dir default are copied there once.
    pub fn default_store() -> Self {
        if let Ok(dir) = std::env::var("SAIL_FORGE_CHECKPOINT_DIR") {
            return Self::new(PathBuf::from(dir));
        }
        let dir = sail_continuity::state_dir().join("forge-checkpoints");
        let store = Self::new(dir);
        store.import_legacy(&std::env::temp_dir().join("sail_forge_checkpoints"));
        store
    }

    /// Copy checkpoints this store does not have yet from `legacy`. The legacy files are
    /// left in place.
    pub fn import_legacy(&self, legacy: &std::path::Path) -> usize {
        let Ok(entries) = fs::read_dir(legacy) else {
            return 0;
        };
        let mut imported = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            let (Some(name), Some("json")) =
                (path.file_name(), path.extension().and_then(|e| e.to_str()))
            else {
                continue;
            };
            let target = self.base_dir.join(name);
            if target.exists() {
                continue;
            }
            let valid = fs::read_to_string(&path)
                .ok()
                .and_then(|c| serde_json::from_str::<TaskCheckpoint>(&c).ok());
            if let Some(cp) = valid
                && self.save(&cp).is_ok()
            {
                imported += 1;
            }
        }
        if imported > 0 {
            info!(
                "Imported {imported} Forge checkpoints from {} into {}",
                legacy.display(),
                self.base_dir.display()
            );
        }
        imported
    }

    fn checkpoint_path(&self, job_id: &str) -> PathBuf {
        self.base_dir.join(format!("{}.json", job_id))
    }

    /// Atomically persist checkpoint to disk using temp file rename.
    pub fn save(&self, checkpoint: &TaskCheckpoint) -> Result<(), std::io::Error> {
        let target = self.checkpoint_path(&checkpoint.job_id);
        let tmp = self
            .base_dir
            .join(format!("{}.tmp.{}", checkpoint.job_id, Uuid::new_v4()));

        let json = serde_json::to_string_pretty(checkpoint)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        fs::write(&tmp, json)?;
        fs::rename(&tmp, &target)?;
        info!(
            "Saved durable checkpoint for job {} (state: {:?})",
            checkpoint.job_id, checkpoint.state
        );
        Ok(())
    }

    /// Load checkpoint by job_id.
    pub fn load(&self, job_id: &str) -> Result<Option<TaskCheckpoint>, std::io::Error> {
        let path = self.checkpoint_path(job_id);
        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path)?;
        let checkpoint: TaskCheckpoint = serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        Ok(Some(checkpoint))
    }

    /// List all checkpoints currently stored on disk (for daemon reboot recovery).
    pub fn list_all(&self) -> Result<Vec<TaskCheckpoint>, std::io::Error> {
        let mut results = Vec::new();
        if !self.base_dir.exists() {
            return Ok(results);
        }

        for entry in fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(cp) = serde_json::from_str::<TaskCheckpoint>(&content) {
                        results.push(cp);
                    }
                }
            }
        }

        Ok(results)
    }

    /// Delete checkpoint on completion or cleanup.
    pub fn delete(&self, job_id: &str) -> Result<(), std::io::Error> {
        let path = self.checkpoint_path(job_id);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint() -> TaskCheckpoint {
        TaskCheckpoint::new(
            "job-1".into(),
            "o/r".into(),
            1,
            "main".into(),
            "agent/forge-issue-1".into(),
            ForgeBudgetConfig::default(),
            ReviewPolicyConfig::default(),
        )
    }

    #[test]
    fn long_bodies_are_truncated_with_a_marker() {
        let mut cp = checkpoint();
        cp.push_activity(ForgeActivityEntry::new(
            1,
            "model",
            "big",
            "x".repeat(MAX_ACTIVITY_BODY_CHARS + 10),
        ));
        assert!(cp.activity[0].body.ends_with(&format!(
            "[truncated 10 of {} chars]",
            MAX_ACTIVITY_BODY_CHARS + 10
        )));
    }

    #[test]
    fn oldest_entries_are_dropped_but_context_is_kept() {
        let mut cp = checkpoint();
        cp.push_activity(ForgeActivityEntry::new(0, "context", "ctx", "issue"));
        for i in 0..MAX_ACTIVITY_ENTRIES + 5 {
            cp.push_activity(ForgeActivityEntry::new(1, "feedback", format!("e{i}"), ""));
        }
        assert_eq!(cp.activity.len(), MAX_ACTIVITY_ENTRIES);
        assert_eq!(cp.activity[0].kind, "context");
        assert_eq!(
            cp.activity.last().unwrap().title,
            format!("e{}", MAX_ACTIVITY_ENTRIES + 4)
        );
    }

    #[test]
    fn total_size_is_bounded() {
        let mut cp = checkpoint();
        for _ in 0..200 {
            cp.push_activity(ForgeActivityEntry::new(
                1,
                "model",
                "m",
                "y".repeat(MAX_ACTIVITY_BODY_CHARS),
            ));
        }
        let total: usize = cp.activity.iter().map(|e| e.body.len()).sum();
        assert!(total <= MAX_ACTIVITY_TOTAL_CHARS, "{total}");
    }

    #[test]
    fn legacy_checkpoints_are_imported_once_without_overwriting() {
        let root = std::env::temp_dir().join(format!("forge_cp_import_{}", Uuid::new_v4()));
        let legacy = TaskCheckpointStore::new(root.join("legacy"));
        let current = TaskCheckpointStore::new(root.join("current"));
        let mut a = checkpoint();
        a.job_id = "job-a".into();
        legacy.save(&a).unwrap();
        let mut b = checkpoint();
        b.job_id = "job-b".into();
        b.iteration = 1;
        legacy.save(&b).unwrap();
        b.iteration = 7;
        current.save(&b).unwrap();
        fs::write(root.join("legacy/broken.json"), "{").unwrap();

        assert_eq!(current.import_legacy(&root.join("legacy")), 1);
        assert!(current.load("job-a").unwrap().is_some());
        assert_eq!(current.load("job-b").unwrap().unwrap().iteration, 7);
        assert_eq!(current.import_legacy(&root.join("legacy")), 0);
        assert!(legacy.load("job-a").unwrap().is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn old_checkpoints_without_activity_still_load() {
        let mut v = serde_json::to_value(checkpoint()).unwrap();
        v.as_object_mut().unwrap().remove("activity");
        let cp: TaskCheckpoint = serde_json::from_value(v).unwrap();
        assert!(cp.activity.is_empty());
    }
}
