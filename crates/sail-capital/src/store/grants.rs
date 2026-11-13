//! Owner-declared grants: units received without buying them. A grant row is never
//! changed except to set `retracted_at` once, and never deleted.

use super::CapitalStore;
use super::{StoreError, decode, encode, parse_ts};
use crate::contracts::*;
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// Grants live apart from imported entries: they come from the owner, not a file. A row
/// is never changed except to set `retracted_at` once, and never deleted.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS grants (
    id TEXT PRIMARY KEY,
    account TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    grant_json TEXT NOT NULL,
    retracted_at TEXT
);
CREATE INDEX IF NOT EXISTS grants_by_account ON grants (account, recorded_at);
CREATE TRIGGER IF NOT EXISTS grants_retract_once BEFORE UPDATE ON grants
WHEN OLD.retracted_at IS NOT NULL
  OR NEW.retracted_at IS NULL
  OR NEW.id IS NOT OLD.id
  OR NEW.account IS NOT OLD.account
  OR NEW.recorded_at IS NOT OLD.recorded_at
  OR NEW.grant_json IS NOT OLD.grant_json
BEGIN SELECT RAISE(ABORT, 'a grant can only be retracted, once'); END;
CREATE TRIGGER IF NOT EXISTS grants_no_delete BEFORE DELETE ON grants
BEGIN SELECT RAISE(ABORT, 'grants are never deleted'); END;
";

/// Units the owner declared they received without buying them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantView {
    /// Stable id.
    pub id: String,
    /// Broker symbol, as XTB writes it (e.g. `"GALP.PT"`).
    pub symbol: String,
    /// Stock or ETF.
    pub kind: crate::contracts::InstrumentKind,
    /// Units received, positive.
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: rust_decimal::Decimal,
    /// When the units arrived.
    pub received_at: DateTime<Utc>,
    /// The cost basis the owner states; `None` is zero.
    pub cost: Option<crate::contracts::Money>,
    /// The owner's note, capped.
    pub note: Option<String>,
    /// When SAIL recorded it. Point-in-time questions see it from here on.
    pub recorded_at: DateTime<Utc>,
    /// When the owner retracted it, if they did.
    pub retracted_at: Option<DateTime<Utc>>,
}

impl CapitalStore {
    /// Records a grant. `grant.id` must be new.
    pub fn record_grant(&self, account: &AccountId, grant: &GrantView) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        conn.execute(
            "INSERT INTO grants (id, account, recorded_at, grant_json, retracted_at)
             VALUES (?1, ?2, ?3, ?4, NULL)",
            params![
                grant.id,
                account.as_str(),
                grant.recorded_at.to_rfc3339(),
                encode(grant)?
            ],
        )?;
        Ok(())
    }

    /// Every grant for `account`, newest first, with its retraction time.
    pub fn grants(&self, account: &AccountId) -> Result<Vec<GrantView>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT grant_json, retracted_at FROM grants WHERE account = ?1
             ORDER BY recorded_at DESC, id",
        )?;
        let rows = stmt.query_map(params![account.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (json, retracted) = row?;
            let mut grant: GrantView = decode(&json)?;
            grant.retracted_at = retracted.as_deref().map(parse_ts).transpose()?;
            out.push(grant);
        }
        Ok(out)
    }

    /// Retracts a grant at `now`. A grant is retracted at most once.
    pub fn retract_grant(
        &self,
        account: &AccountId,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<RetractOutcome, StoreError> {
        let changed = {
            let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
            conn.execute(
                "UPDATE grants SET retracted_at = ?1
                 WHERE account = ?2 AND id = ?3 AND retracted_at IS NULL",
                params![now.to_rfc3339(), account.as_str(), id],
            )?
        };
        let grant = self.grants(account)?.into_iter().find(|g| g.id == id);
        Ok(match (changed, grant) {
            (_, None) => RetractOutcome::NotFound,
            (0, Some(g)) => RetractOutcome::AlreadyRetracted(g),
            (_, Some(g)) => RetractOutcome::Retracted(g),
        })
    }
}

/// What a retraction did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetractOutcome {
    /// Retracted now.
    Retracted(GrantView),
    /// It had been retracted before; nothing changed.
    AlreadyRetracted(GrantView),
    /// No such grant for the account.
    NotFound,
}
