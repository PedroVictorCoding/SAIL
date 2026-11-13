//! Stored per-share dividend histories fetched from Yahoo.

use super::CapitalStore;
use super::{decode, encode, parse_ts, StoreError};
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
}
