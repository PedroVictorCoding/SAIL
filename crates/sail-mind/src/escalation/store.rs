//! Durable escalation ledger (Kilo). Spec: the module doc in `escalation/mod.rs`.
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use super::{
    DecisionRecord, EscalationJob, EscalationPolicy, JobState, MAX_ATTEMPTS, MAX_STORED_BYTES,
    Origin, Outcome, OutcomeStats, TaskKind, UsageTotals,
};

/// Cache rows are kept for this long; the policy's shorter TTL is applied when reading.
const CACHE_HORIZON_SECS: i64 = 30 * 24 * 3600;

const JOB_COLUMNS: &str = "id, fingerprint, kind, origin, provider, model, budget, state, \
     attempts, question, justification, expected_benefit, answer, error, input_tokens, \
     output_tokens, cost_micros, correlation_id, created_at, finished_at, outcome";

const JOB_COLUMNS_JOINED: &str = "j.id, j.fingerprint, j.kind, j.origin, j.provider, j.model, \
     j.budget, j.state, j.attempts, j.question, j.justification, j.expected_benefit, j.answer, \
     j.error, j.input_tokens, j.output_tokens, j.cost_micros, j.correlation_id, j.created_at, \
     j.finished_at, j.outcome";

const SCHEMA: &str = "\
PRAGMA journal_mode=WAL;
PRAGMA busy_timeout=5000;
CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    kind TEXT NOT NULL,
    origin TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    budget INTEGER NOT NULL,
    state TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    question TEXT NOT NULL,
    justification TEXT NOT NULL,
    expected_benefit TEXT NOT NULL,
    answer TEXT,
    error TEXT,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cost_micros INTEGER NOT NULL DEFAULT 0,
    correlation_id TEXT,
    created_at INTEGER NOT NULL,
    finished_at INTEGER,
    outcome TEXT
);
CREATE INDEX IF NOT EXISTS jobs_fingerprint ON jobs(fingerprint);
CREATE INDEX IF NOT EXISTS jobs_state ON jobs(state);
CREATE TABLE IF NOT EXISTS usage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider TEXT NOT NULL,
    kind TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cost_micros INTEGER NOT NULL,
    at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS usage_at ON usage(at);
CREATE TABLE IF NOT EXISTS cache (
    fingerprint TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    answer TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS outcomes (
    job_id TEXT PRIMARY KEY,
    outcome TEXT NOT NULL,
    at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS decisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at INTEGER NOT NULL,
    kind TEXT NOT NULL,
    origin TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    decision TEXT NOT NULL,
    route_score REAL NOT NULL,
    job_id TEXT
);
CREATE INDEX IF NOT EXISTS decisions_at ON decisions(at);
CREATE TABLE IF NOT EXISTS provider_blocks (
    provider TEXT PRIMARY KEY,
    blocked_until INTEGER NOT NULL
);
";

pub struct EscalationStore {
    conn: Mutex<Connection>,
}

impl EscalationStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        Self::init(Connection::open(path).map_err(|error| error.to_string())?)
    }

    pub fn open_in_memory() -> Result<Self, String> {
        Self::init(Connection::open_in_memory().map_err(|error| error.to_string())?)
    }

    fn init(conn: Connection) -> Result<Self, String> {
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if version > 2 {
            return Err(format!(
                "escalation database schema version {version} is newer than this build"
            ));
        }
        conn.execute_batch(SCHEMA)
            .map_err(|error| error.to_string())?;
        if version < 2 {
            // v2: outcomes know who recorded them; the owner's word beats a check.
            let has_source = conn
                .prepare("SELECT 1 FROM pragma_table_info('outcomes') WHERE name = 'source'")
                .and_then(|mut stmt| stmt.exists([]))
                .map_err(|error| error.to_string())?;
            if !has_source {
                conn.execute_batch(
                    "ALTER TABLE outcomes ADD COLUMN source TEXT NOT NULL DEFAULT 'owner';
                     ALTER TABLE outcomes ADD COLUMN check_name TEXT;",
                )
                .map_err(|error| error.to_string())?;
            }
            conn.pragma_update(None, "user_version", 2)
                .map_err(|error| error.to_string())?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Totals for `now`'s UTC day/month, rate-limit blocks still in force, in-flight
    /// fingerprints, and fingerprints finished within `policy.cooldown_secs`.
    pub fn usage(
        &self,
        policy: &EscalationPolicy,
        now: DateTime<Utc>,
    ) -> Result<UsageTotals, String> {
        let conn = self.conn();
        let now_ms = now.timestamp_millis();
        let day_start = Utc
            .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
            .single()
            .map(|at| at.timestamp_millis())
            .unwrap_or(now_ms);
        let month_start = Utc
            .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
            .single()
            .map(|at| at.timestamp_millis())
            .unwrap_or(now_ms);
        let (tokens_today, cost_today_micros, calls_today) = conn
            .query_row(
                "SELECT COALESCE(SUM(input_tokens + output_tokens), 0),
                        COALESCE(SUM(cost_micros), 0), COUNT(*)
                 FROM usage WHERE at >= ?1",
                params![day_start],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|error| error.to_string())?;
        let (tokens_month, cost_month_micros) = conn
            .query_row(
                "SELECT COALESCE(SUM(input_tokens + output_tokens), 0),
                        COALESCE(SUM(cost_micros), 0)
                 FROM usage WHERE at >= ?1",
                params![month_start],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| error.to_string())?;
        let blocked_until = {
            let mut statement = conn
                .prepare(
                    "SELECT provider, blocked_until FROM provider_blocks
                     WHERE blocked_until > ?1 ORDER BY provider",
                )
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map(params![now_ms], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|error| error.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|(provider, until)| (provider, millis(until)))
                .collect()
        };
        let in_flight = {
            let mut statement = conn
                .prepare(
                    "SELECT DISTINCT fingerprint FROM jobs
                     WHERE state IN ('pending', 'running') ORDER BY fingerprint",
                )
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| error.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|error| error.to_string())?
        };
        let recent = if policy.cooldown_secs == 0 {
            Vec::new()
        } else {
            let cooldown_ms = seconds_to_millis(policy.cooldown_secs);
            let cutoff = now_ms.saturating_sub(cooldown_ms);
            let mut statement = conn
                .prepare(
                    "SELECT fingerprint, MAX(finished_at) FROM jobs
                     WHERE state = 'done' AND finished_at IS NOT NULL AND finished_at >= ?1
                     GROUP BY fingerprint ORDER BY fingerprint",
                )
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map(params![cutoff], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|error| error.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|(fingerprint, finished)| (fingerprint, millis(finished)))
                .collect()
        };
        Ok(UsageTotals {
            tokens_today,
            tokens_month,
            cost_today_micros,
            cost_month_micros,
            calls_today,
            blocked_until,
            in_flight,
            recent,
        })
    }

    pub fn outcomes(&self) -> Result<OutcomeStats, String> {
        let conn = self.conn();
        let mut statement = conn
            .prepare(
                "SELECT j.kind, o.outcome, COUNT(*) FROM outcomes o
                 JOIN jobs j ON j.id = o.job_id
                 GROUP BY j.kind, o.outcome",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u32>(2)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        let mut by_kind: Vec<(TaskKind, u32, u32)> = Vec::new();
        for row in rows {
            let (kind, outcome, count) = row.map_err(|error| error.to_string())?;
            let kind = TaskKind::parse(&kind).unwrap_or(TaskKind::Reasoning);
            let index = match by_kind.iter().position(|(known, _, _)| *known == kind) {
                Some(index) => index,
                None => {
                    by_kind.push((kind, 0, 0));
                    by_kind.len() - 1
                }
            };
            let entry = &mut by_kind[index];
            if outcome == "improved" {
                entry.1 = entry.1.saturating_add(count);
            } else {
                entry.2 = entry.2.saturating_add(count);
            }
        }
        by_kind.sort_by_key(|(kind, _, _)| {
            TaskKind::ALL
                .iter()
                .position(|known| known == kind)
                .unwrap_or(usize::MAX)
        });
        Ok(OutcomeStats { by_kind })
    }

    /// A `Done` job with this fingerprint finished within `ttl_secs` of `now`.
    pub fn cached(
        &self,
        fingerprint: &str,
        ttl_secs: u64,
        now: DateTime<Utc>,
    ) -> Result<Option<EscalationJob>, String> {
        if ttl_secs == 0 {
            return Ok(None);
        }
        let conn = self.conn();
        let now_ms = now.timestamp_millis();
        let cutoff = now_ms.saturating_sub(seconds_to_millis(ttl_secs));
        let sql = format!(
            "SELECT {JOB_COLUMNS_JOINED} FROM cache c JOIN jobs j ON j.id = c.job_id
             WHERE c.fingerprint = ?1 AND c.created_at >= ?2 AND c.expires_at > ?3
               AND j.state = 'done'
             ORDER BY c.created_at DESC LIMIT 1"
        );
        conn.query_row(&sql, params![fingerprint, cutoff, now_ms], job_from_row)
            .optional()
            .map_err(|error| error.to_string())
    }

    /// Insert a new job (state `Pending`). Text fields are cut to `MAX_STORED_BYTES`.
    pub fn insert_job(&self, job: &EscalationJob) -> Result<(), String> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO jobs (
                id, fingerprint, kind, origin, provider, model, budget, state, attempts,
                question, justification, expected_benefit, answer, error, input_tokens,
                output_tokens, cost_micros, correlation_id, created_at, finished_at, outcome
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                       ?16, ?17, ?18, ?19, ?20, ?21)",
            params![
                job.id,
                job.fingerprint,
                job.kind.as_str(),
                origin_str(job.origin),
                job.provider,
                job.model,
                job.budget,
                state_str(job.state),
                job.attempts,
                cut(&job.question),
                cut(&job.justification),
                cut(&job.expected_benefit),
                job.answer.as_deref().map(cut),
                job.error.as_deref().map(cut),
                job.input_tokens,
                job.output_tokens,
                job.cost_micros,
                job.correlation_id.as_deref().map(cut),
                job.created_at.timestamp_millis(),
                job.finished_at.map(|at| at.timestamp_millis()),
                job.outcome.map(outcome_str),
            ],
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Move a job to `Running` and increment `attempts`.
    pub fn mark_running(&self, id: &str) -> Result<(), String> {
        let conn = self.conn();
        let changed = conn
            .execute(
                "UPDATE jobs SET state = 'running', attempts = attempts + 1,
                 finished_at = NULL, error = NULL
                 WHERE id = ?1",
                params![id],
            )
            .map_err(|error| error.to_string())?;
        if changed == 0 {
            return Err(format!("unknown escalation job {id}"));
        }
        Ok(())
    }

    /// Finish a job: `Done` with answer and usage, which also writes one `usage` row.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_job(
        &self,
        id: &str,
        answer: &str,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cost_micros: u64,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let (fingerprint, provider, kind): (String, String, String) = tx
            .query_row(
                "SELECT fingerprint, provider, kind FROM jobs WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("unknown escalation job {id}"))?;
        let now_ms = now.timestamp_millis();
        let answer = cut(answer);
        tx.execute(
            "UPDATE jobs SET state = 'done', answer = ?2, model = ?3, error = NULL,
             input_tokens = ?4, output_tokens = ?5, cost_micros = ?6, finished_at = ?7
             WHERE id = ?1",
            params![
                id,
                answer,
                model,
                input_tokens,
                output_tokens,
                cost_micros,
                now_ms
            ],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT INTO usage (provider, kind, input_tokens, output_tokens, cost_micros, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                provider,
                kind,
                input_tokens,
                output_tokens,
                cost_micros,
                now_ms
            ],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT INTO cache (fingerprint, job_id, answer, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(fingerprint) DO UPDATE SET
                job_id = excluded.job_id,
                answer = excluded.answer,
                created_at = excluded.created_at,
                expires_at = excluded.expires_at",
            params![
                fingerprint,
                id,
                answer,
                now_ms,
                now_ms.saturating_add(CACHE_HORIZON_SECS.saturating_mul(1000))
            ],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(())
    }

    /// `Failed` (or back to `Pending` when `retry` and attempts < MAX_ATTEMPTS).
    pub fn fail_job(
        &self,
        id: &str,
        error: &str,
        retry: bool,
        now: DateTime<Utc>,
    ) -> Result<JobState, String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let attempts: i64 = tx
            .query_row(
                "SELECT attempts FROM jobs WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("unknown escalation job {id}"))?;
        let state = if retry && attempts < i64::from(MAX_ATTEMPTS) {
            JobState::Pending
        } else {
            JobState::Failed
        };
        let finished_at = (state == JobState::Failed).then_some(now.timestamp_millis());
        tx.execute(
            "UPDATE jobs SET state = ?2, error = ?3, finished_at = ?4 WHERE id = ?1",
            params![id, state_str(state), cut(error), finished_at],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(state)
    }

    /// Record a provider rate limit until `until`.
    pub fn block_provider(&self, provider: &str, until: DateTime<Utc>) -> Result<(), String> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO provider_blocks (provider, blocked_until) VALUES (?1, ?2)
             ON CONFLICT(provider) DO UPDATE SET
                blocked_until = MAX(blocked_until, excluded.blocked_until)",
            params![provider, until.timestamp_millis()],
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn job(&self, id: &str) -> Result<Option<EscalationJob>, String> {
        let conn = self.conn();
        let sql = format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = ?1");
        conn.query_row(&sql, params![id], job_from_row)
            .optional()
            .map_err(|error| error.to_string())
    }

    /// Newest first.
    pub fn jobs(&self, limit: usize) -> Result<Vec<EscalationJob>, String> {
        let conn = self.conn();
        let sql =
            format!("SELECT {JOB_COLUMNS} FROM jobs ORDER BY created_at DESC, rowid DESC LIMIT ?1");
        let mut statement = conn.prepare(&sql).map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params![limit as i64], job_from_row)
            .map_err(|error| error.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| error.to_string())
    }

    /// Jobs left `Running` by a previous process go back to `Pending`; returns pending jobs.
    pub fn recover(&self) -> Result<Vec<EscalationJob>, String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let now_ms = Utc::now().timestamp_millis();
        tx.execute(
            "UPDATE jobs SET state = 'failed', finished_at = ?1, error = ?2
             WHERE state IN ('pending', 'running') AND attempts >= ?3",
            params![now_ms, "too many attempts", MAX_ATTEMPTS],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE jobs SET state = 'pending' WHERE state = 'running'",
            [],
        )
        .map_err(|error| error.to_string())?;
        let sql = format!(
            "SELECT {JOB_COLUMNS} FROM jobs WHERE state = 'pending'
             ORDER BY created_at ASC, rowid ASC"
        );
        let pending = {
            let mut statement = tx.prepare(&sql).map_err(|error| error.to_string())?;
            let rows = statement
                .query_map([], job_from_row)
                .map_err(|error| error.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|error| error.to_string())?
        };
        tx.commit().map_err(|error| error.to_string())?;
        Ok(pending)
    }

    /// Set (or replace) a job's outcome as the owner. Unknown job → error.
    pub fn record_outcome(&self, id: &str, outcome: Outcome) -> Result<(), String> {
        self.write_outcome(id, outcome, None).map(|_| ())
    }

    /// Record an outcome measured by an automatic check (`check` names it, e.g.
    /// `forge_tests`, `adopted_in_reply`). Never replaces an outcome the owner gave; a later
    /// check replaces an earlier check. Returns whether it was recorded.
    pub fn record_check_outcome(
        &self,
        id: &str,
        outcome: Outcome,
        check: &str,
    ) -> Result<bool, String> {
        self.write_outcome(id, outcome, Some(check))
    }

    /// Job ids with this correlation id, newest first (at most 20).
    pub fn jobs_by_correlation(&self, correlation_id: &str) -> Result<Vec<String>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id FROM jobs WHERE correlation_id = ?1
                 ORDER BY created_at DESC LIMIT 20",
            )
            .map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map(params![correlation_id], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| error.to_string())
    }

    fn write_outcome(
        &self,
        id: &str,
        outcome: Outcome,
        check: Option<&str>,
    ) -> Result<bool, String> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let exists: Option<i64> = tx
            .query_row("SELECT 1 FROM jobs WHERE id = ?1", params![id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|error| error.to_string())?;
        if exists.is_none() {
            return Err(format!("unknown escalation job {id}"));
        }
        if check.is_some() {
            let owner_said: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM outcomes WHERE job_id = ?1 AND source = 'owner'",
                    params![id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| error.to_string())?;
            if owner_said.is_some() {
                return Ok(false);
            }
        }
        let check_name = check.map(|c| c.chars().take(64).collect::<String>());
        tx.execute(
            "INSERT INTO outcomes (job_id, outcome, at, source, check_name)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(job_id) DO UPDATE SET
                outcome = excluded.outcome,
                at = excluded.at,
                source = excluded.source,
                check_name = excluded.check_name",
            params![
                id,
                outcome_str(outcome),
                Utc::now().timestamp_millis(),
                if check.is_some() { "check" } else { "owner" },
                check_name
            ],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE jobs SET outcome = ?2 WHERE id = ?1",
            params![id, outcome_str(outcome)],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(true)
    }

    pub fn log_decision(&self, record: &DecisionRecord) -> Result<(), String> {
        let conn = self.conn();
        let decision =
            serde_json::to_string(&record.decision).map_err(|error| error.to_string())?;
        conn.execute(
            "INSERT INTO decisions (at, kind, origin, fingerprint, decision, route_score, job_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.at.timestamp_millis(),
                record.kind.as_str(),
                origin_str(record.origin),
                record.fingerprint,
                decision,
                f64::from(record.route_score),
                record.job_id,
            ],
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Newest first.
    pub fn decisions(&self, limit: usize) -> Result<Vec<DecisionRecord>, String> {
        let conn = self.conn();
        let mut statement = conn
            .prepare(
                "SELECT at, kind, origin, fingerprint, decision, route_score, job_id
                 FROM decisions ORDER BY at DESC, id DESC LIMIT ?1",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params![limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        let mut records = Vec::new();
        for row in rows {
            let (at, kind, origin, fingerprint, decision, score, job_id) =
                row.map_err(|error| error.to_string())?;
            records.push(DecisionRecord {
                at: millis(at),
                kind: TaskKind::parse(&kind).unwrap_or(TaskKind::Reasoning),
                origin: parse_origin(&origin),
                fingerprint,
                decision: serde_json::from_str(&decision)
                    .map_err(|error| format!("stored decision is invalid: {error}"))?,
                route_score: score as f32,
                job_id,
            });
        }
        Ok(records)
    }

    /// Delete usage/decision rows older than `keep_days` and expired cache rows.
    pub fn prune(&self, keep_days: u32, now: DateTime<Utc>) -> Result<(), String> {
        let conn = self.conn();
        let now_ms = now.timestamp_millis();
        let cutoff = now_ms.saturating_sub(seconds_to_millis(u64::from(keep_days) * 24 * 3600));
        conn.execute("DELETE FROM usage WHERE at < ?1", params![cutoff])
            .map_err(|error| error.to_string())?;
        conn.execute("DELETE FROM decisions WHERE at < ?1", params![cutoff])
            .map_err(|error| error.to_string())?;
        conn.execute("DELETE FROM cache WHERE expires_at <= ?1", params![now_ms])
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn millis(ms: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(ms).unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

fn seconds_to_millis(seconds: u64) -> i64 {
    i64::try_from(seconds)
        .unwrap_or(i64::MAX / 1000)
        .saturating_mul(1000)
}

fn cut(text: &str) -> String {
    if text.len() <= MAX_STORED_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_STORED_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn origin_str(origin: Origin) -> &'static str {
    match origin {
        Origin::ModelTool => "model_tool",
        Origin::VoiceTurn => "voice_turn",
        Origin::Background => "background",
        Origin::Owner => "owner",
    }
}

fn parse_origin(value: &str) -> Origin {
    match value {
        "voice_turn" => Origin::VoiceTurn,
        "background" => Origin::Background,
        "owner" => Origin::Owner,
        _ => Origin::ModelTool,
    }
}

fn state_str(state: JobState) -> &'static str {
    match state {
        JobState::Pending => "pending",
        JobState::Running => "running",
        JobState::Done => "done",
        JobState::Failed => "failed",
    }
}

fn parse_state(value: &str) -> JobState {
    match value {
        "running" => JobState::Running,
        "done" => JobState::Done,
        "failed" => JobState::Failed,
        _ => JobState::Pending,
    }
}

fn outcome_str(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Improved => "improved",
        Outcome::NoChange => "no_change",
        Outcome::Worse => "worse",
    }
}

fn parse_outcome(value: &str) -> Option<Outcome> {
    match value {
        "improved" => Some(Outcome::Improved),
        "no_change" => Some(Outcome::NoChange),
        "worse" => Some(Outcome::Worse),
        _ => None,
    }
}

fn job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EscalationJob> {
    let kind: String = row.get(2)?;
    let origin: String = row.get(3)?;
    let state: String = row.get(7)?;
    Ok(EscalationJob {
        id: row.get(0)?,
        fingerprint: row.get(1)?,
        kind: TaskKind::parse(&kind).unwrap_or(TaskKind::Reasoning),
        origin: parse_origin(&origin),
        provider: row.get(4)?,
        model: row.get(5)?,
        budget: row.get(6)?,
        state: parse_state(&state),
        attempts: row.get(8)?,
        question: row.get(9)?,
        justification: row.get(10)?,
        expected_benefit: row.get(11)?,
        answer: row.get(12)?,
        error: row.get(13)?,
        input_tokens: row.get(14)?,
        output_tokens: row.get(15)?,
        cost_micros: row.get(16)?,
        correlation_id: row.get(17)?,
        created_at: millis(row.get(18)?),
        finished_at: row.get::<_, Option<i64>>(19)?.map(millis),
        outcome: row
            .get::<_, Option<String>>(20)?
            .as_deref()
            .and_then(parse_outcome),
    })
}
