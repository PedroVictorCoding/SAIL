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

use crate::forecast::{ModelId, ModelScore};
use crate::outlook::TrackRecord;
use crate::service::RecordTotals;
use crate::signals::{SignalId, SignalSkill};
use crate::trade::{MIN_TRADES, SetupId, Side, Verdict};

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
    /// The dividend growth estimate, backtested on payout histories (round 24).
    Income,
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
    /// Round 24: the latest dividend growth calibration.
    pub dividend_calibration: Option<&'a crate::dividends::calibrate::CalibrationReport>,
}

/// Builds the track record. See the module doc.
pub fn summarize(inputs: &ScorecardInputs<'_>, as_of: DateTime<Utc>) -> Scorecard {
    let mut rows = Vec::new();

    rows.extend(
        inputs
            .forecasts
            .iter()
            .filter(|score| score.model != ModelId::NoChange)
            .map(forecast_row),
    );
    rows.extend(inputs.signals.iter().map(signal_row));
    rows.extend(inputs.outlook.iter().map(outlook_row));
    rows.extend(inputs.setups.iter().map(setup_row));
    if let Some(totals) = inputs.live {
        rows.push(live_row(totals));
    }
    if let Some(report) = inputs.dividend_calibration {
        rows.push(dividend_growth_row(report));
    }

    rows.sort_by(|a, b| {
        a.standing
            .cmp(&b.standing)
            .then(a.family.cmp(&b.family))
            .then(
                a.horizon_sessions
                    .unwrap_or(0)
                    .cmp(&b.horizon_sessions.unwrap_or(0)),
            )
            .then(a.name.cmp(&b.name))
            .then(a.key.cmp(&b.key))
    });

    let earned = rows
        .iter()
        .filter(|row| row.standing == Standing::Earned)
        .count() as u32;
    let summary = if earned == 0 {
        "Nothing has earned trust yet. SAIL shows ranges, not calls.".to_string()
    } else {
        let names: Vec<&str> = rows
            .iter()
            .filter(|row| row.standing == Standing::Earned)
            .map(|row| row.name.as_str())
            .take(3)
            .collect();
        format!(
            "{earned} of {} earned trust: {}.",
            rows.len(),
            names.join(", ")
        )
    };

    Scorecard {
        as_of,
        rows,
        earned,
        summary,
    }
}

fn forecast_row(score: &ModelScore) -> ScoreRow {
    let standing = if score.forecasts >= 100 && score.calibrated && score.crps_skill > 0.0 {
        Standing::Earned
    } else if score.forecasts < 100 {
        Standing::NotYet
    } else {
        Standing::Failed
    };
    let headline = match standing {
        Standing::Earned => format!(
            "{}% better than no change over {} forecasts; ranges well calibrated.",
            whole(score.crps_skill * 100.0),
            thousands(score.forecasts)
        ),
        Standing::NotYet => format!(
            "Needs 100 scored forecasts; has {}.",
            thousands(score.forecasts)
        ),
        Standing::Failed if !score.calibrated => {
            let observed = whole(score.interval_90 * 100.0);
            if score.interval_90 < 0.85 {
                format!("Ranges too narrow: {observed}% landed inside the 90% band.")
            } else if score.interval_90 > 0.95 {
                format!("Ranges too wide: {observed}% landed inside the 90% band.")
            } else {
                format!("Ranges miscalibrated: {observed}% landed inside the 90% band.")
            }
        }
        Standing::Failed => format!(
            "No better than no change over {} forecasts.",
            thousands(score.forecasts)
        ),
    };
    ScoreRow {
        key: format!(
            "forecast:{}:h{}",
            model_key(score.model),
            score.horizon_sessions
        ),
        family: Family::Forecast,
        name: model_name(score.model).to_string(),
        horizon_sessions: Some(score.horizon_sessions),
        standing,
        headline,
        evidence: score.forecasts,
    }
}

fn signal_row(skill: &SignalSkill) -> ScoreRow {
    let standing = if skill.weight > 0.0 {
        Standing::Earned
    } else if (skill.origins as usize) < crate::signals::MIN_ORIGINS {
        Standing::NotYet
    } else {
        Standing::Failed
    };
    let headline = match standing {
        Standing::Earned => format!(
            "Rank IC {:+.2} (t {:.1}) over {} weeks; weight {:.2}.",
            skill.ic_mean, skill.t_stat, skill.origins, skill.weight
        ),
        Standing::NotYet => format!(
            "Needs {} cross-sections; has {}.",
            crate::signals::MIN_ORIGINS,
            skill.origins
        ),
        Standing::Failed => format!(
            "Rank IC {:+.2} (t {:.1}): indistinguishable from chance.",
            skill.ic_mean, skill.t_stat
        ),
    };
    ScoreRow {
        key: format!(
            "signal:{}:h{}",
            signal_key(skill.signal),
            skill.horizon_sessions
        ),
        family: Family::Signal,
        name: signal_name(skill.signal).to_string(),
        horizon_sessions: Some(skill.horizon_sessions),
        standing,
        headline,
        evidence: skill.origins,
    }
}

fn outlook_row(record: &TrackRecord) -> ScoreRow {
    let band_ok = (record.inside_90 - 0.90).abs() <= 0.10;
    let standing = if record.replays >= 24 && record.skill_vs_no_change > 0.0 && band_ok {
        Standing::Earned
    } else if record.replays < 24 {
        Standing::NotYet
    } else {
        Standing::Failed
    };
    let headline = match standing {
        Standing::Earned => format!(
            "90% band held {}% of {} monthly replays; {}% better than no change.",
            whole(record.inside_90 * 100.0),
            record.replays,
            whole(record.skill_vs_no_change * 100.0)
        ),
        Standing::NotYet => format!("Needs 24 monthly replays; has {}.", record.replays),
        Standing::Failed if !band_ok => format!(
            "90% band held {}% of {} monthly replays.",
            whole(record.inside_90 * 100.0),
            record.replays
        ),
        Standing::Failed => format!(
            "No better than no change across {} monthly replays.",
            record.replays
        ),
    };
    ScoreRow {
        key: format!("outlook:h{}", record.horizon),
        family: Family::Outlook,
        name: "Blended outlook".to_string(),
        horizon_sessions: Some(record.horizon),
        standing,
        headline,
        evidence: record.replays,
    }
}

fn setup_row(verdict: &Verdict) -> ScoreRow {
    let standing = if verdict.publishes {
        Standing::Earned
    } else if verdict.selection.trades < MIN_TRADES {
        Standing::NotYet
    } else {
        Standing::Failed
    };
    let headline = if standing == Standing::NotYet {
        format!(
            "Needs {} trades; has {}.",
            MIN_TRADES, verdict.selection.trades
        )
    } else {
        verdict.reason.clone()
    };
    let side = verdict.selection.side;
    ScoreRow {
        key: format!(
            "setup:{}:{}:{}",
            setup_key(verdict.selection.setup),
            side_key(side),
            verdict.group
        ),
        family: Family::Setup,
        name: format!(
            "{} · {} · {}",
            setup_name(verdict.selection.setup),
            side_name(side),
            verdict.group
        ),
        horizon_sessions: None,
        standing,
        headline,
        evidence: verdict.selection.trades,
    }
}

fn live_row(totals: &RecordTotals) -> ScoreRow {
    let standing = if totals.closed >= 20 && totals.mean_r > 0.0 {
        Standing::Earned
    } else if totals.closed < 20 {
        Standing::NotYet
    } else {
        Standing::Failed
    };
    let headline = match standing {
        Standing::NotYet => format!("Needs 20 closed signals; has {}.", totals.closed),
        _ => format!(
            "{} closed: hit target 1 in {}%, mean {:+.2}R.",
            totals.closed,
            whole(totals.hit_target1 * 100.0),
            totals.mean_r
        ),
    };
    ScoreRow {
        key: "live".to_string(),
        family: Family::Live,
        name: "Published signals (live)".to_string(),
        horizon_sessions: None,
        standing,
        headline,
        evidence: totals.closed,
    }
}

/// Five-year outcomes needed before the dividend growth estimate is judged.
pub const MIN_DIVIDEND_OUTCOMES: u32 = 100;

/// Round 24: the dividend growth estimate in use (the adopted shrinkage) earns trust when
/// it has at least [`MIN_DIVIDEND_OUTCOMES`] five-year outcomes, its bias is within two
/// percentage points a year, and its low–high band holds 55–80% of outcomes (one σ either
/// side would hold about 68%).
fn dividend_growth_row(report: &crate::dividends::calibrate::CalibrationReport) -> ScoreRow {
    let row = report
        .shrink
        .iter()
        .find(|r| r.k == report.adopted_k)
        .or_else(|| report.shrink.first());
    let (n, bias, coverage, mae) = row.map_or((0, 0.0, 0.0, 0.0), |r| {
        (r.n_5y, r.bias_5y, r.band_coverage_5y, r.mae_5y)
    });
    let standing = if n < MIN_DIVIDEND_OUTCOMES {
        Standing::NotYet
    } else if bias.abs() <= 0.02 && (0.55..=0.80).contains(&coverage) {
        Standing::Earned
    } else {
        Standing::Failed
    };
    let headline = match standing {
        Standing::NotYet => {
            format!("Needs {MIN_DIVIDEND_OUTCOMES} five-year payout outcomes; has {n}.")
        }
        _ => format!(
            "Off by {:.1} points a year on average (bias {:+.1}); the low–high band held {}% of outcomes.",
            mae * 100.0,
            bias * 100.0,
            whole(coverage * 100.0)
        ),
    };
    ScoreRow {
        key: "dividend_growth_estimator".to_string(),
        family: Family::Income,
        name: "Dividend growth estimate".to_string(),
        horizon_sessions: None,
        standing,
        headline,
        evidence: n,
    }
}

/// A percentage rounded to a whole number.
fn whole(value: f64) -> i64 {
    (value).round() as i64
}

/// An integer with thousands separators.
fn thousands(value: u32) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn model_name(model: ModelId) -> &'static str {
    match model {
        ModelId::NoChange => "No change",
        ModelId::TrailingDrift => "Trailing drift",
        ModelId::EwmaVolatility => "EWMA volatility",
        ModelId::EmpiricalHorizon => "Past returns",
    }
}

fn model_key(model: ModelId) -> &'static str {
    match model {
        ModelId::NoChange => "no_change",
        ModelId::TrailingDrift => "trailing_drift",
        ModelId::EwmaVolatility => "ewma_volatility",
        ModelId::EmpiricalHorizon => "empirical_horizon",
    }
}

fn signal_name(signal: SignalId) -> &'static str {
    match signal {
        SignalId::Momentum12_1 => "12-1 momentum",
        SignalId::ShortTermReversal => "Short-term reversal",
        SignalId::LowVolatility => "Low volatility",
        SignalId::Trend => "Trend",
        SignalId::ChartPattern => "Chart patterns (YOLO)",
        SignalId::ChartTrend => "Chart trend (YOLO)",
        SignalId::FoundationForecast => "Foundation model (Chronos)",
        SignalId::FilingSentiment => "Filing sentiment",
        SignalId::InsiderBuying => "Insider buying",
    }
}

fn signal_key(signal: SignalId) -> &'static str {
    match signal {
        SignalId::Momentum12_1 => "momentum12_1",
        SignalId::ShortTermReversal => "short_term_reversal",
        SignalId::LowVolatility => "low_volatility",
        SignalId::Trend => "trend",
        SignalId::ChartPattern => "chart_pattern",
        SignalId::ChartTrend => "chart_trend",
        SignalId::FoundationForecast => "foundation_forecast",
        SignalId::FilingSentiment => "filing_sentiment",
        SignalId::InsiderBuying => "insider_buying",
    }
}

fn setup_name(setup: SetupId) -> &'static str {
    match setup {
        SetupId::TrendPullback => "Trend pullback",
        SetupId::SqueezeBreakout => "Squeeze breakout",
        SetupId::Donchian55 => "55-day breakout",
        SetupId::BollingerReversion => "Bollinger reversion",
        SetupId::InsideBarBreak => "Inside-bar break",
        SetupId::ThreeDayReversal => "Three-day reversal",
        SetupId::EarningsGapDrift => "Earnings gap drift",
        SetupId::VolumeBreakout => "Volume breakout",
    }
}

fn setup_key(setup: SetupId) -> &'static str {
    match setup {
        SetupId::TrendPullback => "trend_pullback",
        SetupId::SqueezeBreakout => "squeeze_breakout",
        SetupId::Donchian55 => "donchian55",
        SetupId::BollingerReversion => "bollinger_reversion",
        SetupId::InsideBarBreak => "inside_bar_break",
        SetupId::ThreeDayReversal => "three_day_reversal",
        SetupId::EarningsGapDrift => "earnings_gap_drift",
        SetupId::VolumeBreakout => "volume_breakout",
    }
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Long => "long",
        Side::Short => "short",
    }
}

fn side_key(side: Side) -> &'static str {
    match side {
        Side::Long => "long",
        Side::Short => "short",
    }
}
