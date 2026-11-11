//! Forecast baselines and their evaluation (round 10, CAP-16/17, Kilo's slice).
//!
//! The EPIC requires simple, honest baselines with proper scoring before any neural model
//! (`.sea`). This module provides both:
//! - the EPIC's `ForecastDistribution` for a few fixed baseline models;
//! - a walk-forward evaluation that scores them out of sample against the no-change
//!   model.
//!
//! **Purity.** No I/O and no clock. The caller passes the close series and the forecast
//! origin.
//!
//! **Returns.** Log returns over a horizon counted in **sessions** (rows of the series),
//! never calendar days.
//!
//! **Floats.** The statistics are `f64`, because they need logs, exponentials and square
//! roots. Prices enter as `Decimal` and are converted once.
//!
//! **Honesty rules** (EPIC A2):
//! - too little data gives `ForecastStatus::InsufficientEvidence`, never a number;
//! - `probability_positive` is published only when the model and horizon passed the
//!   calibration check;
//! - expected and median terminal prices are kept apart (`P0·exp(μ+σ²/2)` vs `P0·exp(μ)`);
//! - nothing here proposes a trade.
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Schema version of [`ForecastDistribution`].
pub const SCHEMA_VERSION: &str = "capital-forecast/1";

/// Horizons forecast and evaluated, in sessions (about a week, a month and a quarter).
pub const HORIZONS: [u32; 3] = [5, 20, 60];

/// Fewest daily log returns a model needs before it forecasts (about a year).
pub const MIN_OBSERVATIONS: usize = 252;

/// Quantile levels reported, always in this order.
pub const QUANTILE_LEVELS: [f64; 5] = [0.05, 0.25, 0.5, 0.75, 0.95];

/// The baselines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelId {
    /// Zero drift; σ from the last 252 daily log returns, scaled by √h. Gaussian.
    NoChange,
    /// Drift = mean daily log return over the last 252, × h; σ as `NoChange`. Gaussian.
    TrailingDrift,
    /// Zero drift; σ from an EWMA of squared daily log returns (λ = 0.94, RiskMetrics),
    /// scaled by √h. Gaussian.
    EwmaVolatility,
    /// The empirical distribution of every past non-overlapping h-session log return in
    /// the last 3 years, needing at least 20 of them. Quantiles are read off directly, and
    /// the mean and σ are sample moments. Not Gaussian.
    EmpiricalHorizon,
}

impl ModelId {
    /// Every baseline, in report order.
    pub const ALL: [ModelId; 4] = [
        ModelId::NoChange,
        ModelId::TrailingDrift,
        ModelId::EwmaVolatility,
        ModelId::EmpiricalHorizon,
    ];
}

/// Whether a forecast could be made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ForecastStatus {
    /// Numbers are present.
    Ok,
    /// Not enough or unusable data; every numeric field is `None`.
    InsufficientEvidence {
        /// Why, in plain words.
        reason: String,
    },
}

/// One quantile of the h-session log return.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Quantile {
    /// Level, e.g. 0.05.
    pub p: f64,
    /// Log return at that level.
    pub log_return: f64,
}

/// Whether a model and horizon passed the calibration check in evaluation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalibrationState {
    /// True only when [`calibration_verified`] held for this model and horizon.
    pub verified: bool,
    /// The evaluation it rests on, e.g. `"walk-forward 2019-01-02..2026-11-10, 1840 forecasts"`.
    pub cohort: Option<String>,
}

/// The EPIC's `ForecastDistribution` for one instrument, model and horizon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForecastDistribution {
    /// [`SCHEMA_VERSION`].
    pub schema_version: String,
    /// Ledger instrument key.
    pub instrument_key: String,
    /// Broker symbol.
    pub symbol: String,
    /// The model.
    pub model: ModelId,
    /// The last session whose close the forecast used.
    pub as_of_session: NaiveDate,
    /// Horizon in sessions.
    pub horizon_sessions: u32,
    /// Close at `as_of_session`.
    #[serde(with = "rust_decimal::serde::str")]
    pub reference_close: Decimal,
    /// Ok or insufficient evidence.
    pub status: ForecastStatus,
    /// Mean log return over the horizon.
    pub expected_log_return: Option<f64>,
    /// Median log return over the horizon.
    pub median_log_return: Option<f64>,
    /// Standard deviation of the horizon log return (same horizon and measure).
    pub sigma_log_return: Option<f64>,
    /// `sigma²`.
    pub variance_log_return: Option<f64>,
    /// At [`QUANTILE_LEVELS`].
    pub quantiles: Vec<Quantile>,
    /// P(log return > 0). Only when `calibration.verified`; else `None`.
    pub probability_positive: Option<f64>,
    /// `P0·exp(μ+σ²/2)` for Gaussian models; `None` for `EmpiricalHorizon`.
    pub expected_terminal_price: Option<f64>,
    /// `P0·exp(median)`.
    pub median_terminal_price: Option<f64>,
    /// Daily log returns used.
    pub observations_used: u32,
    /// Calibration state.
    pub calibration: CalibrationState,
}

/// One forecast origin's input: closes oldest first, sessions strictly increasing.
#[derive(Clone, Debug, PartialEq)]
pub struct CloseSeries<'a> {
    /// Ledger instrument key.
    pub instrument_key: &'a str,
    /// Broker symbol.
    pub symbol: &'a str,
    /// `(session, close)` pairs, oldest first. Closes must be positive.
    pub closes: &'a [(NaiveDate, Decimal)],
}

/// Forecasts `model` at `horizon` sessions from the **last** close in `series`. The
/// caller cuts the series at the origin; the function never looks beyond it.
pub fn forecast(
    series: &CloseSeries<'_>,
    model: ModelId,
    horizon: u32,
    calibration: CalibrationState,
) -> ForecastDistribution {
    // Kilo implements this; the stub keeps the crate building.
    let _ = calibration;
    let (as_of_session, reference_close) = series
        .closes
        .last()
        .cloned()
        .unwrap_or((NaiveDate::MIN, Decimal::ZERO));
    ForecastDistribution {
        schema_version: SCHEMA_VERSION.to_string(),
        instrument_key: series.instrument_key.to_string(),
        symbol: series.symbol.to_string(),
        model,
        as_of_session,
        horizon_sessions: horizon,
        reference_close,
        status: ForecastStatus::InsufficientEvidence {
            reason: "not implemented".into(),
        },
        expected_log_return: None,
        median_log_return: None,
        sigma_log_return: None,
        variance_log_return: None,
        quantiles: Vec::new(),
        probability_positive: None,
        expected_terminal_price: None,
        median_terminal_price: None,
        observations_used: 0,
        calibration: CalibrationState {
            verified: false,
            cohort: None,
        },
    }
}

/// Walk-forward settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvalConfig {
    /// Horizons to score.
    pub horizons: Vec<u32>,
    /// Sessions between forecast origins.
    pub step_sessions: usize,
    /// Fewest daily returns before the first origin ([`MIN_OBSERVATIONS`]).
    pub min_train: usize,
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            horizons: HORIZONS.to_vec(),
            step_sessions: 5,
            min_train: MIN_OBSERVATIONS,
        }
    }
}

/// Observed frequency at one nominal quantile level.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoveragePoint {
    /// Nominal level.
    pub p: f64,
    /// Share of outcomes at or below that quantile.
    pub observed: f64,
}

/// Out-of-sample scores for one model and horizon, pooled over instruments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelScore {
    /// The model.
    pub model: ModelId,
    /// Horizon in sessions.
    pub horizon_sessions: u32,
    /// Scored forecasts.
    pub forecasts: u32,
    /// Mean CRPS of the log-return forecast (lower is better).
    pub crps: f64,
    /// Mean pinball loss averaged over [`QUANTILE_LEVELS`].
    pub pinball: f64,
    /// Mean Gaussian negative log-likelihood; `None` for `EmpiricalHorizon`.
    pub nll: Option<f64>,
    /// `1 - crps / crps(NoChange)` on the same forecasts; 0 for `NoChange` itself.
    pub crps_skill: f64,
    /// Observed coverage at each quantile level.
    pub coverage: Vec<CoveragePoint>,
    /// Share of outcomes inside the 5–95% interval (nominal 0.90).
    pub interval_90: f64,
    /// Share of non-zero outcomes whose sign the median got right.
    pub direction_hit_rate: f64,
    /// Whether [`calibration_verified`] holds.
    pub calibrated: bool,
}

/// Cross-sectional rank information coefficient for one model and horizon: the mean,
/// across origins with at least 5 instruments, of the Spearman correlation between
/// forecast mean and realized log return.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RankIc {
    /// The model.
    pub model: ModelId,
    /// Horizon in sessions.
    pub horizon_sessions: u32,
    /// Mean rank IC.
    pub mean: f64,
    /// Origins averaged.
    pub origins: u32,
}

/// The evaluation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    /// Settings used.
    pub config: EvalConfig,
    /// Instruments with enough data, by symbol.
    pub instruments: Vec<String>,
    /// Instruments skipped, with why.
    pub skipped: Vec<(String, String)>,
    /// First and last forecast origin.
    pub origins: Option<(NaiveDate, NaiveDate)>,
    /// Scores, by horizon then [`ModelId::ALL`] order.
    pub scores: Vec<ModelScore>,
    /// Rank IC; empty when fewer than 5 instruments.
    pub rank_ic: Vec<RankIc>,
    /// Plain-language notes: what the scores mean, and their limits (survivorship, no
    /// costs, a short history).
    pub notes: Vec<String>,
}

/// The calibration check: at least 100 forecasts, the 5–95% interval covers 0.85..=0.95
/// of outcomes, and the observed share below the median is within 0.42..=0.58.
pub fn calibration_verified(score: &ModelScore) -> bool {
    let median = score
        .coverage
        .iter()
        .find(|c| (c.p - 0.5).abs() < 1e-9)
        .map(|c| c.observed);
    score.forecasts >= 100
        && (0.85..=0.95).contains(&score.interval_90)
        && median.is_some_and(|m| (0.42..=0.58).contains(&m))
}

/// Walk-forward evaluation of every baseline over `series`. Origins step by
/// `step_sessions` from `min_train`. Each forecast uses only closes up to its origin.
/// Its outcome is the log return from the origin to `origin + h` (skipped if beyond the
/// series). Origins of one instrument are `step_sessions` apart, so h-session outcomes
/// overlap when `h > step`; the scores are means and the notes say so.
pub fn evaluate(series: &[CloseSeries<'_>], config: &EvalConfig) -> EvalReport {
    // Kilo implements this; the stub keeps the crate building.
    let _ = series;
    EvalReport {
        config: config.clone(),
        instruments: Vec::new(),
        skipped: Vec::new(),
        origins: None,
        scores: Vec::new(),
        rank_ic: Vec::new(),
        notes: Vec::new(),
    }
}
