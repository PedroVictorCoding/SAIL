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

use super::{Bar, Costs, Trade};

/// Kilo, round 19.
pub fn cut_session(bars: &[Bar]) -> Option<NaiveDate> {
    let _ = bars;
    None
}

/// Kilo, round 19.
pub fn prefix_hash(bars: &[Bar], cut: NaiveDate) -> String {
    let _ = (bars, cut);
    String::new()
}

/// Kilo, round 19.
pub fn costs_tag(costs: Costs) -> String {
    let _ = costs;
    String::new()
}

/// Kilo, round 19.
pub fn merge(cached: Vec<Trade>, fresh: Vec<Trade>, cut: NaiveDate) -> Vec<Trade> {
    let _ = cut;
    let mut out = cached;
    out.extend(fresh);
    out
}
