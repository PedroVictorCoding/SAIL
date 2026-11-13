//! Round 19: index membership for the research universe (Claude's slice). Append-only:
//! each fetch stores a snapshot; the latest snapshot per index is current.

use super::{CapitalStore, StoreError, decode, encode, parse_ts};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

/// The membership snapshot table. Claude, round 19.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS research_members (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    index_name TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    members_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS research_members_by_index ON research_members (index_name, id);
CREATE TRIGGER IF NOT EXISTS research_members_append_only_u BEFORE UPDATE ON research_members
BEGIN SELECT RAISE(ABORT, 'research membership is append-only'); END;
CREATE TRIGGER IF NOT EXISTS research_members_append_only_d BEFORE DELETE ON research_members
BEGIN SELECT RAISE(ABORT, 'research membership is append-only'); END;
";

/// A stored membership snapshot: when it was fetched, and the members.
pub type ResearchSnapshot = (DateTime<Utc>, Vec<crate::universe::ResearchMember>);

impl CapitalStore {
    /// Stores one fetched membership snapshot for `index`. Claude, round 19.
    pub fn record_research_members(
        &self,
        index: &str,
        members: &[crate::universe::ResearchMember],
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        conn.execute(
            "INSERT INTO research_members (index_name, fetched_at, members_json)
             VALUES (?1, ?2, ?3)",
            params![index, now.to_rfc3339(), encode(&members)?],
        )?;
        Ok(())
    }

    /// The latest snapshot for `index`, with when it was fetched. Claude, round 19.
    pub fn research_members(&self, index: &str) -> Result<Option<ResearchSnapshot>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT fetched_at, members_json FROM research_members
                 WHERE index_name = ?1 ORDER BY id DESC LIMIT 1",
                params![index],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(at, json)| Ok((parse_ts(&at)?, decode(&json)?)))
            .transpose()
    }
}
