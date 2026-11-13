//! Stored signal-board reads ([`crate::board_record`]), one per account, session and
//! instrument, first write wins.

use super::{CapitalStore, StoreError, decode, encode};
use crate::board_record::ReadSnapshot;
use crate::contracts::AccountId;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

/// The board-read table: one row per account, session and instrument, first write wins.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS board_reads (
    account TEXT NOT NULL,
    session TEXT NOT NULL,
    key TEXT NOT NULL,
    payload TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    UNIQUE (account, session, key)
);
";

impl CapitalStore {
    /// Stores snapshots; one already stored for the same account, session and key is kept
    /// as it was. Returns how many were new.
    pub fn record_board_reads(
        &self,
        account: &AccountId,
        reads: &[ReadSnapshot],
        now: DateTime<Utc>,
    ) -> Result<u32, StoreError> {
        let mut conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let tx = conn.transaction()?;
        let mut added = 0u32;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO board_reads
                 (account, session, key, payload, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for read in reads {
                added += stmt.execute(params![
                    account.as_str(),
                    read.session.format("%Y-%m-%d").to_string(),
                    read.key,
                    encode(read)?,
                    now.to_rfc3339()
                ])? as u32;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// Every stored snapshot for the account, oldest session first, then key.
    pub fn board_reads(&self, account: &AccountId) -> Result<Vec<ReadSnapshot>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn
            .prepare("SELECT payload FROM board_reads WHERE account = ?1 ORDER BY session, key")?;
        let rows = stmt.query_map(params![account.as_str()], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(decode(&row?)?);
        }
        Ok(out)
    }

    /// The latest session with a stored snapshot for the account.
    pub fn latest_board_session(
        &self,
        account: &AccountId,
    ) -> Result<Option<chrono::NaiveDate>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let session: Option<String> = conn
            .query_row(
                "SELECT MAX(session) FROM board_reads WHERE account = ?1",
                params![account.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        session
            .map(|session| {
                chrono::NaiveDate::parse_from_str(&session, "%Y-%m-%d")
                    .map_err(|_| StoreError::Decode(format!("bad session {session}")))
            })
            .transpose()
    }
}
