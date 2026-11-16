//! Round 25: monthly snapshots of the dividend projection, one per account and month,
//! append-only ([`crate::dividends::record`]).

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::params;

use super::CapitalStore;
use super::{StoreError, decode, encode};
use crate::contracts::AccountId;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS income_snapshots (
    account TEXT NOT NULL,
    month TEXT NOT NULL,
    made_at TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    PRIMARY KEY (account, month)
);
CREATE TRIGGER IF NOT EXISTS income_snapshots_no_update BEFORE UPDATE ON income_snapshots
BEGIN SELECT RAISE(ABORT, 'income snapshots are append-only'); END;
CREATE TRIGGER IF NOT EXISTS income_snapshots_no_delete BEFORE DELETE ON income_snapshots
BEGIN SELECT RAISE(ABORT, 'income snapshots are append-only'); END;
";

impl CapitalStore {
    /// Stores `snapshot` for `account` unless that month already has one. Returns whether
    /// it was new.
    pub fn record_income_snapshot(
        &self,
        account: &AccountId,
        snapshot: &crate::dividends::record::IncomeSnapshot,
    ) -> Result<bool, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let added = conn.execute(
            "INSERT OR IGNORE INTO income_snapshots (account, month, made_at, snapshot_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                account.as_str(),
                snapshot.month.format("%Y-%m-%d").to_string(),
                snapshot.made_at.to_rfc3339(),
                encode(snapshot)?
            ],
        )?;
        Ok(added > 0)
    }

    /// Whether `account` has a snapshot for `month`.
    pub fn has_income_snapshot(
        &self,
        account: &AccountId,
        month: NaiveDate,
    ) -> Result<bool, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM income_snapshots WHERE account = ?1 AND month = ?2",
            params![account.as_str(), month.format("%Y-%m-%d").to_string()],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Every snapshot of `account` made by `known_by`, oldest month first.
    pub fn income_snapshots(
        &self,
        account: &AccountId,
        known_by: DateTime<Utc>,
    ) -> Result<Vec<crate::dividends::record::IncomeSnapshot>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT snapshot_json FROM income_snapshots
             WHERE account = ?1 AND made_at <= ?2 ORDER BY month",
        )?;
        let rows = stmt.query_map(params![account.as_str(), known_by.to_rfc3339()], |r| {
            r.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(decode(&row?)?);
        }
        Ok(out)
    }
}
