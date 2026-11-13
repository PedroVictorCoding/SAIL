//! Risk-policy versions and suggestion actions, plus the tables that hold model signal
//! values, which both policies and signals share.

use super::CapitalStore;
use super::{decode, encode, StoreError};
use crate::contracts::*;
use chrono::{DateTime, Utc};
use rusqlite::params;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS policy_versions (
    account TEXT NOT NULL,
    version INTEGER NOT NULL,
    saved_at TEXT NOT NULL,
    policy_json TEXT NOT NULL,
    PRIMARY KEY (account, version)
);
CREATE TRIGGER IF NOT EXISTS policy_versions_no_update BEFORE UPDATE ON policy_versions
BEGIN SELECT RAISE(ABORT, 'policy versions are append-only'); END;
CREATE TRIGGER IF NOT EXISTS policy_versions_no_delete BEFORE DELETE ON policy_versions
BEGIN SELECT RAISE(ABORT, 'policy versions are append-only'); END;
CREATE TABLE IF NOT EXISTS signal_values (
    signal TEXT NOT NULL,
    model_version TEXT NOT NULL,
    instrument_key TEXT NOT NULL,
    session TEXT NOT NULL,
    value REAL NOT NULL,
    computed_at TEXT NOT NULL,
    PRIMARY KEY (signal, model_version, instrument_key, session)
);
CREATE TRIGGER IF NOT EXISTS signal_values_no_update BEFORE UPDATE ON signal_values
BEGIN SELECT RAISE(ABORT, 'signal values are append-only'); END;
CREATE TRIGGER IF NOT EXISTS signal_values_no_delete BEFORE DELETE ON signal_values
BEGIN SELECT RAISE(ABORT, 'signal values are append-only'); END;
CREATE TABLE IF NOT EXISTS outlook_paths (
    component TEXT NOT NULL,
    model_version TEXT NOT NULL,
    instrument_key TEXT NOT NULL,
    origin TEXT NOT NULL,
    horizon INTEGER NOT NULL,
    path_json TEXT NOT NULL,
    computed_at TEXT NOT NULL,
    PRIMARY KEY (component, model_version, instrument_key, origin, horizon)
);
CREATE TRIGGER IF NOT EXISTS outlook_paths_no_update BEFORE UPDATE ON outlook_paths
BEGIN SELECT RAISE(ABORT, 'outlook paths are append-only'); END;
CREATE TRIGGER IF NOT EXISTS outlook_paths_no_delete BEFORE DELETE ON outlook_paths
BEGIN SELECT RAISE(ABORT, 'outlook paths are append-only'); END;
CREATE TABLE IF NOT EXISTS suggestion_actions (
    rowid_ INTEGER PRIMARY KEY AUTOINCREMENT,
    account TEXT NOT NULL,
    suggestion_id TEXT NOT NULL,
    action TEXT NOT NULL,
    at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS suggestion_actions_by_account ON suggestion_actions (account, suggestion_id);
CREATE TRIGGER IF NOT EXISTS suggestion_actions_no_update BEFORE UPDATE ON suggestion_actions
BEGIN SELECT RAISE(ABORT, 'suggestion actions are append-only'); END;
CREATE TRIGGER IF NOT EXISTS suggestion_actions_no_delete BEFORE DELETE ON suggestion_actions
BEGIN SELECT RAISE(ABORT, 'suggestion actions are append-only'); END;
";

impl CapitalStore {
    /// Stores a new policy version for `account` (the next number) and returns it.
    pub fn save_policy(
        &self,
        account: &AccountId,
        limits: &crate::policy::RiskLimits,
        status: crate::policy::PolicyStatus,
        now: DateTime<Utc>,
    ) -> Result<crate::policy::RiskPolicy, StoreError> {
        let mut conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let tx = conn.transaction()?;
        let last: Option<u32> = tx.query_row(
            "SELECT MAX(version) FROM policy_versions WHERE account = ?1",
            params![account.as_str()],
            |r| r.get(0),
        )?;
        let policy = crate::policy::RiskPolicy {
            version: last.unwrap_or(0) + 1,
            status,
            limits: limits.clone(),
            saved_at: Some(now),
        };
        tx.execute(
            "INSERT INTO policy_versions (account, version, saved_at, policy_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                account.as_str(),
                policy.version,
                now.to_rfc3339(),
                encode(&policy)?
            ],
        )?;
        tx.commit()?;
        Ok(policy)
    }

    /// Every stored policy version for `account`, newest first.
    pub fn policies(
        &self,
        account: &AccountId,
    ) -> Result<Vec<crate::policy::RiskPolicy>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT policy_json FROM policy_versions WHERE account = ?1 ORDER BY version DESC",
        )?;
        let rows = stmt.query_map(params![account.as_str()], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(decode(&row?)?);
        }
        Ok(out)
    }

    /// Records what the owner did with a suggestion.
    pub fn record_suggestion_action(
        &self,
        account: &AccountId,
        record: &crate::suggest::ActionRecord,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        conn.execute(
            "INSERT INTO suggestion_actions (account, suggestion_id, action, at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                account.as_str(),
                record.suggestion_id,
                encode(&record.action)?,
                record.at.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Suggestion ids the owner dismissed or marked done.
    pub fn handled_suggestions(
        &self,
        account: &AccountId,
    ) -> Result<std::collections::BTreeSet<String>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn
            .prepare("SELECT DISTINCT suggestion_id FROM suggestion_actions WHERE account = ?1")?;
        let rows = stmt.query_map(params![account.as_str()], |r| r.get::<_, String>(0))?;
        let mut out = std::collections::BTreeSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    }
}
