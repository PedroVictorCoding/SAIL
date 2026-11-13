//! Curiosity Engine (#453): bounded idle-time investigations of the owner's open questions.
//!
//! "Investigate further" on an inbox item, or an open question on the agenda, becomes a
//! small piece of background work: the local model looks through what SAIL already knows
//! (memory, school data) in at most [`CuriosityLimits::max_tool_steps`] steps and
//! [`CuriosityLimits::max_tokens`] tokens, and writes short notes. The notes come back as a
//! `Hypothesis` signal (digest by default), never as the owner's words.
//!
//! Everything here is deterministic except the [`Investigator`], which Dialogue implements
//! with the local model. The rules:
//!
//! - **When ([`admit`]).** Never under `Off` or `Quiet` autonomy, during live voice, in
//!   quiet hours or focus, or before the owner has been idle for `min_idle`. At most
//!   `runs_per_day` runs per UTC day; one at a time.
//! - **What ([`pick`]).** Owner-requested investigations (`AgendaKind::Investigation`)
//!   under `Balanced` or `Proactive`; open questions (`OpenQuestion`) only under
//!   `Proactive`. An item is not revisited within `item_cooldown`. Investigations first,
//!   then items never looked into, then the longest since their last run.
//! - **Receipts.** Every run, including failed and cancelled ones, is recorded in the
//!   [`CuriosityLedger`] with tokens and steps used. Receipts are what `runs_per_day`
//!   counts, so a crash loop cannot spend more than the daily allowance.
//! - **Escalation.** The investigator may ask a stronger model only through
//!   `escalate_reasoning` with `Origin::Background`; that job's id is on the receipt, and
//!   its answer arrives later as its own signal.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, params};
use sail_pulse::attention::Autonomy;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::agenda::{AgendaItem, AgendaKind, AgendaStatus, AgendaStore};
use crate::detect::{Observation, excerpt};

pub const MAX_SUMMARY_CHARS: usize = 1_200;
pub const MAX_SOURCES: usize = 6;
pub const MAX_SOURCE_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CuriosityLimits {
    pub runs_per_day: u32,
    pub max_tool_steps: u32,
    pub max_tokens: u32,
    /// Minutes the owner must have been idle (no conversation turn).
    pub min_idle_minutes: i64,
    /// Hours before the same item is looked into again.
    pub item_cooldown_hours: i64,
    /// Wall-clock cap for one run, in seconds.
    pub max_run_secs: u64,
}

impl Default for CuriosityLimits {
    fn default() -> Self {
        Self {
            runs_per_day: 3,
            max_tool_steps: 4,
            max_tokens: 2_000,
            min_idle_minutes: 20,
            item_cooldown_hours: 24,
            max_run_secs: 240,
        }
    }
}

impl CuriosityLimits {
    pub fn max_run(&self) -> StdDuration {
        StdDuration::from_secs(self.max_run_secs.clamp(1, 900))
    }
}

/// Facts [`admit`] decides on, gathered by the caller.
#[derive(Debug, Clone, PartialEq)]
pub struct Gate {
    pub autonomy: Autonomy,
    pub voice_active: bool,
    pub quiet_hours: bool,
    pub focus: bool,
    /// Time since the last conversation turn; `None` when there has been none.
    pub idle_for: Option<Duration>,
    pub runs_today: u32,
    pub running: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Skip {
    Autonomy,
    Voice,
    QuietHours,
    Focus,
    NotIdle,
    DailyLimit,
    Running,
    NothingToDo,
}

pub fn admit(gate: &Gate, limits: &CuriosityLimits) -> Result<(), Skip> {
    if matches!(gate.autonomy, Autonomy::Off | Autonomy::Quiet) {
        return Err(Skip::Autonomy);
    }
    if gate.voice_active {
        return Err(Skip::Voice);
    }
    if gate.running {
        return Err(Skip::Running);
    }
    if gate.quiet_hours {
        return Err(Skip::QuietHours);
    }
    if gate.focus {
        return Err(Skip::Focus);
    }
    if gate
        .idle_for
        .is_some_and(|idle| idle < Duration::minutes(limits.min_idle_minutes))
    {
        return Err(Skip::NotIdle);
    }
    if gate.runs_today >= limits.runs_per_day {
        return Err(Skip::DailyLimit);
    }
    Ok(())
}

/// The item to look into next, if any.
pub fn pick(
    items: &[AgendaItem],
    last_runs: &HashMap<String, DateTime<Utc>>,
    autonomy: Autonomy,
    limits: &CuriosityLimits,
    now: DateTime<Utc>,
) -> Option<AgendaItem> {
    let cooldown = Duration::hours(limits.item_cooldown_hours);
    let mut eligible: Vec<&AgendaItem> = items
        .iter()
        .filter(|item| matches!(item.status, AgendaStatus::Open | AgendaStatus::InProgress))
        .filter(|item| match item.kind {
            AgendaKind::Investigation => autonomy >= Autonomy::Balanced,
            AgendaKind::OpenQuestion => autonomy >= Autonomy::Proactive,
            _ => false,
        })
        .filter(|item| {
            last_runs
                .get(&item.id)
                .is_none_or(|at| now - *at >= cooldown)
        })
        .collect();
    eligible.sort_by_key(|item| {
        (
            item.kind != AgendaKind::Investigation,
            last_runs.get(&item.id).copied(),
            item.updated_at,
        )
    });
    eligible.first().map(|item| (*item).clone())
}

/// What the investigator is asked to look into.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Brief {
    pub item_id: String,
    pub title: String,
    pub detail: String,
    pub last_evidence: Option<String>,
}

impl From<&AgendaItem> for Brief {
    fn from(item: &AgendaItem) -> Self {
        Self {
            item_id: item.id.clone(),
            title: item.title.clone(),
            detail: item.detail.clone(),
            last_evidence: item.last_evidence.clone(),
        }
    }
}

/// What one investigation produced.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Finding {
    /// SAIL's own notes (bounded); never phrased as the owner's words.
    pub summary: String,
    /// Where the notes come from, e.g. `memory: …`, `moodle: …` (bounded).
    pub sources: Vec<String>,
    pub tokens_used: u64,
    pub tool_steps: u32,
    /// A Background escalation the investigator started, if any.
    pub escalation_job: Option<String>,
    /// False when the investigator found nothing worth telling.
    pub conclusive: bool,
}

#[async_trait]
pub trait Investigator: Send + Sync {
    async fn investigate(
        &self,
        brief: &Brief,
        limits: &CuriosityLimits,
        cancel: CancellationToken,
    ) -> Result<Finding, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Finding,
    NothingFound,
    Failed,
    Cancelled,
}

/// The budget receipt for one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub id: String,
    pub item_id: String,
    pub item_title: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub outcome: RunOutcome,
    pub tokens_used: u64,
    pub tool_steps: u32,
    pub escalation_job: Option<String>,
    /// Short summary or error.
    pub note: String,
}

/// Run receipts, in `$SAIL_DATA/discovery/curiosity.db`.
pub struct CuriosityLedger {
    conn: Mutex<Connection>,
}

const LEDGER_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS runs (
    id TEXT PRIMARY KEY,
    item_id TEXT NOT NULL,
    item_title TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER NOT NULL,
    outcome TEXT NOT NULL,
    tokens_used INTEGER NOT NULL,
    tool_steps INTEGER NOT NULL,
    escalation_job TEXT,
    note TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS runs_started ON runs(started_at);
CREATE INDEX IF NOT EXISTS runs_item ON runs(item_id, started_at);
";

impl CuriosityLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| e.to_string())?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, String> {
        Self::init(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(LEDGER_SCHEMA)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn record(&self, receipt: &Receipt) -> Result<(), String> {
        let outcome = serde_json::to_value(receipt.outcome)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        self.lock()
            .execute(
                "INSERT OR REPLACE INTO runs (id, item_id, item_title, started_at, finished_at,
                 outcome, tokens_used, tool_steps, escalation_job, note)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    receipt.id,
                    receipt.item_id,
                    excerpt(&receipt.item_title, 200),
                    receipt.started_at.timestamp_millis(),
                    receipt.finished_at.timestamp_millis(),
                    outcome,
                    receipt.tokens_used as i64,
                    receipt.tool_steps,
                    receipt.escalation_job,
                    excerpt(&receipt.note, 400),
                ],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Runs started on `now`'s UTC day.
    pub fn runs_today(&self, now: DateTime<Utc>) -> Result<u32, String> {
        let day = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|d| d.and_utc().timestamp_millis())
            .unwrap_or_default();
        self.lock()
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE started_at >= ?1",
                params![day],
                |row| row.get::<_, u32>(0),
            )
            .map_err(|e| e.to_string())
    }

    /// Latest run start per item.
    pub fn last_runs(&self) -> Result<HashMap<String, DateTime<Utc>>, String> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT item_id, MAX(started_at) FROM runs GROUP BY item_id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| e.to_string())?;
        let mut out = HashMap::new();
        for row in rows {
            let (item, ms) = row.map_err(|e| e.to_string())?;
            if let Some(at) = DateTime::from_timestamp_millis(ms) {
                out.insert(item, at);
            }
        }
        Ok(out)
    }

    /// Newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<Receipt>, String> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, item_id, item_title, started_at, finished_at, outcome, tokens_used,
                 tool_steps, escalation_job, note FROM runs ORDER BY started_at DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                let outcome: String = row.get(5)?;
                Ok(Receipt {
                    id: row.get(0)?,
                    item_id: row.get(1)?,
                    item_title: row.get(2)?,
                    started_at: DateTime::from_timestamp_millis(row.get(3)?).unwrap_or_default(),
                    finished_at: DateTime::from_timestamp_millis(row.get(4)?).unwrap_or_default(),
                    outcome: serde_json::from_value(serde_json::Value::String(outcome))
                        .unwrap_or(RunOutcome::Failed),
                    tokens_used: row.get::<_, i64>(6)?.max(0) as u64,
                    tool_steps: row.get(7)?,
                    escalation_job: row.get(8)?,
                    note: row.get(9)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())
    }
}

/// The result of [`run_once`].
#[derive(Debug, Clone, PartialEq)]
pub struct RunReport {
    pub receipt: Receipt,
    /// The finding as an observation for the initiative loop (conclusive findings only).
    pub observation: Option<Observation>,
}

/// One admitted, bounded investigation: pick, investigate, record the receipt, and turn a
/// conclusive finding into an observation. The caller checks [`admit`] first.
pub async fn run_once(
    agenda: &AgendaStore,
    ledger: &CuriosityLedger,
    investigator: &dyn Investigator,
    autonomy: Autonomy,
    limits: &CuriosityLimits,
    cancel: CancellationToken,
) -> Result<RunReport, Skip> {
    let now = Utc::now();
    let items = agenda.open_items(200).map_err(|_| Skip::NothingToDo)?;
    let last_runs = ledger.last_runs().unwrap_or_default();
    let item = pick(&items, &last_runs, autonomy, limits, now).ok_or(Skip::NothingToDo)?;
    let brief = Brief::from(&item);
    let started_at = now_ms();
    let run = investigator.investigate(&brief, limits, cancel.clone());
    let result = tokio::select! {
        result = run => result,
        _ = tokio::time::sleep(limits.max_run()) => {
            cancel.cancel();
            Err("the investigation ran out of time".into())
        }
    };
    let finished_at = now_ms();
    let finding = match &result {
        Ok(finding) => Some(bounded(finding.clone())),
        Err(_) => None,
    };
    let outcome = match (&finding, cancel.is_cancelled()) {
        (Some(f), _) if f.conclusive && !f.summary.trim().is_empty() => RunOutcome::Finding,
        (Some(_), _) => RunOutcome::NothingFound,
        (None, true) => RunOutcome::Cancelled,
        (None, false) => RunOutcome::Failed,
    };
    let receipt = Receipt {
        id: uuid::Uuid::new_v4().to_string(),
        item_id: item.id.clone(),
        item_title: item.title.clone(),
        started_at,
        finished_at,
        outcome,
        tokens_used: finding.as_ref().map_or(0, |f| f.tokens_used),
        tool_steps: finding.as_ref().map_or(0, |f| f.tool_steps),
        escalation_job: finding.as_ref().and_then(|f| f.escalation_job.clone()),
        note: match (&finding, &result) {
            (Some(f), _) => excerpt(&f.summary, 400),
            (None, Err(error)) => excerpt(error, 400),
            (None, Ok(_)) => String::new(),
        },
    };
    if let Err(error) = ledger.record(&receipt) {
        tracing::warn!(%error, "curiosity receipt not recorded");
    }
    if item.status == AgendaStatus::Open {
        let _ = agenda.set_status(&item.id, AgendaStatus::InProgress, finished_at);
    }
    let observation = match (&finding, outcome) {
        (Some(finding), RunOutcome::Finding) => Some(Observation::CuriosityFinding {
            run_id: receipt.id.clone(),
            item_id: item.id.clone(),
            title: item.title.clone(),
            summary: finding.summary.clone(),
            sources: finding.sources.clone(),
        }),
        _ => None,
    };
    Ok(RunReport {
        receipt,
        observation,
    })
}

fn bounded(mut finding: Finding) -> Finding {
    finding.summary = excerpt(&finding.summary, MAX_SUMMARY_CHARS);
    finding.sources = finding
        .sources
        .iter()
        .map(|s| excerpt(s, MAX_SOURCE_CHARS))
        .filter(|s| !s.is_empty())
        .take(MAX_SOURCES)
        .collect();
    finding
}

/// Now, at the ledger's millisecond precision.
fn now_ms() -> DateTime<Utc> {
    let now = Utc::now();
    DateTime::from_timestamp_millis(now.timestamp_millis()).unwrap_or(now)
}
