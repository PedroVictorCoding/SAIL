//! Recording and grading the signal board's reads ([`crate::board_record`]).

use super::{CapitalService, ServiceError};
use crate::board_record::BoardRecord;
use crate::contracts::AccountId;
use chrono::{DateTime, Utc};

impl CapitalService {
    /// Stores today's board reads unless this session is already stored. Returns how many
    /// were new. Called by the background warm-up after it builds the board.
    pub fn record_board_snapshot(
        &self,
        account: &AccountId,
        now: DateTime<Utc>,
    ) -> Result<u32, ServiceError> {
        // Kilo, round 18: build the board (cached), skip when its session equals
        // `latest_board_session`, else `snapshot` and `record_board_reads`.
        let _ = (account, now);
        Ok(0)
    }

    /// The graded record. Cached until the stored reads or prices change.
    pub fn board_record(&self, account: &AccountId) -> Result<BoardRecord, ServiceError> {
        // Kilo, round 18: `board_reads`, then `grade` with `closes_for` over the keys,
        // then `summarize`.
        let _ = account;
        Ok(BoardRecord::default())
    }
}
