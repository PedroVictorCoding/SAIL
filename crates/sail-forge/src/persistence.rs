use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::info;
use uuid::Uuid;

use crate::contracts::{
    ForgeBudgetConfig, ForgeJobReport, ForgeTaskEvent, ForgeTaskState, ReviewPolicyConfig,
    ReviewResult, TestExecutionRecord,
};

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
        }
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

    pub fn default_store() -> Self {
        let dir = std::env::var("SAIL_FORGE_CHECKPOINT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("sail_forge_checkpoints"));
        Self::new(dir)
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
