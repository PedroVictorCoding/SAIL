//! The fixed baseline models behind [`crate::forecast::forecast`].
//!
//! Everything here works on one point-in-time cut of a close series: a prepared series
//! never contains a row past its origin. All prices are converted to `f64` once, when the
//! series is prepared, because the models need logarithms and square roots.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use super::stats::{mean, normal_cdf, normal_quantile, quantile_type7, sample_std};
use super::{MIN_OBSERVATIONS, ModelId, QUANTILE_LEVELS};

/// Daily returns used for the trailing mean and spread.
const VOL_WINDOW: usize = 252;

/// RiskMetrics decay for the EWMA variance.
const EWMA_LAMBDA: f64 = 0.94;

/// About three years of sessions, the window for the empirical h-session sample. It is
/// widened at long horizons so at least [`MIN_EMPIRICAL_SAMPLES`] samples still fit.
const EMPIRICAL_LOOKBACK_SESSIONS: usize = 756;

/// Fewest non-overlapping h-session returns the empirical model accepts.
const MIN_EMPIRICAL_SAMPLES: usize = 20;

/// One series, converted once and enriched with the running sums the models reuse.
///
/// `log_closes[i]` is `ln(close_i)`; `returns[k]` is `log_closes[k+1] - log_closes[k]`.
/// The prefix arrays have `returns.len() + 1` entries, so a window `[start, end)` of
/// returns is `prefix[end] - prefix[start]`.
pub(crate) struct PreparedSeries {
    /// Closes oldest first, as stored.
    pub(crate) closes: Vec<(NaiveDate, Decimal)>,
    /// Closes as `f64`, for prices formed from a log return.
    pub(crate) closes_f64: Vec<f64>,
    /// `ln(close_i)`.
    pub(crate) log_closes: Vec<f64>,
    /// Daily log returns.
    pub(crate) returns: Vec<f64>,
    /// Running sum of daily log returns.
    return_prefix: Vec<f64>,
    /// Running sum of squared daily log returns.
    squared_prefix: Vec<f64>,
    /// EWMA variance of daily log returns at each return index.
    ewma_variance: Vec<f64>,
}

impl PreparedSeries {
    /// Converts and validates one series. The only error is a reason, in plain words,
    /// that is safe to show.
    pub(crate) fn new(closes: &[(NaiveDate, Decimal)]) -> Result<Self, String> {
        if closes.is_empty() {
            return Err("there are no closes".into());
        }
        let mut closes_f64 = Vec::with_capacity(closes.len());
        let mut log_closes = Vec::with_capacity(closes.len());
        for (_, close) in closes {
            let value = close
                .to_f64()
                .ok_or_else(|| "a close could not be read as a number".to_string())?;
            if !value.is_finite() || value <= 0.0 {
                return Err("a close was not a positive finite number".into());
            }
            let log = value.ln();
            if !log.is_finite() {
                return Err("a close was not a positive finite number".into());
            }
            closes_f64.push(value);
            log_closes.push(log);
        }

        let mut returns = Vec::with_capacity(closes.len().saturating_sub(1));
        for pair in log_closes.windows(2) {
            let value = pair[1] - pair[0];
            if !value.is_finite() {
                return Err("a daily return was not finite".into());
            }
            returns.push(value);
        }

        let mut return_prefix = Vec::with_capacity(returns.len() + 1);
        let mut squared_prefix = Vec::with_capacity(returns.len() + 1);
        return_prefix.push(0.0);
        squared_prefix.push(0.0);
        for value in &returns {
            return_prefix.push(return_prefix[return_prefix.len() - 1] + value);
            squared_prefix.push(squared_prefix[squared_prefix.len() - 1] + value * value);
        }

        let mut ewma_variance = Vec::with_capacity(returns.len());
        if let Some(first) = returns.first() {
            let mut variance = first * first;
            ewma_variance.push(variance);
            for value in &returns[1..] {
                variance = EWMA_LAMBDA * variance + (1.0 - EWMA_LAMBDA) * value * value;
                ewma_variance.push(variance);
            }
        }

        Ok(Self {
            closes: closes.to_vec(),
            closes_f64,
            log_closes,
            returns,
            return_prefix,
            squared_prefix,
            ewma_variance,
        })
    }
}

/// A model's numbers for one origin and horizon, before they are dressed as a
/// `ForecastDistribution`. `samples` is present only for the empirical model, where the
/// evaluation needs it to compute CRPS.
pub(crate) struct ModelForecast {
    pub(crate) expected_log_return: f64,
    pub(crate) median_log_return: f64,
    pub(crate) sigma_log_return: f64,
    pub(crate) quantiles: [f64; 5],
    pub(crate) probability_positive: Option<f64>,
    pub(crate) expected_terminal_price: Option<f64>,
    pub(crate) median_terminal_price: f64,
    pub(crate) observations_used: u32,
    pub(crate) samples: Option<Vec<f64>>,
}

/// Forecasts `model` at `horizon` from the cut series ending at `origin`.
///
/// `verified` only controls whether `probability_positive` is published; the calibration
/// object itself is passed through by the caller.
pub(crate) fn model_forecast(
    series: &PreparedSeries,
    origin: usize,
    model: ModelId,
    horizon: u32,
    verified: bool,
) -> Result<ModelForecast, String> {
    if horizon == 0 {
        return Err("the horizon must be at least one session".into());
    }
    match model {
        ModelId::EmpiricalHorizon => empirical_forecast(series, origin, horizon, verified),
        ModelId::NoChange | ModelId::TrailingDrift | ModelId::EwmaVolatility => {
            gaussian_forecast(series, origin, model, horizon, verified)
        }
    }
}

fn gaussian_forecast(
    series: &PreparedSeries,
    origin: usize,
    model: ModelId,
    horizon: u32,
    verified: bool,
) -> Result<ModelForecast, String> {
    let available = origin.min(series.returns.len());
    if available < MIN_OBSERVATIONS {
        return Err(format!(
            "needs at least {MIN_OBSERVATIONS} daily returns; has {available}"
        ));
    }
    let h = horizon as f64;
    let (mu_daily, sigma_daily, observations_used) = match model {
        ModelId::EwmaVolatility => {
            let variance = series.ewma_variance[available - 1].max(0.0);
            (0.0, variance.sqrt(), available)
        }
        _ => {
            let used = available.min(VOL_WINDOW);
            let start = available - used;
            let sum = series.return_prefix[available] - series.return_prefix[start];
            let squared = series.squared_prefix[available] - series.squared_prefix[start];
            let mean_daily = sum / used as f64;
            let variance = ((squared - sum * sum / used as f64) / (used - 1) as f64).max(0.0);
            let mu_daily = if model == ModelId::TrailingDrift {
                mean_daily
            } else {
                0.0
            };
            (mu_daily, variance.sqrt(), used)
        }
    };

    let p0 = series.closes_f64[origin];
    let mu = mu_daily * h;
    let sigma = sigma_daily * h.sqrt();
    let quantiles = QUANTILE_LEVELS.map(|p| mu + sigma * normal_quantile(p));
    let probability_positive = verified.then(|| gaussian_probability_positive(mu, sigma));

    Ok(ModelForecast {
        expected_log_return: mu,
        median_log_return: mu,
        sigma_log_return: sigma,
        quantiles,
        probability_positive,
        expected_terminal_price: Some(p0 * (mu + 0.5 * sigma * sigma).exp()),
        median_terminal_price: p0 * mu.exp(),
        observations_used: observations_used.min(u32::MAX as usize) as u32,
        samples: None,
    })
}

fn empirical_forecast(
    series: &PreparedSeries,
    origin: usize,
    horizon: u32,
    verified: bool,
) -> Result<ModelForecast, String> {
    if origin < MIN_OBSERVATIONS {
        let available = origin.min(series.returns.len());
        return Err(format!(
            "needs at least {MIN_OBSERVATIONS} daily returns; has {available}"
        ));
    }
    let h = horizon as usize;
    let last = series.log_closes.len().saturating_sub(1).min(origin);
    if h > last {
        return Err(format!(
            "needs a {horizon}-session history before the origin; has {last} sessions"
        ));
    }
    let last_start = last - h;
    let lookback = EMPIRICAL_LOOKBACK_SESSIONS.max(MIN_EMPIRICAL_SAMPLES.saturating_mul(h));
    let window_start = last.saturating_sub(lookback);
    let mut samples = Vec::new();
    let mut start = last_start;
    loop {
        if start < window_start {
            break;
        }
        samples.push(series.log_closes[start + h] - series.log_closes[start]);
        if start < h {
            break;
        }
        start -= h;
    }
    if samples.len() < MIN_EMPIRICAL_SAMPLES {
        return Err(format!(
            "needs at least {MIN_EMPIRICAL_SAMPLES} non-overlapping {horizon}-session returns in the available history; has {}",
            samples.len()
        ));
    }
    samples.sort_by(f64::total_cmp);
    let mean = mean(&samples);
    let sigma = sample_std(&samples, mean);
    let quantiles = QUANTILE_LEVELS.map(|p| quantile_type7(&samples, p));
    let median = quantile_type7(&samples, 0.5);
    let positive =
        samples.iter().filter(|value| **value > 0.0).count() as f64 / samples.len() as f64;
    let probability_positive = verified.then_some(positive);
    let p0 = series.closes_f64[origin];

    Ok(ModelForecast {
        expected_log_return: mean,
        median_log_return: median,
        sigma_log_return: sigma,
        quantiles,
        probability_positive,
        expected_terminal_price: None,
        median_terminal_price: p0 * median.exp(),
        observations_used: (last - window_start).min(u32::MAX as usize) as u32,
        samples: Some(samples),
    })
}

fn gaussian_probability_positive(mu: f64, sigma: f64) -> f64 {
    if sigma > 0.0 {
        normal_cdf(mu / sigma)
    } else if mu > 0.0 {
        1.0
    } else if mu < 0.0 {
        0.0
    } else {
        0.5
    }
}
