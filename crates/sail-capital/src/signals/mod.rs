//! Signals and their combination (round 11, CAP-16/18, Kilo's slice).
//!
//! A **signal** turns an instrument's past prices (later: charts, filings, models) into a
//! score that claims to rank future returns. SAIL never trusts a claim. Every signal is
//! scored out of sample by its **information coefficient** (IC): the rank correlation
//! between its scores and the returns that followed. Only signals whose IC is reliably
//! above zero get any weight.
//!
//! **The combination formula** (Grinold–Kahn, applied per horizon `h`):
//!
//! 1. Each signal `i` gives raw values `x_ij` for instruments `j`, standardized across
//!    instruments at the same date: `z_ij = clamp((x_ij − mean_i) / sd_i, −3, 3)`.
//! 2. Its walk-forward IC has mean `IC_i` and standard error `SE_i`. The usable IC is
//!    shrunk toward zero by two standard errors, which also sets the sign:
//!    `IC*_i = sign(IC_i) · max(0, |IC_i| − 2·SE_i)`.
//!    A signal that isn't significant at about 95% contributes nothing.
//! 3. Signals overlap, so their weights account for how their scores correlate. With
//!    `C` the correlation matrix of the `z_i` (pooled over the evaluation, plus `0.1·I`
//!    for stability): `w = C⁻¹ · IC*`.
//! 4. The combined score is `s_j = Σ_i w_i z_ij`, rescaled to unit standard deviation
//!    across instruments. Its expected IC is `IC_c = sqrt(IC*ᵀ C⁻¹ IC*)`.
//! 5. The expected excess log return over `h` (the "alpha") is
//!    `α_j = IC_c · σ_j(h) · s_j`, where `σ_j(h)` is the instrument's `EwmaVolatility`
//!    forecast sigma at that horizon.
//!
//! If no signal survives step 2, every `α_j` is exactly 0. Ideas then reduce to "no
//! change", and the page says that no signal passed validation.
//!
//! **Purity.** No I/O and no clock, like `forecast`.
use crate::forecast::CloseSeries;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Fewest instruments for a cross-section. Below it, nothing is scored or combined.
pub const MIN_CROSS_SECTION: usize = 5;

/// Horizons, in sessions, that signals are scored and combined at.
pub const SIGNAL_HORIZONS: [u32; 2] = [20, 60];

/// The signals. Price signals are computed here. Model signals (chart patterns,
/// foundation forecasters, filing sentiment, insider buying) come from Python helpers in
/// a later round. They enter through [`ExternalScores`] and are scored the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalId {
    /// `ln(P[t−21] / P[t−252])`: the past year's return, skipping the last month.
    Momentum12_1,
    /// `−ln(P[t] / P[t−21])`: last month's loser scores high.
    ShortTermReversal,
    /// `−σ` of the last 252 daily log returns: calmer scores high.
    LowVolatility,
    /// `ln(P[t] / mean(P[t−199..=t]))`: distance above the 200-session average.
    Trend,
    /// Chart patterns found by the YOLOv8 pattern detector (later round).
    ChartPattern,
    /// Up/Down trend found by the YOLOv8 trend detector (round 12).
    ChartTrend,
    /// Median forecast of a time-series foundation model, e.g. Chronos-Bolt (later round).
    FoundationForecast,
    /// Tone of the latest filings, e.g. FinBERT on 8-K text (later round).
    FilingSentiment,
    /// Net insider buying from SEC Form 4 (later round).
    InsiderBuying,
}

impl SignalId {
    /// The signals computed from prices in this module, in report order.
    pub const PRICE: [SignalId; 4] = [
        SignalId::Momentum12_1,
        SignalId::ShortTermReversal,
        SignalId::LowVolatility,
        SignalId::Trend,
    ];

    /// Closes needed before the signal has a value.
    pub fn min_closes(self) -> usize {
        match self {
            SignalId::Momentum12_1 | SignalId::LowVolatility => 253,
            SignalId::ShortTermReversal => 22,
            SignalId::Trend => 200,
            _ => 0,
        }
    }
}

/// Raw values of one signal from outside this module (a Python helper), for
/// `instrument_key` at `session`. Higher means "expected to do better".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExternalScores {
    /// Which signal.
    pub signal: SignalId,
    /// `(instrument_key, session, raw value)`.
    pub values: Vec<(String, NaiveDate, f64)>,
}

/// The raw value of a price signal for the series cut at its last close, or `None` when
/// there are fewer than [`SignalId::min_closes`] closes or a value isn't finite.
/// Returns `None` for non-price signals.
pub fn price_signal(series: &CloseSeries<'_>, signal: SignalId) -> Option<f64> {
    // Kilo implements this; the stub keeps the crate building.
    let _ = (series, signal);
    None
}

/// Walk-forward skill of one signal at one horizon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignalSkill {
    /// The signal.
    pub signal: SignalId,
    /// Horizon in sessions.
    pub horizon_sessions: u32,
    /// Cross-sections scored (origins with at least [`MIN_CROSS_SECTION`] instruments).
    pub origins: u32,
    /// Mean Spearman IC across origins.
    pub ic_mean: f64,
    /// Standard error of the mean IC, widened for overlapping outcomes: `sd(IC) /
    /// sqrt(origins / max(1, h / step))`.
    pub ic_se: f64,
    /// `ic_mean / ic_se`.
    pub t_stat: f64,
    /// `IC*`, the shrunk IC used in the combination.
    pub ic_used: f64,
    /// The weight `w_i` in the combination (0 when the signal didn't survive).
    pub weight: f64,
}

/// Walk-forward settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignalEvalConfig {
    /// Horizons to score.
    pub horizons: Vec<u32>,
    /// Calendar days between cross-section dates.
    pub step_days: u32,
    /// Earliest date to score from (`None`: as soon as five instruments have values).
    pub start: Option<NaiveDate>,
}

impl Default for SignalEvalConfig {
    fn default() -> Self {
        Self {
            horizons: SIGNAL_HORIZONS.to_vec(),
            step_days: 7,
            start: None,
        }
    }
}

/// One instrument's combined view at one horizon, at the latest date.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CombinedScore {
    /// Ledger instrument key.
    pub instrument_key: String,
    /// Broker symbol.
    pub symbol: String,
    /// Horizon in sessions.
    pub horizon_sessions: u32,
    /// Standardized score per signal (only signals with a value).
    pub z: Vec<(SignalId, f64)>,
    /// Combined score `s_j` (unit standard deviation across instruments).
    pub combined: f64,
    /// `α_j`: expected excess log return over the horizon. 0 when nothing survived.
    pub alpha: f64,
    /// `σ_j(h)` used.
    pub sigma: f64,
}

/// The signal report: skill per signal and horizon, and the latest combined scores.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignalReport {
    /// Settings used.
    pub config: SignalEvalConfig,
    /// Skill, by horizon then signal.
    pub skill: Vec<SignalSkill>,
    /// `IC_c` per horizon (0 when nothing survived).
    pub combined_ic: Vec<(u32, f64)>,
    /// Latest combined scores, by horizon then symbol.
    pub latest: Vec<CombinedScore>,
    /// Plain-language notes: what passed, what didn't, and the limits (overlap,
    /// survivorship, a small universe, no costs).
    pub notes: Vec<String>,
}

/// Scores every price signal (plus any `external` scores) walk-forward over `series`,
/// then combines the survivors at the latest date by the formula above. A cross-section
/// at date `d` uses each instrument's last close on or before `d`. Its outcome is the log
/// return over the next `h` sessions of that instrument. Instruments without that much
/// future are left out of that cross-section. `sigma` gives `σ_j(h)` per instrument key
/// and horizon at the latest date (from `forecast::forecast` with `EwmaVolatility`).
pub fn evaluate_and_combine(
    series: &[CloseSeries<'_>],
    external: &[ExternalScores],
    sigma: &dyn Fn(&str, u32) -> Option<f64>,
    config: &SignalEvalConfig,
) -> SignalReport {
    // Kilo implements this; the stub keeps the crate building.
    let _ = (series, external, sigma);
    SignalReport {
        config: config.clone(),
        skill: Vec::new(),
        combined_ic: Vec::new(),
        latest: Vec::new(),
        notes: Vec::new(),
    }
}
