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
use std::collections::{BTreeMap, BTreeSet};

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
    board
        .rows
        .iter()
        .filter_map(|row| {
            let session = row.session?;
            let close = row.close?;
            Some(ReadSnapshot {
                session,
                key: row.meta.key.clone(),
                symbol: row.meta.symbol.clone(),
                held: row.meta.held,
                lean: row.read.lean,
                proven: row.read.proven,
                close,
                percentiles: row
                    .readings
                    .iter()
                    .map(|reading| (reading.signal, reading.percentile))
                    .collect(),
                outlook: row.outlook.clone(),
            })
        })
        .collect()
}

/// Grades `snapshots` against later closes. `closes(key)` gives the instrument's stored
/// closes, oldest first.
pub fn grade(
    snapshots: &[ReadSnapshot],
    closes: &dyn Fn(&str) -> Vec<(NaiveDate, f64)>,
) -> Vec<GradedRead> {
    let mut series: BTreeMap<&str, Vec<(NaiveDate, f64)>> = BTreeMap::new();
    for snapshot in snapshots {
        if snapshot.lean == Lean::NoData {
            continue;
        }
        series
            .entry(snapshot.key.as_str())
            .or_insert_with(|| closes(&snapshot.key));
    }

    let mut graded = Vec::new();
    for &horizon in &GRADE_HORIZONS {
        let mut outcomes: Vec<(usize, f64)> = Vec::new();
        for (index, snapshot) in snapshots.iter().enumerate() {
            if snapshot.lean == Lean::NoData {
                continue;
            }
            let Some(closes) = series.get(snapshot.key.as_str()) else {
                continue;
            };
            let Some(position) = closes
                .iter()
                .position(|(session, _)| *session == snapshot.session)
            else {
                continue;
            };
            let Some((_, later)) = closes.get(position + horizon as usize) else {
                continue;
            };
            let outcome = later / snapshot.close - 1.0;
            if outcome.is_finite() {
                outcomes.push((index, outcome));
            }
        }

        let mut groups: Vec<(NaiveDate, Vec<(usize, f64)>)> = Vec::new();
        for (index, outcome) in outcomes {
            let session = snapshots[index].session;
            match groups.iter_mut().find(|(day, _)| *day == session) {
                Some((_, group)) => group.push((index, outcome)),
                None => groups.push((session, vec![(index, outcome)])),
            }
        }

        for (_, group) in groups {
            if group.len() < MIN_PEERS {
                continue;
            }
            let board_return = median(group.iter().map(|(_, outcome)| *outcome));
            for (index, outcome) in group {
                let snapshot = &snapshots[index];
                let outlook = snapshot
                    .outlook
                    .iter()
                    .find(|outlook| outlook.horizon_sessions == horizon);
                graded.push(GradedRead {
                    session: snapshot.session,
                    symbol: snapshot.symbol.clone(),
                    held: snapshot.held,
                    lean: snapshot.lean,
                    proven: snapshot.proven,
                    horizon_sessions: horizon,
                    outcome,
                    board_return,
                    excess: outcome - board_return,
                    beat_board: outcome > board_return,
                    p_up: outlook.map(|outlook| outlook.p_up),
                    median: outlook.map(|outlook| outlook.median),
                    inside_90: outlook
                        .map(|outlook| outlook.low <= outcome && outcome <= outlook.high),
                });
            }
        }
    }
    graded
}

/// The record from snapshots and their grades.
pub fn summarize(snapshots: &[ReadSnapshot], graded: &[GradedRead]) -> BoardRecord {
    let first_session = snapshots.iter().map(|snapshot| snapshot.session).min();
    let sessions = snapshots
        .iter()
        .map(|snapshot| snapshot.session)
        .collect::<BTreeSet<_>>()
        .len() as u32;

    let mut groups: Vec<(Lean, bool, u32, Vec<&GradedRead>)> = Vec::new();
    for read in graded {
        if read.lean == Lean::NoData {
            continue;
        }
        match groups.iter_mut().find(|(lean, proven, horizon, _)| {
            *lean == read.lean && *proven == read.proven && *horizon == read.horizon_sessions
        }) {
            Some((_, _, _, reads)) => reads.push(read),
            None => groups.push((read.lean, read.proven, read.horizon_sessions, vec![read])),
        }
    }
    groups.sort_by_key(|(lean, proven, horizon, _)| (lean_rank(*lean), !*proven, *horizon));
    let by_lean: Vec<LeanRecord> = groups
        .into_iter()
        .map(|(lean, proven, horizon_sessions, reads)| {
            let count = reads.len();
            let n = count as f64;
            let mean_excess = reads.iter().map(|read| read.excess).sum::<f64>() / n;
            let excess_ci = if count < 2 {
                (mean_excess, mean_excess)
            } else {
                let variance = reads
                    .iter()
                    .map(|read| (read.excess - mean_excess).powi(2))
                    .sum::<f64>()
                    / (n - 1.0);
                let margin = 1.96 * variance.sqrt() / n.sqrt();
                (mean_excess - margin, mean_excess + margin)
            };
            LeanRecord {
                lean,
                proven,
                horizon_sessions,
                reads: count as u32,
                beat_board_rate: reads.iter().filter(|read| read.beat_board).count() as f64 / n,
                mean_excess,
                excess_ci,
            }
        })
        .collect();

    let mut outlook = Vec::new();
    for &horizon in &GRADE_HORIZONS {
        let reads: Vec<&GradedRead> = graded
            .iter()
            .filter(|read| read.horizon_sessions == horizon && read.p_up.is_some())
            .collect();
        if reads.is_empty() {
            continue;
        }
        let n = reads.len() as f64;
        let ended_up = |read: &GradedRead| if read.outcome > 0.0 { 1.0 } else { 0.0 };
        let base_rate = reads.iter().map(|read| ended_up(read)).sum::<f64>() / n;
        let direction: Vec<bool> = reads
            .iter()
            .filter_map(|read| {
                let median = read.median?;
                (median != 0.0).then_some((median > 0.0) == (ended_up(read) == 1.0))
            })
            .collect();
        outlook.push(OutlookCalibration {
            horizon_sessions: horizon,
            graded: reads.len() as u32,
            inside_90: reads
                .iter()
                .filter(|read| read.inside_90 == Some(true))
                .count() as f64
                / n,
            brier: reads
                .iter()
                .map(|read| {
                    let p = read.p_up.unwrap_or(base_rate);
                    (p - ended_up(read)).powi(2)
                })
                .sum::<f64>()
                / n,
            brier_base_rate: reads
                .iter()
                .map(|read| (base_rate - ended_up(read)).powi(2))
                .sum::<f64>()
                / n,
            direction_hit_rate: if direction.is_empty() {
                0.0
            } else {
                direction.iter().filter(|hit| **hit).count() as f64 / direction.len() as f64
            },
        });
    }

    let mut recent = graded.to_vec();
    recent.sort_by(|a, b| {
        b.session
            .cmp(&a.session)
            .then(a.symbol.cmp(&b.symbol))
            .then(a.horizon_sessions.cmp(&b.horizon_sessions))
    });
    recent.truncate(RECENT_GRADED);

    let mut notes = Vec::new();
    if graded.is_empty() {
        notes.push(
            "Nothing is graded yet: the first 1-month grades arrive about 21 sessions after the first snapshot."
                .to_string(),
        );
    }
    notes.push(
        "Reads from one session share outcomes, so the mean-excess interval is optimistic."
            .to_string(),
    );
    match first_session {
        Some(session) => notes.push(format!("Recording since {session}: {sessions} sessions.")),
        None => notes.push("No sessions have been recorded yet.".to_string()),
    }

    BoardRecord {
        first_session,
        sessions,
        graded: graded.len() as u32,
        by_lean,
        outlook,
        recent,
        notes,
    }
}

/// The order the owner reads leans in: best first, then neutral, mixed and against.
fn lean_rank(lean: Lean) -> u8 {
    match lean {
        Lean::Favoured => 0,
        Lean::Neutral => 1,
        Lean::Mixed => 2,
        Lean::Disfavoured => 3,
        Lean::NoData => 4,
    }
}

/// The median of `values`; `0` when there are none.
fn median(values: impl Iterator<Item = f64>) -> f64 {
    let mut values: Vec<f64> = values.collect();
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let count = values.len();
    if count == 0 {
        0.0
    } else if count % 2 == 1 {
        values[count / 2]
    } else {
        (values[count / 2 - 1] + values[count / 2]) / 2.0
    }
}
