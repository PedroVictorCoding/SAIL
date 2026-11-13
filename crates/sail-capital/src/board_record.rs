//! The board's forward record (round 18): every read the signal board ([`crate::board`])
//! makes is stored once per session, then graded when its outcome is known. This is how
//! an *unproven* read earns (or loses) trust in the open, instead of only in back-tests.
//!
//! **Snapshot.** [`snapshot`] turns a board into one [`ReadSnapshot`] per row that has a
//! session and a close. Rows without a close are skipped. The store keeps the first
//! snapshot of each `(account, session, key)` and ignores later ones (append-only), so a
//! read can never be rewritten after the fact.
//!
//! **Grading.** For each snapshot and each horizon `h` in [`GRADE_HORIZONS`]:
//! - The outcome is the simple return from the snapshot close to the instrument's close
//!   `h` sessions later, counted in the instrument's own stored sessions after the
//!   snapshot session. With fewer than `h` later sessions the read is not graded yet.
//! - The **board return** is the median outcome of every snapshot taken on the same
//!   session and graded at the same horizon. A read needs at least
//!   [`MIN_PEERS`] graded peers on its session (itself included), else it isn't graded.
//! - `excess = outcome − board return`; `beat_board = excess > 0`.
//! - With an outlook at that horizon (21 ↔ 21, 63 ↔ 63): `inside_90` is
//!   `low ≤ outcome ≤ high`, and the probability `p_up` is scored against
//!   `ended_up = outcome > 0`.
//!
//! **Summary** ([`summarize`]):
//! - [`LeanRecord`] per `(lean, proven, horizon)` that has at least one graded read, in the
//!   order favoured, neutral, mixed, disfavoured, then `proven` before unproven, then
//!   horizon ascending. `no_data` reads are never graded.
//!   - `beat_board_rate`: share of reads with `beat_board`.
//!   - `mean_excess` and a 95% interval `mean ± 1.96·sd/√n`, with `sd` the sample
//!     standard deviation (the interval is `(mean, mean)` when `n < 2`).
//!   - Reads from one session share outcomes, so the interval is optimistic; a note says so.
//! - [`OutlookCalibration`] per horizon with graded outlooks: `inside_90` share, the Brier
//!   score of `p_up` (`mean((p − ended_up)²)`), the Brier score of always saying the
//!   base rate (the share that ended up, over the same reads), and the direction hit rate
//!   (`median > 0` matched `ended_up`; zero medians are skipped).
//! - `recent`: the latest graded reads, newest session first, then symbol, at most
//!   [`RECENT_GRADED`].
//! - `notes`: plain sentences. Always one on overlap (the interval) and one stating how
//!   many sessions are recorded. When nothing is graded yet, one saying when the first
//!   1-month grades arrive (about 21 sessions after the first snapshot).
//!
//! **Purity.** No I/O and no clock.
use crate::board::{Lean, OutlookSummary, SignalBoard};
use crate::signals::SignalId;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Horizons reads are graded at, in sessions (the board's outlook horizons).
pub const GRADE_HORIZONS: [u32; 2] = [21, 63];
/// Fewest graded snapshots on one session before any of them is graded.
pub const MIN_PEERS: usize = 5;
/// Most graded reads in [`BoardRecord::recent`].
pub const RECENT_GRADED: usize = 50;

/// One stored read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadSnapshot {
    /// Session of the close the read was made on.
    pub session: NaiveDate,
    /// Store key of the price series.
    pub key: String,
    /// XTB symbol.
    pub symbol: String,
    /// Held when the read was made.
    pub held: bool,
    /// The lean.
    pub lean: Lean,
    /// Rested on validated signals.
    pub proven: bool,
    /// The close the read was made on.
    pub close: f64,
    /// Percentile per signal at the time.
    pub percentiles: Vec<(SignalId, f64)>,
    /// The outlook at the time.
    pub outlook: Vec<OutlookSummary>,
}

/// One read with its outcome at one horizon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradedRead {
    /// Session the read was made on.
    pub session: NaiveDate,
    /// XTB symbol.
    pub symbol: String,
    /// Held when the read was made.
    pub held: bool,
    /// The lean.
    pub lean: Lean,
    /// Rested on validated signals.
    pub proven: bool,
    /// Horizon in sessions.
    pub horizon_sessions: u32,
    /// Simple return over the horizon.
    pub outcome: f64,
    /// Median outcome of the board on the same session.
    pub board_return: f64,
    /// `outcome − board_return`.
    pub excess: f64,
    /// `excess > 0`.
    pub beat_board: bool,
    /// The outlook's chance of ending higher, when there was an outlook.
    pub p_up: Option<f64>,
    /// The outlook's median, when there was one.
    pub median: Option<f64>,
    /// The outcome fell inside the outlook's 90% range.
    pub inside_90: Option<bool>,
}

/// How one kind of read has done.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LeanRecord {
    /// The lean.
    pub lean: Lean,
    /// Validated or not.
    pub proven: bool,
    /// Horizon in sessions.
    pub horizon_sessions: u32,
    /// Graded reads.
    pub reads: u32,
    /// Share that beat the board.
    pub beat_board_rate: f64,
    /// Mean excess return.
    pub mean_excess: f64,
    /// 95% interval on the mean excess.
    pub excess_ci: (f64, f64),
}

/// How the outlook has done at one horizon, on recorded reads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutlookCalibration {
    /// Horizon in sessions.
    pub horizon_sessions: u32,
    /// Graded outlooks.
    pub graded: u32,
    /// Share inside the 90% range (aim: 0.90).
    pub inside_90: f64,
    /// Brier score of the chance of ending higher (lower is better).
    pub brier: f64,
    /// Brier score of always saying the base rate.
    pub brier_base_rate: f64,
    /// Share where the median's direction was right.
    pub direction_hit_rate: f64,
}

/// `GET /signals/board/record`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BoardRecord {
    /// First recorded session.
    pub first_session: Option<NaiveDate>,
    /// Sessions with a snapshot.
    pub sessions: u32,
    /// Reads graded, all horizons.
    pub graded: u32,
    /// By lean, proof and horizon.
    pub by_lean: Vec<LeanRecord>,
    /// The outlook's calibration by horizon.
    pub outlook: Vec<OutlookCalibration>,
    /// Latest graded reads.
    pub recent: Vec<GradedRead>,
    /// Plain notes.
    pub notes: Vec<String>,
}

/// One snapshot per board row with a session and a close (see the module doc).
pub fn snapshot(board: &SignalBoard) -> Vec<ReadSnapshot> {
    // Kilo, round 18.
    let _ = board;
    Vec::new()
}

/// Grades `snapshots` against later closes. `closes(key)` gives the instrument's stored
/// closes, oldest first.
pub fn grade(
    snapshots: &[ReadSnapshot],
    closes: &dyn Fn(&str) -> Vec<(NaiveDate, f64)>,
) -> Vec<GradedRead> {
    // Kilo, round 18.
    let _ = (snapshots, closes);
    Vec::new()
}

/// The record from snapshots and their grades.
pub fn summarize(snapshots: &[ReadSnapshot], graded: &[GradedRead]) -> BoardRecord {
    // Kilo, round 18.
    let _ = (snapshots, graded);
    BoardRecord::default()
}
