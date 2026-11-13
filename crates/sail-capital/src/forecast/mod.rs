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
pub mod baseline;
mod model;
pub(crate) mod stats;

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use self::stats::{empirical_crps, gaussian_crps, gaussian_nll, mean, pinball_loss, spearman};

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
    /// about the last 3 years, widened at long horizons so at least 20 samples fit.
    /// Needs at least 20 of them. Quantiles are read off directly, and the mean and σ are
    /// sample moments. Not Gaussian.
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
    /// P(log return > 0). Only when `calibration.verified`, and only for models that
    /// estimate drift (`TrailingDrift`, `EmpiricalHorizon`); else `None`.
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
    let (as_of_session, reference_close) = series
        .closes
        .last()
        .cloned()
        .unwrap_or((NaiveDate::MIN, Decimal::ZERO));
    let prepared = match model::PreparedSeries::new(series.closes) {
        Ok(prepared) => prepared,
        Err(reason) => {
            return insufficient_evidence(
                series,
                model,
                horizon,
                as_of_session,
                reference_close,
                calibration,
                reason,
            );
        }
    };
    let origin = prepared.closes.len() - 1;
    match model::model_forecast(&prepared, origin, model, horizon, calibration.verified) {
        Ok(numbers) => ForecastDistribution {
            schema_version: SCHEMA_VERSION.to_string(),
            instrument_key: series.instrument_key.to_string(),
            symbol: series.symbol.to_string(),
            model,
            as_of_session,
            horizon_sessions: horizon,
            reference_close,
            status: ForecastStatus::Ok,
            expected_log_return: Some(numbers.expected_log_return),
            median_log_return: Some(numbers.median_log_return),
            sigma_log_return: Some(numbers.sigma_log_return),
            variance_log_return: Some(numbers.sigma_log_return * numbers.sigma_log_return),
            quantiles: QUANTILE_LEVELS
                .iter()
                .zip(numbers.quantiles)
                .map(|(p, log_return)| Quantile { p: *p, log_return })
                .collect(),
            probability_positive: numbers.probability_positive,
            expected_terminal_price: numbers.expected_terminal_price,
            median_terminal_price: Some(numbers.median_terminal_price),
            observations_used: numbers.observations_used,
            calibration,
        },
        Err(reason) => insufficient_evidence(
            series,
            model,
            horizon,
            as_of_session,
            reference_close,
            calibration,
            reason,
        ),
    }
}

fn insufficient_evidence(
    series: &CloseSeries<'_>,
    model: ModelId,
    horizon: u32,
    as_of_session: NaiveDate,
    reference_close: Decimal,
    calibration: CalibrationState,
    reason: String,
) -> ForecastDistribution {
    ForecastDistribution {
        schema_version: SCHEMA_VERSION.to_string(),
        instrument_key: series.instrument_key.to_string(),
        symbol: series.symbol.to_string(),
        model,
        as_of_session,
        horizon_sessions: horizon,
        reference_close,
        status: ForecastStatus::InsufficientEvidence { reason },
        expected_log_return: None,
        median_log_return: None,
        sigma_log_return: None,
        variance_log_return: None,
        quantiles: Vec::new(),
        probability_positive: None,
        expected_terminal_price: None,
        median_terminal_price: None,
        observations_used: 0,
        calibration,
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
    let longest_horizon = config.horizons.iter().copied().max().unwrap_or(0) as usize;
    let required_sessions = config
        .min_train
        .saturating_add(longest_horizon)
        .saturating_add(1);

    let mut prepared: Vec<(String, model::PreparedSeries)> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    for instrument in series {
        match model::PreparedSeries::new(instrument.closes) {
            Ok(cut) if cut.closes.len() >= required_sessions => {
                prepared.push((instrument.symbol.to_string(), cut));
            }
            Ok(cut) => skipped.push((
                instrument.symbol.to_string(),
                format!(
                    "needs at least {required_sessions} sessions ({} training returns and a {longest_horizon}-session outcome); has {}",
                    config.min_train,
                    cut.closes.len()
                ),
            )),
            Err(reason) => skipped.push((instrument.symbol.to_string(), reason)),
        }
    }

    let instruments: Vec<String> = prepared.iter().map(|(symbol, _)| symbol.clone()).collect();
    let step = config.step_sessions.max(1);
    let mut origin_range: Option<(NaiveDate, NaiveDate)> = None;
    let mut sparse_origins = 0usize;
    let mut scores: Vec<ModelScore> = Vec::new();
    let mut rank_ic: Vec<RankIc> = Vec::new();

    for &horizon in &config.horizons {
        let horizon_sessions = horizon as usize;
        if horizon_sessions == 0 {
            continue;
        }
        let mut observations: Vec<Vec<Observation>> =
            (0..ModelId::ALL.len()).map(|_| Vec::new()).collect();

        for (instrument_index, (_, cut)) in prepared.iter().enumerate() {
            let mut origin = config.min_train;
            while origin + horizon_sessions < cut.closes.len() {
                let outcome_days =
                    (cut.closes[origin + horizon_sessions].0 - cut.closes[origin].0).num_days();
                if outcome_days > model::max_outcome_days(horizon_sessions)
                    || model::sparse_reason(cut, origin).is_some()
                {
                    sparse_origins += 1;
                    origin += step;
                    continue;
                }
                for (model_index, model) in ModelId::ALL.iter().enumerate() {
                    let Ok(numbers) = model::model_forecast(cut, origin, *model, horizon, false)
                    else {
                        continue;
                    };
                    let outcome =
                        cut.log_closes[origin + horizon_sessions] - cut.log_closes[origin];
                    let crps = match numbers.samples.as_deref() {
                        Some(samples) => empirical_crps(samples, outcome),
                        None => gaussian_crps(
                            numbers.expected_log_return,
                            numbers.sigma_log_return,
                            outcome,
                        ),
                    };
                    let pinball = QUANTILE_LEVELS
                        .iter()
                        .zip(numbers.quantiles.iter())
                        .map(|(p, quantile)| pinball_loss(*p, *quantile, outcome))
                        .sum::<f64>()
                        / QUANTILE_LEVELS.len() as f64;
                    let nll = (*model != ModelId::EmpiricalHorizon).then(|| {
                        gaussian_nll(
                            numbers.expected_log_return,
                            numbers.sigma_log_return,
                            outcome,
                        )
                    });
                    let date = cut.closes[origin].0;
                    origin_range = Some(match origin_range {
                        None => (date, date),
                        Some((first, last)) => (first.min(date), last.max(date)),
                    });
                    observations[model_index].push(Observation {
                        key: (instrument_index, origin),
                        date,
                        forecast_mean: numbers.expected_log_return,
                        outcome,
                        quantiles: numbers.quantiles,
                        crps,
                        pinball,
                        nll,
                    });
                }
                origin += step;
            }
        }

        let base_crps: BTreeMap<(usize, usize), f64> = observations
            .first()
            .map(|rows| rows.iter().map(|row| (row.key, row.crps)).collect())
            .unwrap_or_default();

        for (model_index, model) in ModelId::ALL.iter().enumerate() {
            let rows = &observations[model_index];
            if rows.is_empty() {
                continue;
            }
            let count = rows.len() as f64;
            let crps = rows.iter().map(|row| row.crps).sum::<f64>() / count;
            let pinball = rows.iter().map(|row| row.pinball).sum::<f64>() / count;
            let nll_values: Vec<f64> = rows.iter().filter_map(|row| row.nll).collect();
            let nll = (!nll_values.is_empty()).then(|| mean(&nll_values));
            let coverage: Vec<CoveragePoint> = QUANTILE_LEVELS
                .iter()
                .enumerate()
                .map(|(level, p)| {
                    let observed = rows
                        .iter()
                        .filter(|row| row.outcome <= row.quantiles[level])
                        .count() as f64
                        / count;
                    CoveragePoint { p: *p, observed }
                })
                .collect();
            let interval_90 = rows
                .iter()
                .filter(|row| row.outcome >= row.quantiles[0] && row.outcome <= row.quantiles[4])
                .count() as f64
                / count;
            let mut hits = 0usize;
            let mut non_zero = 0usize;
            for row in rows {
                if row.outcome == 0.0 {
                    continue;
                }
                non_zero += 1;
                if row.quantiles[2].signum() == row.outcome.signum() {
                    hits += 1;
                }
            }
            let direction_hit_rate = if non_zero == 0 {
                0.0
            } else {
                hits as f64 / non_zero as f64
            };
            let crps_skill = if *model == ModelId::NoChange {
                0.0
            } else {
                let mut model_sum = 0.0;
                let mut base_sum = 0.0;
                let mut paired = 0usize;
                for row in rows {
                    if let Some(base) = base_crps.get(&row.key) {
                        model_sum += row.crps;
                        base_sum += *base;
                        paired += 1;
                    }
                }
                if paired > 0 && base_sum > 0.0 {
                    1.0 - model_sum / base_sum
                } else {
                    0.0
                }
            };
            let mut score = ModelScore {
                model: *model,
                horizon_sessions: horizon,
                forecasts: rows.len() as u32,
                crps,
                pinball,
                nll,
                crps_skill,
                coverage,
                interval_90,
                direction_hit_rate,
                calibrated: false,
            };
            score.calibrated = calibration_verified(&score);
            scores.push(score);

            if prepared.len() >= 5 {
                let mut by_date: BTreeMap<NaiveDate, Vec<(f64, f64)>> = BTreeMap::new();
                for row in rows {
                    by_date
                        .entry(row.date)
                        .or_default()
                        .push((row.forecast_mean, row.outcome));
                }
                let mut sum = 0.0;
                let mut origins = 0u32;
                for pairs in by_date.values() {
                    if pairs.len() < 5 {
                        continue;
                    }
                    if let Some(rho) = spearman(pairs) {
                        sum += rho;
                        origins += 1;
                    }
                }
                if origins > 0 {
                    rank_ic.push(RankIc {
                        model: *model,
                        horizon_sessions: horizon,
                        mean: sum / origins as f64,
                        origins,
                    });
                }
            }
        }
    }

    EvalReport {
        config: config.clone(),
        instruments,
        skipped,
        origins: origin_range,
        scores,
        rank_ic,
        notes: {
            let mut notes =
                evaluation_notes(config, step, longest_horizon, &prepared, origin_range);
            if sparse_origins > 0 {
                notes.push(format!(
                    "{sparse_origins} forecast origins were not scored because the price source is missing trading days around them."
                ));
            }
            notes
        },
    }
}

/// One scored forecast, kept only long enough to aggregate into a [`ModelScore`].
struct Observation {
    /// `(instrument index, origin index)`, used to pair a model with `NoChange`.
    key: (usize, usize),
    /// The origin's session date.
    date: NaiveDate,
    /// Forecast mean log return.
    forecast_mean: f64,
    /// Realized log return over the horizon.
    outcome: f64,
    /// Forecast quantiles at [`QUANTILE_LEVELS`].
    quantiles: [f64; 5],
    /// Gaussian CRPS or the empirical sample form.
    crps: f64,
    /// Mean pinball loss over the five levels.
    pinball: f64,
    /// Gaussian NLL, absent for the empirical model.
    nll: Option<f64>,
}

fn evaluation_notes(
    config: &EvalConfig,
    step: usize,
    longest_horizon: usize,
    prepared: &[(String, model::PreparedSeries)],
    origins: Option<(NaiveDate, NaiveDate)>,
) -> Vec<String> {
    let mut notes = vec![
        format!(
            "Each forecast uses only closes before its origin; the first {} daily returns of a series are history, not scored.",
            config.min_train
        ),
        "Scores measure log-return forecasts before spreads, fees, taxes and FX; they are not portfolio returns and they are not recommendations.".into(),
        "Only currently held instruments with stored prices are scored, so the sample is survivorship-biased and not a market-wide test.".into(),
    ];
    for &horizon in &config.horizons {
        if horizon as usize > step {
            notes.push(format!(
                "{horizon}-session outcomes overlap because origins are {step} sessions apart; the scores average overlapping windows rather than independent trials."
            ));
        }
    }
    let total_sessions: usize = prepared.iter().map(|(_, cut)| cut.closes.len()).sum();
    let scored_sessions: usize = prepared
        .iter()
        .map(|(_, cut)| cut.closes.len().saturating_sub(config.min_train))
        .sum();
    if total_sessions > 0 {
        notes.push(format!(
            "The first {} daily returns of every series are training history; about {:.0}% of the stored sessions follow an origin and are scored out of sample.",
            config.min_train,
            100.0 * scored_sessions as f64 / total_sessions as f64
        ));
    }
    if let Some((first, last)) = origins {
        notes.push(format!("Out-of-sample origins span {first}..{last}."));
    } else if longest_horizon > 0 {
        notes.push("No origin had a complete outcome, so there is nothing to score.".into());
    }
    if prepared.len() < 5 {
        notes.push(
            "Rank IC needs at least five instruments at a shared origin; it is omitted.".into(),
        );
    }
    notes
}
