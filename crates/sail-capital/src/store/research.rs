//! Round 19: index membership for the research universe (Claude's slice). Append-only:
//! each fetch stores a snapshot; the latest snapshot per index is current.

use super::{CapitalStore, StoreError, decode, encode, parse_ts};
use crate::contracts::{DecisionTime, InputProvenance, VintagePolicy};
use chrono::{DateTime, NaiveDate, Utc};
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

    /// The newest membership snapshot for `index` known at `decision.at` (or, when
    /// `policy == AllowReconstructed` and none existed by `decision.at`, rebuilt from the
    /// earliest later snapshot). Membership `(from, to)` intervals are clipped to
    /// `decision.session` so future membership changes do not leak into the snapshot.
    pub fn research_members_as_of(
        &self,
        index: &str,
        decision: DecisionTime,
        policy: VintagePolicy,
    ) -> Result<Option<(ResearchSnapshot, InputProvenance)>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT fetched_at, members_json FROM research_members
             WHERE index_name = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![index], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
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
        let clip = |mut members: Vec<crate::universe::ResearchMember>, session: NaiveDate| {
            for m in &mut members {
                m.membership = m
                    .membership
                    .iter()
                    .filter(|(from, _)| *from <= session)
                    .map(|(from, to)| (*from, to.filter(|t| *t <= session)))
                    .collect();
            }
            members.retain(|m| !m.membership.is_empty());
            members
        };
        if let Some((at, json)) = observed {
            let members = clip(decode(&json)?, decision.session);
            return Ok(Some(((at, members), InputProvenance::Observed)));
        }
        if policy == VintagePolicy::AllowReconstructed
            && let Some((at, json)) = later
        {
            let members = clip(decode(&json)?, decision.session);
            return Ok(Some(((at, members), InputProvenance::Reconstructed)));
        }
        Ok(None)
    }
}
