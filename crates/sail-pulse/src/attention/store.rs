//! Durable attention inbox (Antigravity). Spec: the module doc in `attention/mod.rs`.
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, NaiveTime, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use super::{
    Assessment, AttentionContext, AttentionItem, AttentionSettings, AttentionStats, Delivery,
    DigestBatch, DropReason, Feedback, ItemState, MAX_DIGEST_ITEMS, Preference, Provenance, Signal,
    Source, normalize, score,
};
use crate::{PulseError, Result};

pub struct AttentionStore {
    conn: Mutex<Connection>,
}

impl AttentionStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| PulseError::Storage(format!("create {}: {e}", parent.display())))?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS attention_items (
                 id TEXT PRIMARY KEY,
                 source TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 dedupe_key TEXT NOT NULL,
                 title TEXT NOT NULL,
                 body TEXT NOT NULL,
                 urgency REAL NOT NULL,
                 relevance REAL NOT NULL,
                 novelty REAL NOT NULL,
                 confidence REAL NOT NULL,
                 deadline TEXT,
                 provenance TEXT NOT NULL,
                 link TEXT,
                 observed_at TEXT NOT NULL,
                 score REAL NOT NULL,
                 interruption_cost REAL NOT NULL,
                 delivery TEXT NOT NULL,
                 drop_reason TEXT,
                 reasons_json TEXT NOT NULL,
                 state TEXT NOT NULL,
                 snoozed_until TEXT,
                 delivered_at TEXT,
                 digested_at TEXT,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 last_feedback_json TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_attention_dedupe
                 ON attention_items(dedupe_key, created_at);
             CREATE INDEX IF NOT EXISTS idx_attention_state_delivery
                 ON attention_items(state, delivery, created_at);
             CREATE INDEX IF NOT EXISTS idx_attention_snoozed
                 ON attention_items(state, snoozed_until);
             CREATE TABLE IF NOT EXISTS attention_preferences (
                 source TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 accepted REAL NOT NULL,
                 rejected REAL NOT NULL,
                 muted INTEGER NOT NULL,
                 updated_at TEXT NOT NULL,
                 PRIMARY KEY (source, kind)
             );
             CREATE TABLE IF NOT EXISTS attention_settings (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 settings_json TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS attention_feedback (
                 id TEXT PRIMARY KEY,
                 item_id TEXT NOT NULL,
                 feedback_kind TEXT NOT NULL,
                 feedback_json TEXT NOT NULL,
                 created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_attention_feedback_item
                 ON attention_feedback(item_id);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| PulseError::Storage("attention store lock poisoned".into()))
    }

    pub fn settings(&self) -> Result<AttentionSettings> {
        let conn = self.lock()?;
        load_settings(&conn)
    }

    pub fn save_settings(&self, settings: &AttentionSettings) -> Result<()> {
        let conn = self.lock()?;
        let json = serde_json::to_string(settings)
            .map_err(|e| PulseError::Storage(format!("serialize settings: {e}")))?;
        let now = ts(Utc::now());
        conn.execute(
            "INSERT INTO attention_settings (id, settings_json, updated_at)
             VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET
                 settings_json = excluded.settings_json,
                 updated_at = excluded.updated_at",
            params![json, now],
        )?;
        Ok(())
    }

    /// Normalize, assess (with the stored preference and cooldown history) and store.
    /// `context.interrupts_last_hour` / `suggestions_today` are overwritten from the store's
    /// own counts. A `Drop` is stored too (state `Dismissed`) so it is observable.
    pub fn ingest(&self, signal: Signal, mut context: AttentionContext) -> Result<AttentionItem> {
        let signal = normalize(signal)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;

        let settings = load_settings(&tx)?;
        context.focus_until = match (context.focus_until, settings.focus_until) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        context.interrupts_last_hour = count_interrupts_last_hour(&tx, context.now)?;
        context.suggestions_today = count_suggestions_today(&tx, context.now)?;

        let recently_delivered =
            is_within_cooldown(&tx, &signal.dedupe_key, settings.cooldown_secs, context.now)?;
        let preference = load_preference(&tx, signal.source, &signal.kind)?;

        let assessment = score::assess(
            &signal,
            &context,
            &settings,
            preference.as_ref(),
            recently_delivered,
        );
        let state = if matches!(assessment.delivery, Delivery::Drop(_)) {
            ItemState::Dismissed
        } else {
            ItemState::Pending
        };

        let item = AttentionItem {
            id: Uuid::new_v4().to_string(),
            signal,
            assessment,
            state,
            snoozed_until: None,
            created_at: context.now,
            updated_at: context.now,
            last_feedback: None,
        };

        insert_item(&tx, &item)?;
        tx.commit()?;
        Ok(item)
    }

    /// `Interrupt` items still `Pending`, plus `Snoozed` items whose time has come; marks
    /// them `Delivered`. Oldest first.
    pub fn deliverable(&self, now: DateTime<Utc>) -> Result<Vec<AttentionItem>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now_str = ts(now);

        let mut items = {
            let mut stmt = tx.prepare(
                "SELECT id, source, kind, dedupe_key, title, body, urgency, relevance, novelty,
                        confidence, deadline, provenance, link, observed_at, score,
                        interruption_cost, delivery, drop_reason, reasons_json, state,
                        snoozed_until, created_at, updated_at, last_feedback_json
                 FROM attention_items
                 WHERE (delivery = 'interrupt' AND state = 'pending')
                    OR (state = 'snoozed' AND snoozed_until IS NOT NULL AND snoozed_until <= ?1)
                 ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map(params![now_str], row_to_item)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };

        for item in &mut items {
            tx.execute(
                "UPDATE attention_items
                 SET state = 'delivered',
                     delivered_at = ?1,
                     updated_at = ?1,
                     snoozed_until = NULL
                 WHERE id = ?2",
                params![now_str, item.id],
            )?;
            item.state = ItemState::Delivered;
            item.snoozed_until = None;
            item.updated_at = now;
        }

        tx.commit()?;
        Ok(items)
    }

    /// The items [`deliverable`](Self::deliverable) would return, without marking them.
    /// Presence routing peeks, sends each item to one place, and then calls
    /// [`mark_delivered`](Self::mark_delivered) only for items that actually went
    /// somewhere, so an item held for the owner's return is not lost. Oldest first.
    pub fn pending_interrupts(&self, now: DateTime<Utc>) -> Result<Vec<AttentionItem>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, source, kind, dedupe_key, title, body, urgency, relevance, novelty,
                    confidence, deadline, provenance, link, observed_at, score,
                    interruption_cost, delivery, drop_reason, reasons_json, state,
                    snoozed_until, created_at, updated_at, last_feedback_json
             FROM attention_items
             WHERE (delivery = 'interrupt' AND state = 'pending')
                OR (state = 'snoozed' AND snoozed_until IS NOT NULL AND snoozed_until <= ?1)
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![ts(now)], row_to_item)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Collect pending `Digest` items into one batch (marks them `Digested`). `None` when empty.
    pub fn release_digest(&self, now: DateTime<Utc>) -> Result<Option<DigestBatch>> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let now_str = ts(now);

        let mut items = {
            let mut stmt = tx.prepare(
                "SELECT id, source, kind, dedupe_key, title, body, urgency, relevance, novelty,
                        confidence, deadline, provenance, link, observed_at, score,
                        interruption_cost, delivery, drop_reason, reasons_json, state,
                        snoozed_until, created_at, updated_at, last_feedback_json
                 FROM attention_items
                 WHERE delivery = 'digest' AND state = 'pending'
                 ORDER BY created_at ASC
                 LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![MAX_DIGEST_ITEMS as i64], row_to_item)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };

        if items.is_empty() {
            return Ok(None);
        }

        for item in &mut items {
            tx.execute(
                "UPDATE attention_items
                 SET state = 'digested',
                     digested_at = ?1,
                     updated_at = ?1
                 WHERE id = ?2",
                params![now_str, item.id],
            )?;
            item.state = ItemState::Digested;
            item.updated_at = now;
        }

        tx.commit()?;
        Ok(Some(DigestBatch {
            released_at: now,
            items,
        }))
    }

    /// Top pending `Suggest` items by score, not marked (the caller marks the one it raises).
    pub fn suggestions(&self, limit: usize) -> Result<Vec<AttentionItem>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, source, kind, dedupe_key, title, body, urgency, relevance, novelty,
                    confidence, deadline, provenance, link, observed_at, score,
                    interruption_cost, delivery, drop_reason, reasons_json, state,
                    snoozed_until, created_at, updated_at, last_feedback_json
             FROM attention_items
             WHERE delivery = 'suggest' AND state = 'pending'
             ORDER BY score DESC, created_at ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], row_to_item)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn mark_delivered(&self, id: &str, now: DateTime<Utc>) -> Result<()> {
        let conn = self.lock()?;
        let now_str = ts(now);
        let changed = conn.execute(
            "UPDATE attention_items
             SET state = 'delivered',
                 delivered_at = ?1,
                 updated_at = ?1
             WHERE id = ?2",
            params![now_str, id],
        )?;
        if changed == 0 {
            return Err(PulseError::NotFound(id.to_string()));
        }
        Ok(())
    }

    /// Record feedback, update the learned preference and re-schedule the item.
    pub fn feedback(
        &self,
        id: &str,
        feedback: Feedback,
        now: DateTime<Utc>,
    ) -> Result<AttentionItem> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut item = fetch_item(&tx, id)?;

        let fb_id = Uuid::new_v4().to_string();
        let fb_kind = feedback_kind(feedback);
        let fb_json = serde_json::to_string(&feedback)
            .map_err(|e| PulseError::Storage(format!("serialize feedback: {e}")))?;
        let now_str = ts(now);

        tx.execute(
            "INSERT INTO attention_feedback (id, item_id, feedback_kind, feedback_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![fb_id, id, fb_kind, fb_json, now_str],
        )?;

        let existing_pref =
            load_preference(&tx, item.signal.source, &item.signal.kind)?.unwrap_or(Preference {
                source: item.signal.source,
                kind: item.signal.kind.clone(),
                accepted: 0.0,
                rejected: 0.0,
                muted: false,
                updated_at: now,
            });
        let updated_pref = score::learn(existing_pref, feedback, now);
        upsert_preference(&tx, &updated_pref)?;

        match feedback {
            Feedback::Useful => {
                item.state = ItemState::Done;
                item.snoozed_until = None;
            }
            Feedback::NotNow => {
                item.state = ItemState::Pending;
                item.assessment.delivery = Delivery::Digest;
                item.snoozed_until = None;
            }
            Feedback::Later(until) => {
                item.state = ItemState::Snoozed;
                item.snoozed_until = Some(until);
            }
            Feedback::Investigate => {
                item.state = ItemState::Investigating;
                item.snoozed_until = None;
            }
            Feedback::Dismiss | Feedback::Mute => {
                item.state = ItemState::Dismissed;
                item.snoozed_until = None;
            }
        }
        item.last_feedback = Some(feedback);
        item.updated_at = now;

        let (delivery_str, drop_reason_str) = delivery_parts(item.assessment.delivery);
        tx.execute(
            "UPDATE attention_items
             SET delivery = ?1,
                 drop_reason = ?2,
                 state = ?3,
                 snoozed_until = ?4,
                 updated_at = ?5,
                 last_feedback_json = ?6
             WHERE id = ?7",
            params![
                delivery_str,
                drop_reason_str,
                state_as_str(item.state),
                item.snoozed_until.map(ts),
                now_str,
                fb_json,
                item.id
            ],
        )?;

        tx.commit()?;
        Ok(item)
    }

    pub fn get(&self, id: &str) -> Result<AttentionItem> {
        let conn = self.lock()?;
        fetch_item(&conn, id)
    }

    /// Items not `Dismissed`/`Done`, newest first.
    pub fn inbox(&self, limit: usize) -> Result<Vec<AttentionItem>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, source, kind, dedupe_key, title, body, urgency, relevance, novelty,
                    confidence, deadline, provenance, link, observed_at, score,
                    interruption_cost, delivery, drop_reason, reasons_json, state,
                    snoozed_until, created_at, updated_at, last_feedback_json
             FROM attention_items
             WHERE state NOT IN ('dismissed', 'done')
             ORDER BY created_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], row_to_item)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn preferences(&self) -> Result<Vec<Preference>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT source, kind, accepted, rejected, muted, updated_at
             FROM attention_preferences
             ORDER BY source ASC, kind ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            let source_str: String = row.get(0)?;
            let kind: String = row.get(1)?;
            let accepted: f32 = row.get(2)?;
            let rejected: f32 = row.get(3)?;
            let muted: i64 = row.get(4)?;
            let updated_at_str: String = row.get(5)?;
            Ok((
                source_str,
                kind,
                accepted,
                rejected,
                muted != 0,
                updated_at_str,
            ))
        })?;
        let mut prefs = Vec::new();
        for row in rows {
            let (source_str, kind, accepted, rejected, muted, updated_at_str) = row?;
            prefs.push(Preference {
                source: parse_source(&source_str)?,
                kind,
                accepted,
                rejected,
                muted,
                updated_at: parse_ts(&updated_at_str)?,
            });
        }
        Ok(prefs)
    }

    pub fn unmute(&self, source: Source, kind: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE attention_preferences
             SET muted = 0
             WHERE source = ?1 AND kind = ?2",
            params![source.as_str(), kind],
        )?;
        Ok(())
    }

    pub fn stats(&self, now: DateTime<Utc>) -> Result<AttentionStats> {
        let conn = self.lock()?;
        let by_delivery = count_grouped(
            &conn,
            "SELECT delivery, COUNT(*) FROM attention_items GROUP BY delivery ORDER BY delivery",
        )?;
        let by_state = count_grouped(
            &conn,
            "SELECT state, COUNT(*) FROM attention_items GROUP BY state ORDER BY state",
        )?;
        let feedback = count_grouped(
            &conn,
            "SELECT feedback_kind, COUNT(*) FROM attention_feedback GROUP BY feedback_kind ORDER BY feedback_kind",
        )?;
        let interrupts_last_hour = count_interrupts_last_hour(&conn, now)?;
        let suggestions_today = count_suggestions_today(&conn, now)?;

        Ok(AttentionStats {
            by_delivery,
            by_state,
            feedback,
            interrupts_last_hour,
            suggestions_today,
        })
    }

    /// Delete `Dismissed`/`Done`/`Digested` items older than `keep_days`.
    pub fn prune(&self, keep_days: u32, now: DateTime<Utc>) -> Result<usize> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let cutoff = ts(now - chrono::Duration::days(i64::from(keep_days)));
        let deleted = tx.execute(
            "DELETE FROM attention_items
             WHERE state IN ('dismissed', 'done', 'digested')
               AND updated_at < ?1",
            params![cutoff],
        )?;
        tx.execute(
            "DELETE FROM attention_feedback
             WHERE item_id NOT IN (SELECT id FROM attention_items)",
            [],
        )?;
        tx.commit()?;
        Ok(deleted)
    }
}

fn ts(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn parse_ts(raw: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(raw)?.with_timezone(&Utc))
}

fn load_settings(conn: &Connection) -> Result<AttentionSettings> {
    let json: Option<String> = conn
        .query_row(
            "SELECT settings_json FROM attention_settings WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    match json {
        Some(raw) => serde_json::from_str(&raw)
            .map_err(|e| PulseError::Storage(format!("corrupt attention settings: {e}"))),
        None => Ok(AttentionSettings::default()),
    }
}

fn load_preference(conn: &Connection, source: Source, kind: &str) -> Result<Option<Preference>> {
    let row = conn
        .query_row(
            "SELECT accepted, rejected, muted, updated_at
             FROM attention_preferences
             WHERE source = ?1 AND kind = ?2",
            params![source.as_str(), kind],
            |row| {
                let accepted: f32 = row.get(0)?;
                let rejected: f32 = row.get(1)?;
                let muted: i64 = row.get(2)?;
                let updated_at: String = row.get(3)?;
                Ok((accepted, rejected, muted != 0, updated_at))
            },
        )
        .optional()?;
    match row {
        Some((accepted, rejected, muted, updated_at)) => Ok(Some(Preference {
            source,
            kind: kind.to_string(),
            accepted,
            rejected,
            muted,
            updated_at: parse_ts(&updated_at)?,
        })),
        None => Ok(None),
    }
}

fn upsert_preference(conn: &Connection, pref: &Preference) -> Result<()> {
    conn.execute(
        "INSERT INTO attention_preferences (source, kind, accepted, rejected, muted, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(source, kind) DO UPDATE SET
             accepted = excluded.accepted,
             rejected = excluded.rejected,
             muted = excluded.muted,
             updated_at = excluded.updated_at",
        params![
            pref.source.as_str(),
            pref.kind,
            pref.accepted,
            pref.rejected,
            i64::from(pref.muted),
            ts(pref.updated_at),
        ],
    )?;
    Ok(())
}

fn is_within_cooldown(
    conn: &Connection,
    dedupe_key: &str,
    cooldown_secs: u64,
    now: DateTime<Utc>,
) -> Result<bool> {
    if cooldown_secs == 0 {
        return Ok(false);
    }
    let secs = i64::try_from(cooldown_secs).unwrap_or(i64::MAX);
    let cutoff = ts(now - chrono::Duration::seconds(secs));
    let count: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM attention_items
         WHERE dedupe_key = ?1
           AND delivery != 'drop'
           AND COALESCE(delivered_at, digested_at, created_at) >= ?2",
        params![dedupe_key, cutoff],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn count_interrupts_last_hour(conn: &Connection, now: DateTime<Utc>) -> Result<u32> {
    let cutoff = ts(now - chrono::Duration::hours(1));
    let count: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM attention_items
         WHERE delivery = 'interrupt'
           AND (
               (delivered_at IS NOT NULL AND delivered_at >= ?1)
               OR (state = 'pending' AND created_at >= ?1)
           )",
        params![cutoff],
        |row| row.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

fn count_suggestions_today(conn: &Connection, now: DateTime<Utc>) -> Result<u32> {
    let start_of_day = now.date_naive().and_time(NaiveTime::MIN).and_utc();
    let cutoff = ts(start_of_day);
    let count: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM attention_items
         WHERE delivery = 'suggest'
           AND state != 'dismissed'
           AND (
               (delivered_at IS NOT NULL AND delivered_at >= ?1)
               OR (state = 'pending' AND created_at >= ?1)
           )",
        params![cutoff],
        |row| row.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

fn count_grouped(conn: &Connection, sql: &str) -> Result<Vec<(String, u64)>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        let key: String = row.get(0)?;
        let count: i64 = row.get(1)?;
        Ok((key, u64::try_from(count).unwrap_or(0)))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn insert_item(conn: &Connection, item: &AttentionItem) -> Result<()> {
    let (delivery_str, drop_reason_str) = delivery_parts(item.assessment.delivery);
    let reasons_json = serde_json::to_string(&item.assessment.reasons)
        .map_err(|e| PulseError::Storage(format!("serialize reasons: {e}")))?;
    let last_feedback_json = item
        .last_feedback
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| PulseError::Storage(format!("serialize feedback: {e}")))?;

    conn.execute(
        "INSERT INTO attention_items (
             id, source, kind, dedupe_key, title, body, urgency, relevance, novelty,
             confidence, deadline, provenance, link, observed_at, score, interruption_cost,
             delivery, drop_reason, reasons_json, state, snoozed_until, delivered_at,
             digested_at, created_at, updated_at, last_feedback_json
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
             ?10, ?11, ?12, ?13, ?14, ?15, ?16,
             ?17, ?18, ?19, ?20, ?21, NULL,
             NULL, ?22, ?23, ?24
         )",
        params![
            item.id,
            item.signal.source.as_str(),
            item.signal.kind,
            item.signal.dedupe_key,
            item.signal.title,
            item.signal.body,
            item.signal.urgency,
            item.signal.relevance,
            item.signal.novelty,
            item.signal.confidence,
            item.signal.deadline.map(ts),
            provenance_as_str(item.signal.provenance),
            item.signal.link,
            ts(item.signal.observed_at),
            item.assessment.score,
            item.assessment.interruption_cost,
            delivery_str,
            drop_reason_str,
            reasons_json,
            state_as_str(item.state),
            item.snoozed_until.map(ts),
            ts(item.created_at),
            ts(item.updated_at),
            last_feedback_json,
        ],
    )?;
    Ok(())
}

fn fetch_item(conn: &Connection, id: &str) -> Result<AttentionItem> {
    let item = conn
        .query_row(
            "SELECT id, source, kind, dedupe_key, title, body, urgency, relevance, novelty,
                    confidence, deadline, provenance, link, observed_at, score,
                    interruption_cost, delivery, drop_reason, reasons_json, state,
                    snoozed_until, created_at, updated_at, last_feedback_json
             FROM attention_items
             WHERE id = ?1",
            params![id],
            row_to_item,
        )
        .optional()?;
    item.ok_or_else(|| PulseError::NotFound(id.to_string()))
}

fn row_to_item(row: &Row<'_>) -> rusqlite::Result<AttentionItem> {
    let id: String = row.get(0)?;
    let source_str: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let dedupe_key: String = row.get(3)?;
    let title: String = row.get(4)?;
    let body: String = row.get(5)?;
    let urgency: f32 = row.get(6)?;
    let relevance: f32 = row.get(7)?;
    let novelty: f32 = row.get(8)?;
    let confidence: f32 = row.get(9)?;
    let deadline_raw: Option<String> = row.get(10)?;
    let provenance_str: String = row.get(11)?;
    let link: Option<String> = row.get(12)?;
    let observed_at_raw: String = row.get(13)?;
    let score_val: f32 = row.get(14)?;
    let interruption_cost: f32 = row.get(15)?;
    let delivery_str: String = row.get(16)?;
    let drop_reason_str: Option<String> = row.get(17)?;
    let reasons_json: String = row.get(18)?;
    let state_str: String = row.get(19)?;
    let snoozed_until_raw: Option<String> = row.get(20)?;
    let created_at_raw: String = row.get(21)?;
    let updated_at_raw: String = row.get(22)?;
    let last_feedback_json: Option<String> = row.get(23)?;

    let to_sql_err = |e: PulseError| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    };

    let source = parse_source(&source_str).map_err(to_sql_err)?;
    let provenance = parse_provenance(&provenance_str).map_err(to_sql_err)?;
    let deadline = deadline_raw
        .as_deref()
        .map(parse_ts)
        .transpose()
        .map_err(to_sql_err)?;
    let observed_at = parse_ts(&observed_at_raw).map_err(to_sql_err)?;
    let delivery = parse_delivery(&delivery_str, drop_reason_str.as_deref()).map_err(to_sql_err)?;
    let reasons: Vec<String> = serde_json::from_str(&reasons_json)
        .map_err(|e| to_sql_err(PulseError::Storage(format!("corrupt reasons_json: {e}"))))?;
    let state = parse_state(&state_str).map_err(to_sql_err)?;
    let snoozed_until = snoozed_until_raw
        .as_deref()
        .map(parse_ts)
        .transpose()
        .map_err(to_sql_err)?;
    let created_at = parse_ts(&created_at_raw).map_err(to_sql_err)?;
    let updated_at = parse_ts(&updated_at_raw).map_err(to_sql_err)?;
    let last_feedback: Option<Feedback> = last_feedback_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| to_sql_err(PulseError::Storage(format!("corrupt last_feedback: {e}"))))?;

    Ok(AttentionItem {
        id,
        signal: Signal {
            source,
            kind,
            dedupe_key,
            title,
            body,
            urgency,
            relevance,
            novelty,
            confidence,
            deadline,
            provenance,
            link,
            observed_at,
        },
        assessment: Assessment {
            score: score_val,
            interruption_cost,
            delivery,
            reasons,
        },
        state,
        snoozed_until,
        created_at,
        updated_at,
        last_feedback,
    })
}

fn parse_source(raw: &str) -> Result<Source> {
    Ok(match raw {
        "moodle" => Source::Moodle,
        "calendar" => Source::Calendar,
        "reminders" => Source::Reminders,
        "github" => Source::GitHub,
        "research" => Source::Research,
        "capital" => Source::Capital,
        "system" => Source::System,
        "escalation" => Source::Escalation,
        "curiosity" => Source::Curiosity,
        "projects" => Source::Projects,
        other => return Err(PulseError::Storage(format!("unknown source {other}"))),
    })
}

fn provenance_as_str(p: Provenance) -> &'static str {
    match p {
        Provenance::UserCommitment => "user_commitment",
        Provenance::Observation => "observation",
        Provenance::Suggestion => "suggestion",
        Provenance::Hypothesis => "hypothesis",
    }
}

fn parse_provenance(raw: &str) -> Result<Provenance> {
    Ok(match raw {
        "user_commitment" => Provenance::UserCommitment,
        "observation" => Provenance::Observation,
        "suggestion" => Provenance::Suggestion,
        "hypothesis" => Provenance::Hypothesis,
        other => return Err(PulseError::Storage(format!("unknown provenance {other}"))),
    })
}

fn delivery_parts(delivery: Delivery) -> (&'static str, Option<&'static str>) {
    match delivery {
        Delivery::Interrupt => ("interrupt", None),
        Delivery::Suggest => ("suggest", None),
        Delivery::Digest => ("digest", None),
        Delivery::Silent => ("silent", None),
        Delivery::Drop(reason) => ("drop", Some(drop_reason_as_str(reason))),
    }
}

fn drop_reason_as_str(reason: DropReason) -> &'static str {
    match reason {
        DropReason::Duplicate => "duplicate",
        DropReason::Muted => "muted",
        DropReason::NotNovel => "not_novel",
        DropReason::Stale => "stale",
        DropReason::AutonomyOff => "autonomy_off",
    }
}

fn parse_delivery(delivery: &str, drop_reason: Option<&str>) -> Result<Delivery> {
    Ok(match delivery {
        "interrupt" => Delivery::Interrupt,
        "suggest" => Delivery::Suggest,
        "digest" => Delivery::Digest,
        "silent" => Delivery::Silent,
        "drop" => {
            let reason = match drop_reason {
                Some("duplicate") => DropReason::Duplicate,
                Some("muted") => DropReason::Muted,
                Some("not_novel") => DropReason::NotNovel,
                Some("stale") => DropReason::Stale,
                Some("autonomy_off") => DropReason::AutonomyOff,
                other => {
                    return Err(PulseError::Storage(format!(
                        "unknown drop reason {other:?}"
                    )));
                }
            };
            Delivery::Drop(reason)
        }
        other => return Err(PulseError::Storage(format!("unknown delivery {other}"))),
    })
}

fn state_as_str(state: ItemState) -> &'static str {
    match state {
        ItemState::Pending => "pending",
        ItemState::Delivered => "delivered",
        ItemState::Digested => "digested",
        ItemState::Snoozed => "snoozed",
        ItemState::Investigating => "investigating",
        ItemState::Dismissed => "dismissed",
        ItemState::Done => "done",
    }
}

fn parse_state(raw: &str) -> Result<ItemState> {
    Ok(match raw {
        "pending" => ItemState::Pending,
        "delivered" => ItemState::Delivered,
        "digested" => ItemState::Digested,
        "snoozed" => ItemState::Snoozed,
        "investigating" => ItemState::Investigating,
        "dismissed" => ItemState::Dismissed,
        "done" => ItemState::Done,
        other => return Err(PulseError::Storage(format!("unknown item state {other}"))),
    })
}

fn feedback_kind(feedback: Feedback) -> &'static str {
    match feedback {
        Feedback::Useful => "useful",
        Feedback::NotNow => "not_now",
        Feedback::Later(_) => "later",
        Feedback::Investigate => "investigate",
        Feedback::Dismiss => "dismiss",
        Feedback::Mute => "mute",
    }
}
