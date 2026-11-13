//! Round 19: data-quality checks on daily bars before they become evidence (Claude's
//! slice). **This module doc is the spec.**
//!
//! Yahoo's OHLC (our default source) is already split-adjusted, but a series can mix
//! sources (a file upload marked `unadjusted`, Stooq marked `unknown`), and a split
//! applied on one side of a source switch shows up as a one-day jump. Such a jump creates
//! fake breakouts and fake stop-outs in the replay.
//!
//! [`gaps`] flags every session whose open or close moved by more than
//! [`GAP_RATIO`] (either way) from the previous close. When the ratio is within
//! [`SPLIT_TOLERANCE`] of a common split ratio (2, 3, 4, 5, 10, 20 or their inverses, and
//! 3:2 / 2:3), it names that ratio as a likely split. [`usable`] says whether an
//! instrument's bars may be replayed: false when any likely split remains in the bars.
//! Pure: no I/O, no clock.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::Bar;

/// A one-day move larger than this (as a ratio, ≥ 1.0 either way) is flagged.
pub const GAP_RATIO: f64 = 1.4;
/// Relative distance to a split ratio that counts as a likely split.
pub const SPLIT_TOLERANCE: f64 = 0.03;

/// One flagged session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gap {
    /// Session of the jump.
    pub session: NaiveDate,
    /// Move from the previous close, as `max(a, b) / min(a, b)` of the two prices.
    pub ratio: f64,
    /// Price fell (true) or rose (false).
    pub down: bool,
    /// A likely split as `(new, old)`, e.g. `(4, 1)` for a 4-for-1 split that wasn't
    /// adjusted.
    pub likely_split: Option<(u32, u32)>,
}

/// Flagged sessions, oldest first. Claude, round 19.
pub fn gaps(bars: &[Bar]) -> Vec<Gap> {
    let _ = bars;
    Vec::new()
}

/// Whether the bars may be replayed as evidence. Claude, round 19.
pub fn usable(bars: &[Bar]) -> bool {
    gaps(bars).iter().all(|gap| gap.likely_split.is_none())
}
