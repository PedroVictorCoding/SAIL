//! Round 19: cached, parallel replays and the live evidence for a trade run (Kilo's slice).
use super::{CapitalService, ServiceError};
use crate::contracts::AccountId;
use crate::trade::select::LiveTrade;
use crate::trade::{Instrument, Trade};
use std::collections::HashMap;

/// Replayed trades per instrument key, with cache counts.
#[derive(Debug, Default)]
pub(super) struct CachedReplays {
    /// Trades per instrument key.
    pub replays: HashMap<String, Vec<Trade>>,
    /// Instruments served from the cache.
    pub hits: u32,
    /// Instruments replayed in full.
    pub misses: u32,
}

impl CapitalService {
    /// Every instrument's replayed trades, keyed by instrument key, with the counts of
    /// cache hits and misses. Kilo, round 19:
    /// - per instrument, read [`crate::store::ReplayCache`] for its key and
    ///   [`crate::trade::cache::costs_tag`] of `Costs::default_for(class)`;
    /// - a hit needs the stored `cut` to be a session in the current bars at least
    ///   `REPLAY_SAFE_SESSIONS` before the last bar, and `prefix_hash(bars, cut)` to
    ///   match; then `merge(cached, replay_from(i, costs, cut), cut)`; else a full
    ///   `replay` (a miss);
    /// - replays run in parallel with `std::thread::scope` over
    ///   `std::thread::available_parallelism()` chunks (no new dependencies);
    /// - afterwards store a fresh prefix for every instrument at its current
    ///   `cut_session` in **one** `record_replay_caches` call;
    /// - instruments whose bars fail [`crate::trade::quality::usable`] are skipped here
    ///   (the caller drops them; they never reach the evidence).
    pub(super) fn cached_replays(
        &self,
        universe: &[Instrument],
    ) -> Result<CachedReplays, ServiceError> {
        Ok(CachedReplays {
            misses: universe.len() as u32,
            ..CachedReplays::default()
        })
    }

    /// Closed live cards (`status == "closed"` with a graded trade), both tiers, as
    /// [`LiveTrade`]s with the card's group from [`crate::trade::group_of`]. Kilo,
    /// round 19.
    pub(super) fn live_trades(&self, account: &AccountId) -> Result<Vec<LiveTrade>, ServiceError> {
        let _ = account;
        Ok(Vec::new())
    }
}
