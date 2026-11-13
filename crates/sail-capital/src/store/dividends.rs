//! Stored per-share dividend histories fetched from Yahoo.

use super::CapitalStore;
use super::{StoreError, decode, encode, parse_ts};
use crate::contracts::{DecisionTime, InputProvenance, VintagePolicy};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS dividend_histories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    yahoo TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    currency TEXT,
    history_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS dividend_histories_by_symbol ON dividend_histories (yahoo, id);
CREATE TRIGGER IF NOT EXISTS dividend_histories_no_update BEFORE UPDATE ON dividend_histories
BEGIN SELECT RAISE(ABORT, 'dividend histories are append-only'); END;
CREATE TRIGGER IF NOT EXISTS dividend_histories_no_delete BEFORE DELETE ON dividend_histories
BEGIN SELECT RAISE(ABORT, 'dividend histories are append-only'); END;
";

/// A stored per-share dividend history: when fetched, its currency, and the payments.
pub type DividendHistory = (
    DateTime<Utc>,
    Option<String>,
    Vec<crate::dividends::Payment>,
);

impl CapitalStore {
    /// Changes whenever a payout history is stored.
    pub fn dividend_history_version(&self) -> Result<String, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let last: Option<i64> =
            conn.query_row("SELECT MAX(rowid) FROM dividend_histories", [], |r| {
                r.get(0)
            })?;
        Ok(last.unwrap_or(0).to_string())
    }

    /// Stores a per-share dividend history snapshot for a Yahoo symbol.
    pub fn record_dividend_history(
        &self,
        yahoo: &str,
        currency: Option<&str>,
        history: &[crate::dividends::Payment],
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        conn.execute(
            "INSERT INTO dividend_histories (yahoo, fetched_at, currency, history_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![yahoo, now.to_rfc3339(), currency, encode(&history)?],
        )?;
        Ok(())
    }

    /// The latest per-share dividend history for a Yahoo symbol.
    pub fn dividend_history(&self, yahoo: &str) -> Result<Option<DividendHistory>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let row: Option<(String, Option<String>, String)> = conn
            .query_row(
                "SELECT fetched_at, currency, history_json FROM dividend_histories
                 WHERE yahoo = ?1 ORDER BY id DESC LIMIT 1",
                params![yahoo],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        row.map(|(at, currency, json)| Ok((parse_ts(&at)?, currency, decode(&json)?)))
            .transpose()
    }

    /// The newest dividend/corporate-action snapshot for `yahoo` known at `decision.at`
    /// (or, when `policy == AllowReconstructed` and none existed by `decision.at`, rebuilt
    /// from the earliest later snapshot filtered to `ex_date <= decision.session`).
    pub fn dividend_history_as_of(
        &self,
        yahoo: &str,
        decision: DecisionTime,
        policy: VintagePolicy,
    ) -> Result<Option<(DividendHistory, InputProvenance)>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT fetched_at, currency, history_json FROM dividend_histories
             WHERE yahoo = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![yahoo], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut observed: Option<(DateTime<Utc>, Option<String>, String)> = None;
        let mut later: Option<(DateTime<Utc>, Option<String>, String)> = None;
        for row in rows {
            let (at_str, currency, json) = row?;
            let at = parse_ts(&at_str)?;
            if at <= decision.at {
                if observed.as_ref().is_none_or(|(prev, ..)| at >= *prev) {
                    observed = Some((at, currency, json));
                }
            } else if later.as_ref().is_none_or(|(prev, ..)| at < *prev) {
                later = Some((at, currency, json));
            }
        }
        if let Some((at, currency, json)) = observed {
            let payments: Vec<crate::dividends::Payment> = decode(&json)?;
            return Ok(Some(((at, currency, payments), InputProvenance::Observed)));
        }
        if policy == VintagePolicy::AllowReconstructed
            && let Some((at, currency, json)) = later
        {
            let mut payments: Vec<crate::dividends::Payment> = decode(&json)?;
            payments.retain(|p| p.ex_date <= decision.session);
            return Ok(Some((
                (at, currency, payments),
                InputProvenance::Reconstructed,
            )));
        }
        Ok(None)
    }
}
