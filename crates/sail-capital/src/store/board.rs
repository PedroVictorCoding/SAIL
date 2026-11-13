//! Stored signal-board reads ([`crate::board_record`]), one per account, session and
//! instrument, first write wins.

use super::CapitalStore;
use super::StoreError;
use crate::board_record::ReadSnapshot;
use crate::contracts::AccountId;
use chrono::{DateTime, Utc};

/// The board-read table. Kilo, round 18: `board_reads(account, session, key, payload
/// JSON, recorded_at)` with `UNIQUE(account, session, key)`.
pub(super) const SCHEMA: &str = "";

impl CapitalStore {
    /// Stores snapshots; one already stored for the same account, session and key is kept
    /// as it was. Returns how many were new.
    pub fn record_board_reads(
        &self,
        account: &AccountId,
        reads: &[ReadSnapshot],
        now: DateTime<Utc>,
    ) -> Result<u32, StoreError> {
        // Kilo, round 18.
        let _ = (account, reads, now);
        Ok(0)
    }

    /// Every stored snapshot for the account, oldest session first, then key.
    pub fn board_reads(&self, account: &AccountId) -> Result<Vec<ReadSnapshot>, StoreError> {
        // Kilo, round 18.
        let _ = account;
        Ok(Vec::new())
    }

    /// The latest session with a stored snapshot for the account.
    pub fn latest_board_session(
        &self,
        account: &AccountId,
    ) -> Result<Option<chrono::NaiveDate>, StoreError> {
        // Kilo, round 18.
        let _ = account;
        Ok(None)
    }
}
