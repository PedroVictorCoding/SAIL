//! The trade universe: watchlist changes, earnings dates, trade runs, published cards
//! and their grades.

use super::CapitalStore;
use super::{StoreError, decode, encode, parse_ts};
use crate::contracts::*;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS watchlist_changes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at TEXT NOT NULL,
    change_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS earnings_dates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    yahoo TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    dates_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS earnings_by_symbol ON earnings_dates (yahoo, fetched_at);
CREATE TABLE IF NOT EXISTS trade_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account TEXT NOT NULL,
    run_at TEXT NOT NULL,
    day_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS trade_cards (
    id TEXT NOT NULL,
    account TEXT NOT NULL,
    published_at TEXT NOT NULL,
    card_json TEXT NOT NULL,
    PRIMARY KEY (account, id)
);
CREATE TABLE IF NOT EXISTS trade_grades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account TEXT NOT NULL,
    card_id TEXT NOT NULL,
    graded_at TEXT NOT NULL,
    status TEXT NOT NULL,
    trade_json TEXT
);
CREATE INDEX IF NOT EXISTS trade_grades_by_card ON trade_grades (account, card_id, graded_at);
CREATE TRIGGER IF NOT EXISTS watchlist_changes_append_only_u BEFORE UPDATE ON watchlist_changes
BEGIN SELECT RAISE(ABORT, 'watchlist changes are append-only'); END;
CREATE TRIGGER IF NOT EXISTS watchlist_changes_append_only_d BEFORE DELETE ON watchlist_changes
BEGIN SELECT RAISE(ABORT, 'watchlist changes are append-only'); END;
CREATE TRIGGER IF NOT EXISTS earnings_append_only_u BEFORE UPDATE ON earnings_dates
BEGIN SELECT RAISE(ABORT, 'earnings snapshots are append-only'); END;
CREATE TRIGGER IF NOT EXISTS trade_runs_append_only_u BEFORE UPDATE ON trade_runs
BEGIN SELECT RAISE(ABORT, 'trade runs are append-only'); END;
CREATE TRIGGER IF NOT EXISTS trade_runs_append_only_d BEFORE DELETE ON trade_runs
BEGIN SELECT RAISE(ABORT, 'trade runs are append-only'); END;
CREATE TRIGGER IF NOT EXISTS trade_cards_append_only_u BEFORE UPDATE ON trade_cards
BEGIN SELECT RAISE(ABORT, 'published signals are never changed'); END;
CREATE TRIGGER IF NOT EXISTS trade_cards_append_only_d BEFORE DELETE ON trade_cards
BEGIN SELECT RAISE(ABORT, 'published signals are never deleted'); END;
CREATE TRIGGER IF NOT EXISTS trade_grades_append_only_u BEFORE UPDATE ON trade_grades
BEGIN SELECT RAISE(ABORT, 'grades are append-only'); END;
CREATE TRIGGER IF NOT EXISTS trade_grades_append_only_d BEFORE DELETE ON trade_grades
BEGIN SELECT RAISE(ABORT, 'grades are append-only'); END;
";

/// An earnings snapshot: when fetched, and the dates.
pub type EarningsSnapshot = (DateTime<Utc>, Vec<chrono::NaiveDate>);

/// A published card with its latest grade.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradedCard {
    /// The card as published.
    pub card: crate::trade::SignalCard,
    /// When it was published.
    pub published_at: DateTime<Utc>,
    /// The trade once closed.
    pub graded: Option<crate::trade::Trade>,
    /// `open`, `closed` or `unfilled`.
    pub status: String,
}

impl CapitalStore {
    /// Records a watchlist change.
    pub fn record_watch_change(
        &self,
        change: &crate::universe::WatchChange,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        conn.execute(
            "INSERT INTO watchlist_changes (at, change_json) VALUES (?1, ?2)",
            params![now.to_rfc3339(), encode(change)?],
        )?;
        Ok(())
    }

    /// Every watchlist change, oldest first.
    pub fn watch_changes(&self) -> Result<Vec<crate::universe::WatchChange>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare("SELECT change_json FROM watchlist_changes ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(decode(&row?)?);
        }
        Ok(out)
    }

    /// Watchlist changes known at `known_by`, oldest first.
    pub fn watch_changes_as_of(
        &self,
        known_by: DateTime<Utc>,
    ) -> Result<Vec<crate::universe::WatchChange>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare("SELECT at, change_json FROM watchlist_changes ORDER BY id")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (at, json) = row?;
            if parse_ts(&at)? <= known_by {
                out.push(decode(&json)?);
            }
        }
        Ok(out)
    }

    /// Stores an earnings snapshot for a Yahoo symbol.
    pub fn record_earnings(
        &self,
        yahoo: &str,
        dates: &[chrono::NaiveDate],
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        conn.execute(
            "INSERT INTO earnings_dates (yahoo, fetched_at, dates_json) VALUES (?1, ?2, ?3)",
            params![yahoo, now.to_rfc3339(), encode(&dates)?],
        )?;
        Ok(())
    }

    /// The latest earnings snapshot for a Yahoo symbol: when fetched, and the dates.
    pub fn earnings(&self, yahoo: &str) -> Result<Option<EarningsSnapshot>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT fetched_at, dates_json FROM earnings_dates WHERE yahoo = ?1
                 ORDER BY id DESC LIMIT 1",
                params![yahoo],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(at, json)| Ok((parse_ts(&at)?, decode(&json)?)))
            .transpose()
    }

    /// The newest earnings snapshot for `yahoo` known at `decision.at`, or (when `policy`
    /// allows) reconstructed from the earliest later snapshot with dates `<= decision.session`.
    pub fn earnings_as_of(
        &self,
        yahoo: &str,
        decision: DecisionTime,
        policy: VintagePolicy,
    ) -> Result<Option<(EarningsSnapshot, InputProvenance)>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT fetched_at, dates_json FROM earnings_dates WHERE yahoo = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![yahoo], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut observed: Option<(DateTime<Utc>, String)> = None;
        let mut later: Option<(DateTime<Utc>, String)> = None;
        for row in rows {
            let (at_str, json) = row?;
            let at = parse_ts(&at_str)?;
            if at <= decision.at {
                if observed.as_ref().is_none_or(|(prev, _)| at >= *prev) {
                    observed = Some((at, json));
                }
            } else if later.as_ref().is_none_or(|(prev, _)| at < *prev) {
                later = Some((at, json));
            }
        }
        if let Some((at, json)) = observed {
            return Ok(Some(((at, decode(&json)?), InputProvenance::Observed)));
        }
        if policy == VintagePolicy::AllowReconstructed
            && let Some((at, json)) = later
        {
            let mut dates: Vec<chrono::NaiveDate> = decode(&json)?;
            dates.retain(|d| *d <= decision.session);
            return Ok(Some(((at, dates), InputProvenance::Reconstructed)));
        }
        Ok(None)
    }

    /// Stores a run's result and publishes its cards and candidates (each card id once per
    /// account).
    pub fn record_trade_run(
        &self,
        account: &AccountId,
        day: &crate::trade::SignalDay,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO trade_runs (account, run_at, day_json) VALUES (?1, ?2, ?3)",
            params![account.as_str(), now.to_rfc3339(), encode(day)?],
        )?;
        for card in day.cards.iter().chain(&day.candidates) {
            tx.execute(
                "INSERT OR IGNORE INTO trade_cards (id, account, published_at, card_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![card.id, account.as_str(), now.to_rfc3339(), encode(card)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The latest run: when, and its result.
    pub fn latest_trade_run(
        &self,
        account: &AccountId,
    ) -> Result<Option<(DateTime<Utc>, crate::trade::SignalDay)>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT run_at, day_json FROM trade_runs WHERE account = ?1
                 ORDER BY id DESC LIMIT 1",
                params![account.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(at, json)| Ok((parse_ts(&at)?, decode(&json)?)))
            .transpose()
    }

    /// Records a grade (`open`, `closed` or `unfilled`) for a published card.
    pub fn record_trade_grade(
        &self,
        account: &AccountId,
        card_id: &str,
        status: &str,
        trade: Option<&crate::trade::Trade>,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let json = trade.map(encode).transpose()?;
        conn.execute(
            "INSERT INTO trade_grades (account, card_id, graded_at, status, trade_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![account.as_str(), card_id, now.to_rfc3339(), status, json],
        )?;
        Ok(())
    }

    /// Every published card with its latest grade, newest first.
    pub fn trade_record(&self, account: &AccountId) -> Result<Vec<GradedCard>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT c.card_json, c.published_at,
                    (SELECT g.status FROM trade_grades g WHERE g.account = c.account
                       AND g.card_id = c.id ORDER BY g.id DESC LIMIT 1),
                    (SELECT g.trade_json FROM trade_grades g WHERE g.account = c.account
                       AND g.card_id = c.id ORDER BY g.id DESC LIMIT 1)
             FROM trade_cards c WHERE c.account = ?1 ORDER BY c.published_at DESC, c.id",
        )?;
        let rows = stmt.query_map(params![account.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (card, at, status, trade) = row?;
            out.push(GradedCard {
                card: decode(&card)?,
                published_at: parse_ts(&at)?,
                graded: trade.as_deref().map(decode).transpose()?,
                status: status.unwrap_or_else(|| "open".into()),
            });
        }
        Ok(out)
    }
}
