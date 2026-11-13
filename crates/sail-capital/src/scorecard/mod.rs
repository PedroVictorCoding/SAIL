//! One track record for everything that claims to predict (round 17).
//!
//! Rounds 10–13 each built their own evaluation, with its own vocabulary: baseline
//! forecasts (CRPS skill and calibration), signal skill (rank IC), the blended outlook
//! (replay coverage), trade setups (expectancy with a holdout) and the live record of
//! published cards. The owner should not have to read five tables to learn the one thing
//! that matters: **has anything earned trust yet?**
//!
//! [`summarize`] maps every result into a [`ScoreRow`] with one shared [`Standing`]:
//!
//! | Family | Row per | `Earned` when | `NotYet` when | otherwise |
//! |---|---|---|---|---|
//! | `Forecast` | [`ModelScore`] except `ModelId::NoChange` (it is the benchmark) | `forecasts ≥ 100`, `calibrated` and `crps_skill > 0` | `forecasts < 100` | `Failed` |
//! | `Signal` | [`SignalSkill`] | `weight > 0` | `origins < signals::MIN_ORIGINS` | `Failed` |
//! | `Outlook` | [`TrackRecord`] | `replays ≥ 24`, `skill_vs_no_change > 0` and `|inside_90 − 0.90| ≤ 0.10` | `replays < 24` | `Failed` |
//! | `Setup` | [`Verdict`] | `publishes` | `selection.trades < trade::MIN_TRADES` | `Failed` |
//! | `Live` | one row from [`RecordTotals`], when given | `closed ≥ 20` and `mean_r > 0` | `closed < 20` | `Failed` |
//!
//! **Names** are plain words, never snake_case ids:
//! - `Forecast`: "No change", "Trailing drift", "EWMA volatility", "Past returns";
//! - `Signal`: "12-1 momentum", "Short-term reversal", "Low volatility", "Trend",
//!   "Chart patterns (YOLO)", "Chart trend (YOLO)", "Foundation model (Chronos)",
//!   "Filing sentiment", "Insider buying";
//! - `Outlook`: "Blended outlook";
//! - `Setup`: setup name, side and group, e.g. "Squeeze breakout · long · stocks", with
//!   setup names "Trend pullback", "Squeeze breakout", "55-day breakout", "Bollinger
//!   reversion", "Inside-bar break", "Three-day reversal";
//! - `Live`: "Published signals (live)".
//!
//! **`key`** is stable and unique across the card: `"<family>:<id>[:<side>:<group>]:h<horizon>"`
//! (omit the parts that don't apply), e.g. `"forecast:gbm:h21"`, `"setup:breakout:long:stocks"`,
//! `"live"`.
//!
//! **`headline`** is one short sentence with the number that decided the standing, in
//! words, with no jargon the page doesn't explain. Examples:
//! - Forecast: "12% better than no change over 1,240 forecasts; ranges well calibrated."
//!   or "Ranges too narrow: 78% landed inside the 90% band.";
//! - Signal: "Rank IC +0.04 (t 2.3) over 40 weeks; weight 0.31." or "Rank IC +0.01 (t 0.4):
//!   indistinguishable from chance.";
//! - Outlook: "90% band held 88% of 36 monthly replays; 3% better than no change.";
//! - Setup: the [`Verdict::reason`] as is;
//! - Live: "12 closed: hit target 1 in 58%, mean +0.21R.";
//! - any `NotYet`: what is missing, e.g. "Needs 100 scored forecasts; has 41.".
//!
//! Percentages are rounded to whole numbers, ICs to 2 decimals, R to 2 decimals.
//! `evidence` is the sample size behind the row (forecasts, origins, replays, trades or
//! closed cards).
//!
//! **Order:** `Earned`, then `NotYet`, then `Failed`; within a standing by family in the
//! table's order, then horizon ascending, then name.
//!
//! **`summary`** is one sentence:
//! - nothing earned: "Nothing has earned trust yet. SAIL shows ranges, not calls.";
//! - otherwise: "<n> of <total> earned trust: <names of up to 3 earned rows, comma separated>."
//!
//! These names live in private helpers in this module (an exhaustive `match`, so a new
//! id fails to compile here). The page shows them as given.
//!
//! `summarize` is pure: no I/O, no clock (it takes `as_of`), deterministic.
//!
//! JSON: `Family` and `Standing` serialize as plain snake_case strings (`"forecast"`,
//! `"not_yet"`), not tagged objects.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::forecast::ModelScore;
use crate::outlook::TrackRecord;
use crate::service::RecordTotals;
use crate::signals::SignalSkill;
use crate::trade::Verdict;

/// Which evaluation a row comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    /// Baseline forecasts, walk-forward (round 10).
    Forecast,
    /// Price and model signals, rank IC (rounds 11–12).
    Signal,
    /// The blended outlook, monthly replays (round 12).
    Outlook,
    /// Trade setups, replay with a holdout (round 13).
    Setup,
    /// Published cards graded live (round 13).
    Live,
}

/// Whether a row has earned trust.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    /// Passed its bar out of sample.
    Earned,
    /// Too little evidence to judge.
    NotYet,
    /// Enough evidence, and it did not pass.
    Failed,
}

/// One line of the track record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoreRow {
    /// Stable id (see the module doc).
    pub key: String,
    /// Which evaluation.
    pub family: Family,
    /// Plain name.
    pub name: String,
    /// Horizon in sessions, when the row has one.
    pub horizon_sessions: Option<u32>,
    /// The verdict.
    pub standing: Standing,
    /// One sentence with the deciding number.
    pub headline: String,
    /// Sample size behind the row.
    pub evidence: u32,
}

/// The whole track record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scorecard {
    /// When it was assembled.
    pub as_of: DateTime<Utc>,
    /// Rows, ordered as documented.
    pub rows: Vec<ScoreRow>,
    /// Count of `Earned` rows.
    pub earned: u32,
    /// One sentence.
    pub summary: String,
}

/// Everything [`summarize`] reads. Missing evaluations are simply empty.
#[derive(Clone, Debug, Default)]
pub struct ScorecardInputs<'a> {
    /// Baseline forecast scores (`EvalReport::scores`).
    pub forecasts: &'a [ModelScore],
    /// Signal skill (`SignalReport::skill`).
    pub signals: &'a [SignalSkill],
    /// Outlook track record, one per horizon.
    pub outlook: &'a [TrackRecord],
    /// The latest trade run's scoreboard.
    pub setups: &'a [Verdict],
    /// Totals of the live record, when any card was ever published.
    pub live: Option<&'a RecordTotals>,
}

/// Builds the track record. See the module doc.
pub fn summarize(inputs: &ScorecardInputs<'_>, as_of: DateTime<Utc>) -> Scorecard {
    let _ = (inputs, as_of);
    unimplemented!("round 17: Kilo")
}
