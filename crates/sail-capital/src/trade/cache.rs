//! Round 19: the replay cache's pure half (Kilo's slice). **This module doc is the spec.**
//!
//! A replay of 8 years of bars is the slow part of a run, and its past never changes:
//! a trade whose signal session is more than [`super::REPLAY_SAFE_SESSIONS`] sessions
//! before the last bar has closed. So a run stores, per instrument and cost tag, the
//! trades signalled before a cut session and a hash of the bars before that cut. The next
//! run reuses them when the hash still matches, and replays only from the cut
//! ([`super::replay_from`]).
//!
//! - [`cut_session`]: the session `REPLAY_SAFE_SESSIONS` bars before the last bar
//!   (`None` when there are fewer bars than that).
//! - [`prefix_hash`]: lowercase hex SHA-256 over the bars with `session < cut`, each fed
//!   as `"{session}|{open:016x}|{high:016x}|{low:016x}|{close:016x}\n"` with the `f64`s as
//!   `to_bits()`. Any change to any earlier bar changes the hash.
//! - [`costs_tag`]: a stable string of the four `Costs` numbers' `to_bits()`, so a change
//!   to the assumed costs invalidates the cache.
//! - [`merge`]: cached trades with `signal_session < cut` followed by the fresh
//!   [`super::replay_from`] trades, in the order [`super::replay`] would give
//!   (setup order, then signal session). The invariant tested in
//!   `tests/trade_cache.rs`: `merge(cached, replay_from(i, c, cut)) == replay(i, c)` for
//!   random walks after appending bars.
//!
//! Pure: no I/O, no clock.

use chrono::NaiveDate;
use sha2::{Digest, Sha256};

use super::{Bar, Costs, SetupId, Trade};

/// Kilo, round 19.
pub fn cut_session(bars: &[Bar]) -> Option<NaiveDate> {
    let index = bars.len().checked_sub(super::REPLAY_SAFE_SESSIONS + 1)?;
    bars.get(index).map(|bar| bar.session)
}

/// Kilo, round 19.
pub fn prefix_hash(bars: &[Bar], cut: NaiveDate) -> String {
    let mut hasher = Sha256::new();
    for bar in bars.iter().take_while(|bar| bar.session < cut) {
        hasher.update(
            format!(
                "{}|{:016x}|{:016x}|{:016x}|{:016x}\n",
                bar.session,
                bar.open.to_bits(),
                bar.high.to_bits(),
                bar.low.to_bits(),
                bar.close.to_bits()
            )
            .as_bytes(),
        );
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Kilo, round 19.
pub fn costs_tag(costs: Costs) -> String {
    format!(
        "{:016x}:{:016x}:{:016x}",
        costs.round_trip.to_bits(),
        costs.fx_conversion.to_bits(),
        costs.cfd_daily_financing.to_bits()
    )
}

/// Kilo, round 19.
pub fn merge(cached: Vec<Trade>, fresh: Vec<Trade>, _cut: NaiveDate) -> Vec<Trade> {
    let mut left = cached.into_iter().peekable();
    let mut right = fresh.into_iter().peekable();
    let mut out = Vec::with_capacity(left.len() + right.len());
    loop {
        let take_left = match (left.peek(), right.peek()) {
            (Some(left), Some(right)) => order(left) <= order(right),
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if take_left {
            if let Some(trade) = left.next() {
                out.push(trade);
            }
        } else if let Some(trade) = right.next() {
            out.push(trade);
        }
    }
    out
}

fn order(trade: &Trade) -> (usize, NaiveDate) {
    (setup_rank(trade.setup), trade.signal_session)
}

fn setup_rank(setup: SetupId) -> usize {
    match setup {
        SetupId::TrendPullback => 0,
        SetupId::SqueezeBreakout => 1,
        SetupId::Donchian55 => 2,
        SetupId::BollingerReversion => 3,
        SetupId::InsideBarBreak => 4,
        SetupId::ThreeDayReversal => 5,
    }
}
