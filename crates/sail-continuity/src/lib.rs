//! SAIL Continuity: the durable, server-owned record of accepted background work.
//!
//! Owner subsystems submit tasks, claim a lease to run them and report progress and
//! outcomes here. Each daemon start opens a new *boot*; [`Continuity::recover`] decides what
//! happens to work an earlier boot left running, and the effect journal
//! ([`Continuity::begin_effect`]) stops an external effect from being replayed after a crash.
//! See `docs/SAIL_CONTINUITY.md` for the contract.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

/// Events kept per task; older ones are dropped.
pub const MAX_EVENTS_PER_TASK: i64 = 200;
/// Longest stored event detail, in characters.
pub const MAX_EVENT_DETAIL_CHARS: usize = 2_000;
/// Largest stored checkpoint, in bytes of JSON.
pub const MAX_CHECKPOINT_BYTES: usize = 64 * 1024;
/// Terminal tasks older than this are pruned when the store opens.
pub const FINISHED_RETENTION_DAYS: i64 = 30;

const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ContinuityError {
    #[error("task {0} not found")]
    NotFound(String),
    #[error("task {task}: lease attempt {attempt} is stale")]
    StaleLease { task: String, attempt: u32 },
    #[error("task {task} is {state}; cannot {action}")]
    InvalidState {
        task: String,
        state: TaskState,
        action: &'static str,
    },
    #[error("task {0} was cancelled")]
    Cancelled(String),
    #[error(
        "effect '{key}' of task {task} was interrupted and its outcome is unknown; resolve the task first"
    )]
    EffectInDoubt { task: String, key: String },
    #[error("effect '{key}' of task {task} is already in progress")]
    EffectInProgress { task: String, key: String },
    #[error("an active task already uses dedupe key '{key}' and needs intervention: {task}")]
    DuplicateNeedsIntervention { task: String, key: String },
    #[error("checkpoint is {size} bytes; the limit is {MAX_CHECKPOINT_BYTES}")]
    CheckpointTooLarge { size: usize },
    #[error("continuity storage error: {0}")]
    Storage(String),
}

impl From<rusqlite::Error> for ContinuityError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<serde_json::Error> for ContinuityError {
    fn from(error: serde_json::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ContinuityError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    Interrupted,
    NeedsIntervention,
    Succeeded,
    Failed,
    Cancelled,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Interrupted => "interrupted",
            Self::NeedsIntervention => "needs_intervention",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "interrupted" => Self::Interrupted,
            "needs_intervention" => Self::NeedsIntervention,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            other => return Err(ContinuityError::Storage(format!("unknown state {other}"))),
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What recovery does with a task an earlier boot left running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumePolicy {
    /// Local, idempotent work: re-run it.
    Restart,
    /// The caller retries on its own: fail it with an explicit reason.
    Abandon,
    /// Never resume automatically: ask the user.
    Manual,
}

impl ResumePolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Restart => "restart",
            Self::Abandon => "abandon",
            Self::Manual => "manual",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "restart" => Self::Restart,
            "abandon" => Self::Abandon,
            "manual" => Self::Manual,
            other => return Err(ContinuityError::Storage(format!("unknown policy {other}"))),
        })
    }
}

/// A request to create a task.
#[derive(Debug, Clone)]
pub struct TaskSpec {
    /// Caller-chosen id (e.g. a Forge job id); a UUID is generated when absent.
    pub id: Option<String>,
    /// Owning subsystem, e.g. `sail-forge`.
    pub owner: String,
    /// Owner-defined kind, e.g. `forge.issue_solve`.
    pub kind: String,
    pub title: String,
    /// At most one active (non-terminal) task per `(owner, dedupe_key)`.
    pub dedupe_key: Option<String>,
    pub resume: ResumePolicy,
    /// Small owner-defined state needed to resume.
    pub checkpoint: serde_json::Value,
}

impl TaskSpec {
    pub fn new(
        owner: impl Into<String>,
        kind: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        Self {
            id: None,
            owner: owner.into(),
            kind: kind.into(),
            title: title.into(),
            dedupe_key: None,
            resume: ResumePolicy::Manual,
            checkpoint: serde_json::Value::Null,
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn with_dedupe_key(mut self, key: impl Into<String>) -> Self {
        self.dedupe_key = Some(key.into());
        self
    }

    pub fn with_resume(mut self, resume: ResumePolicy) -> Self {
        self.resume = resume;
        self
    }

    pub fn with_checkpoint(mut self, checkpoint: serde_json::Value) -> Self {
        self.checkpoint = checkpoint;
        self
    }
}

/// The authoritative record of a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub owner: String,
    pub kind: String,
    pub title: String,
    pub state: TaskState,
    /// Owner-reported current step, e.g. `testing`.
    pub stage: Option<String>,
    /// Why the task is in its state (failure, cancellation, required intervention).
    pub reason: Option<String>,
    pub resume: ResumePolicy,
    pub dedupe_key: Option<String>,
    /// Number of times a worker has claimed the task.
    pub attempt: u32,
    pub cancel_requested: bool,
    pub checkpoint: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Proof that the caller is the current worker of a task. Writes with an older lease fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    task_id: String,
    attempt: u32,
    boot_id: String,
}

impl Lease {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskEvent {
    pub seq: i64,
    pub at: DateTime<Utc>,
    pub kind: String,
    pub detail: String,
}

/// Result of [`Continuity::submit`].
#[derive(Debug, Clone, PartialEq)]
pub struct Submitted {
    pub task: Task,
    /// False when an active task with the same dedupe key was returned instead.
    pub created: bool,
}

/// How a worker ended a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Failed(String),
    Cancelled(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The task is now cancelled.
    Cancelled,
    /// The task is running; its worker must stop. Recovery cancels it if the daemon dies first.
    Requested,
    /// The task had already finished.
    AlreadyFinished(TaskState),
}

/// The user's decision for a task that needs intervention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Re-queue it. Effects in doubt are released: the user has checked their outcome.
    Retry,
    Cancel,
}

/// Answer of [`Continuity::begin_effect`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectGate {
    /// Intent recorded; perform the effect, then commit or fail it.
    Proceed,
    /// The effect already happened; this is its receipt. Do not repeat it.
    AlreadyDone { receipt: String },
}

/// What [`Continuity::recover`] did with work earlier boots left behind.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecoveryReport {
    /// Queued or interrupted tasks the owner should run again.
    pub resumable: Vec<Task>,
    pub needs_intervention: Vec<Task>,
    /// Running tasks with the `abandon` policy, now failed.
    pub abandoned: Vec<Task>,
    /// Running tasks whose cancellation was requested, now cancelled.
    pub cancelled: Vec<Task>,
}

impl RecoveryReport {
    pub fn is_empty(&self) -> bool {
        self.resumable.is_empty()
            && self.needs_intervention.is_empty()
            && self.abandoned.is_empty()
            && self.cancelled.is_empty()
    }
}

/// Filter for [`Continuity::list`].
#[derive(Debug, Clone, Default)]
pub struct TaskFilter {
    pub owner: Option<String>,
    pub include_terminal: bool,
    pub limit: Option<usize>,
}

/// Durable task store. Cheap to share behind an `Arc`.
pub struct Continuity {
    conn: Mutex<Connection>,
    boot_id: String,
    path: Option<PathBuf>,
}

impl std::fmt::Debug for Continuity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Continuity")
            .field("boot_id", &self.boot_id)
            .field("path", &self.path)
            .finish()
    }
}

/// Default store location; see `docs/SAIL_CONTINUITY.md`.
pub fn default_path() -> PathBuf {
    if let Ok(path) = std::env::var("SAIL_CONTINUITY_DB") {
        return PathBuf::from(path);
    }
    state_dir().join("continuity.db")
}

/// Directory for durable runtime task state: the runtime SSD when writable, otherwise the
/// user's data directory. Never the system temp directory, which may be tmpfs.
pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SAIL_STATE_DIR") {
        return PathBuf::from(dir);
    }
    let runtime = std::env::var("SAIL_RUNTIME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/srv/sail"));
    let runtime_state = runtime.join("state");
    if std::fs::create_dir_all(&runtime_state).is_ok() && writable_dir(&runtime_state) {
        return runtime_state;
    }
    std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("sail")
}

fn writable_dir(dir: &Path) -> bool {
    let probe = dir.join(format!(".sail-write-probe-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…")
}

fn invalid(task: &Task, action: &'static str) -> ContinuityError {
    ContinuityError::InvalidState {
        task: task.id.clone(),
        state: task.state,
        action,
    }
}

const TASK_COLUMNS: &str = "id, owner, kind, title, state, stage, reason, resume, dedupe_key, \
     attempt, cancel_requested, checkpoint, created_at, updated_at";

fn task_from_row(row: &Row<'_>) -> rusqlite::Result<(Task, String, String)> {
    let state: String = row.get(4)?;
    let resume: String = row.get(7)?;
    let checkpoint: String = row.get(11)?;
    Ok((
        Task {
            id: row.get(0)?,
            owner: row.get(1)?,
            kind: row.get(2)?,
            title: row.get(3)?,
            state: TaskState::Queued,
            stage: row.get(5)?,
            reason: row.get(6)?,
            resume: ResumePolicy::Manual,
            dedupe_key: row.get(8)?,
            attempt: row.get(9)?,
            cancel_requested: row.get::<_, i64>(10)? != 0,
            checkpoint: serde_json::from_str(&checkpoint).unwrap_or(serde_json::Value::Null),
            created_at: row.get(12)?,
            updated_at: row.get(13)?,
        },
        state,
        resume,
    ))
}

fn finish_row(raw: (Task, String, String)) -> Result<Task> {
    let (mut task, state, resume) = raw;
    task.state = TaskState::parse(&state)?;
    task.resume = ResumePolicy::parse(&resume)?;
    Ok(task)
}

fn load(conn: &Connection, id: &str) -> Result<Task> {
    let raw = conn
        .query_row(
            &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"),
            params![id],
            task_from_row,
        )
        .optional()?
        .ok_or_else(|| ContinuityError::NotFound(id.to_string()))?;
    finish_row(raw)
}

fn push_event(conn: &Connection, task_id: &str, kind: &str, detail: &str) -> Result<()> {
    let seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM task_events WHERE task_id = ?1",
        params![task_id],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO task_events (task_id, seq, at, kind, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            task_id,
            seq,
            now(),
            kind,
            truncate(detail, MAX_EVENT_DETAIL_CHARS)
        ],
    )?;
    conn.execute(
        "DELETE FROM task_events WHERE task_id = ?1 AND seq <= ?2",
        params![task_id, seq - MAX_EVENTS_PER_TASK],
    )?;
    Ok(())
}

fn set_state(
    conn: &Connection,
    id: &str,
    state: TaskState,
    reason: Option<&str>,
    event: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE tasks SET state = ?2, reason = ?3, updated_at = ?4 WHERE id = ?1",
        params![id, state.as_str(), reason, now()],
    )?;
    push_event(conn, id, event, reason.unwrap_or(state.as_str()))
}

impl Continuity {
    /// Open (or create) the store at `path` and start a new boot.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| ContinuityError::Storage(format!("{}: {e}", parent.display())))?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        Self::init(conn, Some(path.to_path_buf()))
    }

    /// A non-durable store for tests and for running without a writable disk.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    /// Open the store at [`default_path`]. Falls back to memory, logging an error, so the
    /// daemon still starts; [`Continuity::is_durable`] then reports `false`.
    pub fn open_default() -> Self {
        let path = default_path();
        match Self::open(&path) {
            Ok(store) => {
                info!(path = %path.display(), "SAIL Continuity store opened");
                store
            }
            Err(error) => {
                tracing::error!(path = %path.display(), %error, "SAIL Continuity store unavailable; background tasks will NOT survive a restart");
                Self::open_in_memory().expect("in-memory SQLite opens")
            }
        }
    }

    fn init(conn: Connection, path: Option<PathBuf>) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS tasks (
                 id TEXT PRIMARY KEY,
                 owner TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 title TEXT NOT NULL,
                 state TEXT NOT NULL,
                 stage TEXT,
                 reason TEXT,
                 resume TEXT NOT NULL,
                 dedupe_key TEXT,
                 attempt INTEGER NOT NULL DEFAULT 0,
                 boot_id TEXT,
                 cancel_requested INTEGER NOT NULL DEFAULT 0,
                 checkpoint TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS tasks_active_dedupe ON tasks(owner, dedupe_key)
                 WHERE dedupe_key IS NOT NULL
                   AND state NOT IN ('succeeded', 'failed', 'cancelled');
             CREATE INDEX IF NOT EXISTS tasks_state ON tasks(state);
             CREATE TABLE IF NOT EXISTS task_events (
                 task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                 seq INTEGER NOT NULL,
                 at TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 detail TEXT NOT NULL,
                 PRIMARY KEY (task_id, seq)
             );
             CREATE TABLE IF NOT EXISTS task_effects (
                 task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                 key TEXT NOT NULL,
                 description TEXT NOT NULL,
                 state TEXT NOT NULL,
                 boot_id TEXT NOT NULL,
                 receipt TEXT,
                 started_at TEXT NOT NULL,
                 finished_at TEXT,
                 PRIMARY KEY (task_id, key)
             );",
        )?;
        let version: Option<i64> = conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match version {
            None => {
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                    params![SCHEMA_VERSION.to_string()],
                )?;
            }
            Some(v) if v > SCHEMA_VERSION => {
                return Err(ContinuityError::Storage(format!(
                    "store schema {v} is newer than this build ({SCHEMA_VERSION})"
                )));
            }
            Some(_) => {}
        }
        let cutoff = now() - Duration::days(FINISHED_RETENTION_DAYS);
        let pruned = conn.execute(
            "DELETE FROM tasks WHERE state IN ('succeeded', 'failed', 'cancelled') AND updated_at < ?1",
            params![cutoff],
        )?;
        if pruned > 0 {
            info!(pruned, "Pruned finished Continuity tasks past retention");
        }
        Ok(Self {
            conn: Mutex::new(conn),
            boot_id: Uuid::new_v4().to_string(),
            path,
        })
    }

    pub fn is_durable(&self) -> bool {
        self.path.is_some()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn boot_id(&self) -> &str {
        &self.boot_id
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave a half-applied transaction behind
        // (SQLite rolls it back), so the connection stays usable.
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Classify tasks earlier boots left behind. Call once at startup, before submitting or
    /// claiming work in this boot.
    pub fn recover(&self) -> Result<RecoveryReport> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut report = RecoveryReport::default();
        let mut attention = std::collections::BTreeSet::new();

        // Effects begun by an earlier boot without a receipt: their outcome is unknown.
        let in_doubt: Vec<(String, String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT task_id, key, description FROM task_effects
                 WHERE state = 'intended' AND boot_id != ?1",
            )?;
            stmt.query_map(params![self.boot_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for (task_id, key, description) in &in_doubt {
            tx.execute(
                "UPDATE task_effects SET state = 'in_doubt' WHERE task_id = ?1 AND key = ?2",
                params![task_id, key],
            )?;
            let task = load(&tx, task_id)?;
            if task.state == TaskState::Cancelled {
                continue;
            }
            warn!(task = %task_id, effect = %key, "Continuity effect in doubt after restart");
            let reason = format!(
                "'{description}' was interrupted and may or may not have happened; check it before retrying"
            );
            // Reopening a finished task must not collide with a newer active task that
            // shares its dedupe key; in that case only the reason records the doubt.
            let conflict = match (&task.dedupe_key, task.state.is_terminal()) {
                (Some(key), true) => tx
                    .query_row(
                        "SELECT 1 FROM tasks WHERE owner = ?1 AND dedupe_key = ?2 AND id != ?3
                         AND state NOT IN ('succeeded', 'failed', 'cancelled')",
                        params![task.owner, key, task.id],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some(),
                _ => false,
            };
            if conflict {
                tx.execute(
                    "UPDATE tasks SET reason = ?2, updated_at = ?3 WHERE id = ?1",
                    params![task_id, reason, now()],
                )?;
                push_event(&tx, task_id, "effect_in_doubt", &reason)?;
            } else {
                set_state(
                    &tx,
                    task_id,
                    TaskState::NeedsIntervention,
                    Some(&reason),
                    "effect_in_doubt",
                )?;
            }
            attention.insert(task_id.clone());
        }

        let orphaned: Vec<Task> = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {TASK_COLUMNS} FROM tasks WHERE state = 'running' AND boot_id IS NOT ?1"
            ))?;
            let rows = stmt
                .query_map(params![self.boot_id], task_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().map(finish_row).collect::<Result<_>>()?
        };
        for task in orphaned {
            let (state, reason, event) = if task.cancel_requested {
                (
                    TaskState::Cancelled,
                    "cancelled before SAIL restarted",
                    "cancelled_on_recovery",
                )
            } else {
                match task.resume {
                    ResumePolicy::Restart => (
                        TaskState::Interrupted,
                        "interrupted when SAIL restarted; will run again",
                        "interrupted",
                    ),
                    ResumePolicy::Abandon => (
                        TaskState::Failed,
                        "interrupted when SAIL restarted",
                        "abandoned",
                    ),
                    ResumePolicy::Manual => (
                        TaskState::NeedsIntervention,
                        "interrupted when SAIL restarted; resuming needs your approval",
                        "needs_intervention",
                    ),
                }
            };
            set_state(&tx, &task.id, state, Some(reason), event)?;
            match state {
                TaskState::Cancelled => report.cancelled.push(load(&tx, &task.id)?),
                TaskState::Failed => report.abandoned.push(load(&tx, &task.id)?),
                _ => {}
            }
        }

        let pending: Vec<Task> = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {TASK_COLUMNS} FROM tasks
                 WHERE state IN ('queued', 'interrupted', 'needs_intervention')
                 ORDER BY created_at"
            ))?;
            let rows = stmt
                .query_map([], task_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().map(finish_row).collect::<Result<_>>()?
        };
        for task in pending {
            attention.remove(&task.id);
            match task.state {
                TaskState::NeedsIntervention => report.needs_intervention.push(task),
                _ => report.resumable.push(task),
            }
        }
        // Finished tasks whose effect is in doubt but which could not be reopened.
        for id in attention {
            report.needs_intervention.push(load(&tx, &id)?);
        }
        tx.commit()?;
        if !report.is_empty() {
            info!(
                resumable = report.resumable.len(),
                needs_intervention = report.needs_intervention.len(),
                abandoned = report.abandoned.len(),
                cancelled = report.cancelled.len(),
                "Continuity recovery complete"
            );
        }
        Ok(report)
    }

    /// Create a task, or return the active task with the same `(owner, dedupe_key)`.
    pub fn submit(&self, spec: TaskSpec) -> Result<Submitted> {
        let checkpoint = serde_json::to_string(&spec.checkpoint)?;
        if checkpoint.len() > MAX_CHECKPOINT_BYTES {
            return Err(ContinuityError::CheckpointTooLarge {
                size: checkpoint.len(),
            });
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if let Some(key) = &spec.dedupe_key {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM tasks WHERE owner = ?1 AND dedupe_key = ?2
                     AND state NOT IN ('succeeded', 'failed', 'cancelled')",
                    params![spec.owner, key],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(id) = existing {
                let task = load(&tx, &id)?;
                if task.state == TaskState::NeedsIntervention {
                    return Err(ContinuityError::DuplicateNeedsIntervention {
                        task: id,
                        key: key.clone(),
                    });
                }
                push_event(&tx, &id, "duplicate_submit", "same work requested again")?;
                tx.commit()?;
                return Ok(Submitted {
                    task,
                    created: false,
                });
            }
        }
        let id = spec.id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let at = now();
        tx.execute(
            "INSERT INTO tasks (id, owner, kind, title, state, resume, dedupe_key, checkpoint,
                                created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7, ?8, ?8)",
            params![
                id,
                spec.owner,
                spec.kind,
                spec.title,
                spec.resume.as_str(),
                spec.dedupe_key,
                checkpoint,
                at
            ],
        )?;
        push_event(&tx, &id, "submitted", &spec.title)?;
        let task = load(&tx, &id)?;
        tx.commit()?;
        Ok(Submitted {
            task,
            created: true,
        })
    }

    /// Take the task for this boot: `queued`/`interrupted` → `running`.
    pub fn claim(&self, id: &str) -> Result<Lease> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let task = load(&tx, id)?;
        if task.cancel_requested && !task.state.is_terminal() {
            set_state(
                &tx,
                id,
                TaskState::Cancelled,
                Some("cancelled before it started"),
                "cancelled",
            )?;
            tx.commit()?;
            return Err(ContinuityError::Cancelled(id.to_string()));
        }
        if !matches!(task.state, TaskState::Queued | TaskState::Interrupted) {
            return Err(invalid(&task, "claim"));
        }
        let attempt = task.attempt + 1;
        tx.execute(
            "UPDATE tasks SET state = 'running', attempt = ?2, boot_id = ?3, reason = NULL,
                              updated_at = ?4
             WHERE id = ?1",
            params![id, attempt, self.boot_id, now()],
        )?;
        push_event(&tx, id, "claimed", &format!("attempt {attempt}"))?;
        tx.commit()?;
        Ok(Lease {
            task_id: id.to_string(),
            attempt,
            boot_id: self.boot_id.clone(),
        })
    }

    fn check_lease(&self, conn: &Connection, lease: &Lease) -> Result<Task> {
        let task = load(conn, &lease.task_id)?;
        let boot: Option<String> = conn.query_row(
            "SELECT boot_id FROM tasks WHERE id = ?1",
            params![lease.task_id],
            |r| r.get(0),
        )?;
        if task.state != TaskState::Running
            || task.attempt != lease.attempt
            || boot.as_deref() != Some(lease.boot_id.as_str())
            || lease.boot_id != self.boot_id
        {
            return Err(ContinuityError::StaleLease {
                task: lease.task_id.clone(),
                attempt: lease.attempt,
            });
        }
        Ok(task)
    }

    /// Record the current step. Fails for a stale lease.
    pub fn progress(&self, lease: &Lease, stage: &str, detail: &str) -> Result<()> {
        let conn = self.conn();
        let task = self.check_lease(&conn, lease)?;
        if task.stage.as_deref() != Some(stage) {
            conn.execute(
                "UPDATE tasks SET stage = ?2, updated_at = ?3 WHERE id = ?1",
                params![lease.task_id, stage, now()],
            )?;
        }
        push_event(&conn, &lease.task_id, stage, detail)
    }

    /// Replace the resume checkpoint. Fails for a stale lease or an oversized checkpoint.
    pub fn save_checkpoint(&self, lease: &Lease, checkpoint: &serde_json::Value) -> Result<()> {
        let json = serde_json::to_string(checkpoint)?;
        if json.len() > MAX_CHECKPOINT_BYTES {
            return Err(ContinuityError::CheckpointTooLarge { size: json.len() });
        }
        let conn = self.conn();
        self.check_lease(&conn, lease)?;
        conn.execute(
            "UPDATE tasks SET checkpoint = ?2, updated_at = ?3 WHERE id = ?1",
            params![lease.task_id, json, now()],
        )?;
        Ok(())
    }

    /// Whether the user asked to stop this task.
    pub fn is_cancel_requested(&self, id: &str) -> Result<bool> {
        Ok(load(&self.conn(), id)?.cancel_requested)
    }

    /// End the task. Fails for a stale lease.
    pub fn finish(&self, lease: &Lease, outcome: Outcome) -> Result<Task> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        self.check_lease(&tx, lease)?;
        let (state, reason) = match &outcome {
            Outcome::Succeeded => (TaskState::Succeeded, None),
            Outcome::Failed(r) => (TaskState::Failed, Some(r.as_str())),
            Outcome::Cancelled(r) => (TaskState::Cancelled, Some(r.as_str())),
        };
        set_state(&tx, &lease.task_id, state, reason, state.as_str())?;
        let task = load(&tx, &lease.task_id)?;
        tx.commit()?;
        Ok(task)
    }

    /// Fail a task that no worker holds (e.g. its owner lost the data needed to resume it).
    pub fn fail_unclaimed(&self, id: &str, reason: &str) -> Result<Task> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let task = load(&tx, id)?;
        if !matches!(
            task.state,
            TaskState::Queued | TaskState::Interrupted | TaskState::NeedsIntervention
        ) {
            return Err(invalid(&task, "fail an unclaimed task"));
        }
        set_state(&tx, id, TaskState::Failed, Some(reason), "failed")?;
        let task = load(&tx, id)?;
        tx.commit()?;
        Ok(task)
    }

    /// Ask a task to stop. Durable: survives a restart.
    pub fn request_cancel(&self, id: &str, reason: &str) -> Result<CancelOutcome> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let task = load(&tx, id)?;
        let outcome = match task.state {
            s if s.is_terminal() => CancelOutcome::AlreadyFinished(s),
            TaskState::Running => {
                tx.execute(
                    "UPDATE tasks SET cancel_requested = 1, updated_at = ?2 WHERE id = ?1",
                    params![id, now()],
                )?;
                push_event(&tx, id, "cancel_requested", reason)?;
                CancelOutcome::Requested
            }
            _ => {
                tx.execute(
                    "UPDATE tasks SET cancel_requested = 1 WHERE id = ?1",
                    params![id],
                )?;
                set_state(&tx, id, TaskState::Cancelled, Some(reason), "cancelled")?;
                CancelOutcome::Cancelled
            }
        };
        tx.commit()?;
        Ok(outcome)
    }

    /// Apply the user's decision to a task that needs intervention.
    pub fn resolve(&self, id: &str, resolution: Resolution) -> Result<Task> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let task = load(&tx, id)?;
        if task.state != TaskState::NeedsIntervention {
            return Err(invalid(&task, "resolve"));
        }
        match resolution {
            Resolution::Retry => {
                tx.execute(
                    "UPDATE task_effects SET state = 'released', finished_at = ?2
                     WHERE task_id = ?1 AND state = 'in_doubt'",
                    params![id, now()],
                )?;
                set_state(
                    &tx,
                    id,
                    TaskState::Queued,
                    Some("retry approved by the user"),
                    "resolved_retry",
                )?;
            }
            Resolution::Cancel => {
                set_state(
                    &tx,
                    id,
                    TaskState::Cancelled,
                    Some("cancelled by the user"),
                    "resolved_cancel",
                )?;
            }
        }
        let task = load(&tx, id)?;
        tx.commit()?;
        Ok(task)
    }

    /// Record the intent to perform an external effect before performing it.
    ///
    /// The task may be in any state except cancelled (an owner can publish the result of a
    /// finished task). Keys are unique per task.
    pub fn begin_effect(&self, task_id: &str, key: &str, description: &str) -> Result<EffectGate> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let task = load(&tx, task_id)?;
        if task.state == TaskState::Cancelled {
            return Err(ContinuityError::Cancelled(task_id.to_string()));
        }
        let existing: Option<(String, String, Option<String>)> = tx
            .query_row(
                "SELECT state, boot_id, receipt FROM task_effects WHERE task_id = ?1 AND key = ?2",
                params![task_id, key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        match existing {
            Some((state, _, Some(receipt))) if state == "committed" => {
                return Ok(EffectGate::AlreadyDone { receipt });
            }
            Some((state, boot, _)) if state == "intended" && boot == self.boot_id => {
                return Err(ContinuityError::EffectInProgress {
                    task: task_id.into(),
                    key: key.into(),
                });
            }
            Some((state, _, _)) if state == "intended" || state == "in_doubt" => {
                return Err(ContinuityError::EffectInDoubt {
                    task: task_id.into(),
                    key: key.into(),
                });
            }
            _ => {}
        }
        tx.execute(
            "INSERT INTO task_effects (task_id, key, description, state, boot_id, started_at)
             VALUES (?1, ?2, ?3, 'intended', ?4, ?5)
             ON CONFLICT(task_id, key) DO UPDATE SET
                 description = excluded.description, state = 'intended',
                 boot_id = excluded.boot_id, receipt = NULL,
                 started_at = excluded.started_at, finished_at = NULL",
            params![task_id, key, description, self.boot_id, now()],
        )?;
        push_event(&tx, task_id, "effect_started", description)?;
        tx.commit()?;
        Ok(EffectGate::Proceed)
    }

    /// The effect happened; store its receipt so it is never repeated.
    pub fn commit_effect(&self, task_id: &str, key: &str, receipt: &str) -> Result<()> {
        self.end_effect(task_id, key, "committed", receipt)
    }

    /// The effect did not happen; it may be attempted again.
    pub fn fail_effect(&self, task_id: &str, key: &str, reason: &str) -> Result<()> {
        self.end_effect(task_id, key, "failed", reason)
    }

    fn end_effect(&self, task_id: &str, key: &str, state: &str, receipt: &str) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE task_effects SET state = ?3, receipt = ?4, finished_at = ?5
             WHERE task_id = ?1 AND key = ?2 AND state = 'intended' AND boot_id = ?6",
            params![task_id, key, state, receipt, now(), self.boot_id],
        )?;
        if changed == 0 {
            return Err(ContinuityError::EffectInDoubt {
                task: task_id.into(),
                key: key.into(),
            });
        }
        push_event(
            &tx,
            task_id,
            &format!("effect_{state}"),
            &format!("{key}: {receipt}"),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Task> {
        load(&self.conn(), id)
    }

    /// Tasks, most recently updated first.
    pub fn list(&self, filter: &TaskFilter) -> Result<Vec<Task>> {
        let conn = self.conn();
        let mut sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE 1 = 1");
        if filter.owner.is_some() {
            sql.push_str(" AND owner = ?1");
        }
        if !filter.include_terminal {
            sql.push_str(" AND state NOT IN ('succeeded', 'failed', 'cancelled')");
        }
        sql.push_str(" ORDER BY updated_at DESC");
        if let Some(limit) = filter.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        let mut stmt = conn.prepare(&sql)?;
        let rows = match &filter.owner {
            Some(owner) => stmt.query_map(params![owner], task_from_row)?,
            None => stmt.query_map([], task_from_row)?,
        }
        .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(finish_row).collect()
    }

    /// Bounded event history after `after_seq`, oldest first.
    pub fn events(&self, id: &str, after_seq: i64, limit: usize) -> Result<Vec<TaskEvent>> {
        let conn = self.conn();
        load(&conn, id)?;
        let mut stmt = conn.prepare(
            "SELECT seq, at, kind, detail FROM task_events
             WHERE task_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![id, after_seq, limit as i64], |r| {
                Ok(TaskEvent {
                    seq: r.get(0)?,
                    at: r.get(1)?,
                    kind: r.get(2)?,
                    detail: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Delete a task that is not running (e.g. the user cleared it from the UI).
    pub fn remove(&self, id: &str) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let task = match load(&tx, id) {
            Ok(task) => task,
            Err(ContinuityError::NotFound(_)) => return Ok(()),
            Err(e) => return Err(e),
        };
        if task.state == TaskState::Running {
            return Err(invalid(&task, "remove"));
        }
        tx.execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }
}
