//! Assembling the one track record for an account.

use super::*;


impl CapitalService {
    /// `GET /scorecard`: every evaluation as one track record ([`crate::scorecard`]).
    ///
    /// Gathers, for `account`: the baseline evaluation's scores ([`Self::evaluation`]), the
    /// signal skill ([`Self::signals`]), the outlook track record at every outlook horizon
    /// ([`Self::track_record`]), the latest trade run's scoreboard and the live record's
    /// totals (only when a card was ever published). An evaluation that can't run yet
    /// (no prices, no trade run) contributes nothing; any other error is returned.
    pub fn scorecard(
        &self,
        account: &AccountId,
    ) -> Result<crate::scorecard::Scorecard, ServiceError> {
        let _ = account;
        unimplemented!("round 17: Kilo")
    }
}
