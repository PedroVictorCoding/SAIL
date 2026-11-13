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

/// Common split ratios, as `(new, old)` shares: a 4-for-1 split makes the price fall to a
/// quarter, a 1-for-10 reverse split makes it ten times higher.
const SPLITS: [(u32, u32); 14] = [
    (2, 1),
    (3, 1),
    (4, 1),
    (5, 1),
    (10, 1),
    (20, 1),
    (3, 2),
    (1, 2),
    (1, 3),
    (1, 4),
    (1, 5),
    (1, 10),
    (1, 20),
    (2, 3),
];

/// Flagged sessions, oldest first: the larger of the open's and the close's move from the
/// previous close, when above [`GAP_RATIO`].
pub fn gaps(bars: &[Bar]) -> Vec<Gap> {
    let mut out = Vec::new();
    for pair in bars.windows(2) {
        let (before, bar) = (pair[0], pair[1]);
        let previous = before.close;
        if !(previous.is_finite() && previous > 0.0) {
            continue;
        }
        let jump = [bar.open, bar.close]
            .into_iter()
            .filter(|price| price.is_finite() && *price > 0.0)
            .map(|price| (price, price.max(previous) / price.min(previous)))
            .max_by(|left, right| left.1.total_cmp(&right.1));
        let Some((price, ratio)) = jump else {
            continue;
        };
        if ratio <= GAP_RATIO {
            continue;
        }
        let down = price < previous;
        // The price moved by `previous / price`; a (new, old) split divides it by new/old.
        let moved = previous / price;
        let likely_split = SPLITS.into_iter().find(|(new, old)| {
            let expected = f64::from(*new) / f64::from(*old);
            ((moved - expected) / expected).abs() <= SPLIT_TOLERANCE
        });
        out.push(Gap {
            session: bar.session,
            ratio,
            down,
            likely_split,
        });
    }
    out
}

/// Whether the bars may be replayed as evidence: no flagged jump looks like a split.
pub fn usable(bars: &[Bar]) -> bool {
    gaps(bars).iter().all(|gap| gap.likely_split.is_none())
}
