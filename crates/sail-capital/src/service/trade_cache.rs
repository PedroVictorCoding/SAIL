//! Round 19: cached, parallel replays and the live evidence for a trade run (Kilo's slice).
use super::{CapitalService, ServiceError};
use crate::contracts::AccountId;
use crate::store::ReplayCache;
use crate::trade::cache::{costs_tag, cut_session, merge, prefix_hash};
use crate::trade::select::LiveTrade;
use crate::trade::{Costs, Instrument, REPLAY_SAFE_SESSIONS, Trade, replay, replay_from};
use chrono::Utc;
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
        let mut caches: Vec<Option<ReplayCache>> = vec![None; universe.len()];
        let mut usable = Vec::with_capacity(universe.len());
        for (index, instrument) in universe.iter().enumerate() {
            if !crate::trade::quality::usable(&instrument.bars) {
                continue;
            }
            let costs = Costs::default_for(instrument.class);
            caches[index] = self
                .store
                .replay_cache(&instrument.key, &costs_tag(costs))?;
            usable.push(index);
        }

        let mut jobs: Vec<(usize, Option<ReplayCache>)> = usable
            .into_iter()
            .map(|index| (index, caches[index].take()))
            .collect();
        let workers = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1);
        let outcomes: Vec<(usize, Vec<Trade>, bool)> = if workers <= 1 || jobs.len() <= 1 {
            jobs.into_iter()
                .map(|(index, cache)| {
                    let (trades, hit) = replay_instrument(&universe[index], cache);
                    (index, trades, hit)
                })
                .collect()
        } else {
            let size = jobs.len().div_ceil(workers).max(1);
            let job_count = jobs.len();
            std::thread::scope(|scope| -> Result<Vec<_>, ServiceError> {
                let mut handles = Vec::new();
                for chunk in jobs.chunks_mut(size) {
                    handles.push(scope.spawn(|| {
                        chunk
                            .iter_mut()
                            .map(|(index, cache)| {
                                let (trades, hit) =
                                    replay_instrument(&universe[*index], cache.take());
                                (*index, trades, hit)
                            })
                            .collect::<Vec<_>>()
                    }));
                }
                let mut out = Vec::with_capacity(job_count);
                for handle in handles {
                    match handle.join() {
                        Ok(chunk) => out.extend(chunk),
                        Err(_) => {
                            return Err(ServiceError::BadRequest(
                                "a replay worker panicked".to_string(),
                            ));
                        }
                    }
                }
                Ok(out)
            })?
        };

        let mut replays = HashMap::with_capacity(outcomes.len());
        let mut fresh = Vec::with_capacity(outcomes.len());
        let mut hits = 0u32;
        let mut misses = 0u32;
        for (index, trades, hit) in outcomes {
            let instrument = &universe[index];
            if hit {
                hits += 1;
            } else {
                misses += 1;
            }
            if let Some(cut) = cut_session(&instrument.bars) {
                let cached = trades
                    .iter()
                    .filter(|trade| trade.signal_session < cut)
                    .cloned()
                    .collect();
                fresh.push(ReplayCache {
                    key: instrument.key.clone(),
                    costs_tag: costs_tag(Costs::default_for(instrument.class)),
                    cut,
                    prefix_hash: prefix_hash(&instrument.bars, cut),
                    trades: cached,
                });
            }
            replays.insert(instrument.key.clone(), trades);
        }
        self.store.record_replay_caches(&fresh, Utc::now())?;
        Ok(CachedReplays {
            replays,
            hits,
            misses,
        })
    }

    /// Closed live cards (`status == "closed"` with a graded trade), both tiers, as
    /// [`LiveTrade`]s with the card's group from [`crate::trade::group_of`]. Kilo,
    /// round 19.
    pub(super) fn live_trades(&self, account: &AccountId) -> Result<Vec<LiveTrade>, ServiceError> {
        let mut out = Vec::new();
        for graded in self.store.trade_record(account)? {
            if graded.status != "closed" {
                continue;
            }
            let Some(trade) = graded.graded else {
                continue;
            };
            out.push(LiveTrade {
                group: crate::trade::group_of(graded.card.class).to_string(),
                setup: graded.card.setup,
                side: graded.card.side,
                tier: graded.card.tier,
                exit_session: trade.exit_session,
                r_multiple: trade.r_multiple,
            });
        }
        Ok(out)
    }
}

fn replay_instrument(instrument: &Instrument, cached: Option<ReplayCache>) -> (Vec<Trade>, bool) {
    let costs = Costs::default_for(instrument.class);
    let tag = costs_tag(costs);
    if let Some(cached) = cached.filter(|cached| cached.costs_tag == tag)
        && let Some(position) = instrument
            .bars
            .iter()
            .position(|bar| bar.session == cached.cut)
        && let Some(last) = instrument.bars.len().checked_sub(1)
        && last.saturating_sub(position) >= REPLAY_SAFE_SESSIONS
        && prefix_hash(&instrument.bars, cached.cut) == cached.prefix_hash
    {
        let fresh = replay_from(instrument, costs, cached.cut);
        return (merge(cached.trades, fresh, cached.cut), true);
    }
    (replay(instrument, costs), false)
}
