//! The built-in component paths (`gbm`, `ewma`, `historical`) and the interpolation of
//! helper paths for `chronos` and `gbdt`.
//!
//! Everything is computed from a series cut at an origin index; rows after the origin are
//! never read. The only exception is the caller's deliberate replay of actual closes.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use crate::forecast::stats::{mean, normal_quantile, quantile_type7, sample_std};

use super::{ComponentId, ExternalPath, LEVELS, PathQuantiles, steps};

const VOL_WINDOW: usize = 252;
const EWMA_LAMBDA: f64 = 0.94;
const HISTORY_WINDOW: usize = 756;
const MIN_HISTORY_SAMPLES: usize = 2;

/// A validated close series, converted once to `f64`, with the running quantities the
/// components reuse.
pub(crate) struct Series {
    pub(crate) dates: Vec<NaiveDate>,
    pub(crate) closes: Vec<f64>,
    pub(crate) log_closes: Vec<f64>,
    pub(crate) returns: Vec<f64>,
    pub(crate) ewma_variance: Vec<f64>,
}

impl Series {
    pub(crate) fn new(closes: &[(NaiveDate, Decimal)]) -> Result<Self, String> {
        if closes.is_empty() {
            return Err("there are no closes".into());
        }
        let mut dates = Vec::with_capacity(closes.len());
        let mut values = Vec::with_capacity(closes.len());
        let mut log_closes = Vec::with_capacity(closes.len());
        for (date, close) in closes {
            let value = close
                .to_f64()
                .ok_or_else(|| "a close could not be read as a number".to_string())?;
            if !value.is_finite() || value <= 0.0 {
                return Err("a close was not a positive finite number".into());
            }
            dates.push(*date);
            values.push(value);
            log_closes.push(value.ln());
        }
        let mut returns = Vec::with_capacity(closes.len().saturating_sub(1));
        for pair in log_closes.windows(2) {
            let value = pair[1] - pair[0];
            if !value.is_finite() {
                return Err("a daily return was not finite".into());
            }
            returns.push(value);
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
            dates,
            closes: values,
            log_closes,
            returns,
            ewma_variance,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.closes.len()
    }

    pub(crate) fn log_return(&self, start: usize, end: usize) -> f64 {
        self.log_closes[end] - self.log_closes[start]
    }
}

fn gbm_quantiles(returns: &[f64], available: usize, horizon: u32) -> Option<[f64; 5]> {
    if available < VOL_WINDOW {
        return None;
    }
    let start = available - VOL_WINDOW;
    let window = &returns[start..available];
    let mu = mean(window);
    let sigma = sample_std(window, mu);
    let t = horizon as f64;
    let mut values = [0.0; 5];
    for (index, level) in LEVELS.iter().enumerate() {
        values[index] = mu * t + sigma * t.sqrt() * normal_quantile(*level);
    }
    Some(values)
}

fn ewma_quantiles(variance: &[f64], available: usize, horizon: u32) -> Option<[f64; 5]> {
    if available < VOL_WINDOW || available == 0 {
        return None;
    }
    let sigma = variance[available - 1].max(0.0).sqrt();
    let t = horizon as f64;
    let mut values = [0.0; 5];
    for (index, level) in LEVELS.iter().enumerate() {
        values[index] = sigma * t.sqrt() * normal_quantile(*level);
    }
    Some(values)
}

fn historical_quantiles(series: &Series, origin: usize, horizon: u32) -> Option<[f64; 5]> {
    let t = horizon as usize;
    if t == 0 || t > origin {
        return None;
    }
    let first_start = origin.saturating_sub(HISTORY_WINDOW);
    let last_start = origin - t;
    if last_start < first_start {
        return None;
    }
    let mut samples = Vec::with_capacity(last_start - first_start + 1);
    for start in first_start..=last_start {
        samples.push(series.log_return(start, start + t));
    }
    if samples.len() < MIN_HISTORY_SAMPLES {
        return None;
    }
    samples.sort_by(f64::total_cmp);
    Some(LEVELS.map(|p| quantile_type7(&samples, p)))
}

/// The built-in components at the horizon step only, in `Gbm`, `Ewma`, `Historical`
/// order. `None` slots mean the component had too little data.
pub(crate) fn builtin_horizon(
    series: &Series,
    origin: usize,
    horizon: u32,
) -> [Option<[f64; 5]>; 3] {
    if origin >= series.len() || origin < VOL_WINDOW {
        return [None, None, None];
    }
    let available = origin.min(series.returns.len());
    [
        gbm_quantiles(&series.returns, available, horizon),
        ewma_quantiles(&series.ewma_variance, available, horizon),
        historical_quantiles(series, origin, horizon),
    ]
}

/// The built-in components at every reported step. All three are `None` when there are
/// fewer than 252 returns before the origin.
pub(crate) fn builtin_paths(
    series: &Series,
    origin: usize,
    horizon: u32,
) -> [(ComponentId, Option<PathQuantiles>); 3] {
    if origin >= series.len() || origin < VOL_WINDOW {
        return [
            (ComponentId::Gbm, None),
            (ComponentId::Ewma, None),
            (ComponentId::Historical, None),
        ];
    }
    let available = origin.min(series.returns.len());
    let step_list = steps(horizon);
    let mut gbm = Vec::with_capacity(step_list.len());
    let mut ewma = Vec::with_capacity(step_list.len());
    let mut historical = Vec::with_capacity(step_list.len());
    for step in &step_list {
        gbm.push(gbm_quantiles(&series.returns, available, *step));
        ewma.push(ewma_quantiles(&series.ewma_variance, available, *step));
        historical.push(historical_quantiles(series, origin, *step));
    }
    let collect = |values: Vec<Option<[f64; 5]>>| -> Option<PathQuantiles> {
        let mut out = Vec::with_capacity(values.len());
        for value in values {
            out.push(value?);
        }
        Some(PathQuantiles {
            steps: step_list.clone(),
            values: out,
        })
    };
    [
        (ComponentId::Gbm, collect(gbm)),
        (ComponentId::Ewma, collect(ewma)),
        (ComponentId::Historical, collect(historical)),
    ]
}

/// The most recent helper path for `component` and `horizon` with an origin on or before
/// `at`. Ties keep the later entry in the caller's slice.
pub(crate) fn external_at(
    paths: &[ExternalPath],
    component: ComponentId,
    horizon: u32,
    at: NaiveDate,
) -> Option<&ExternalPath> {
    let mut best: Option<&ExternalPath> = None;
    for path in paths {
        if path.component != component || path.horizon != horizon || path.origin > at {
            continue;
        }
        match best {
            None => best = Some(path),
            Some(current) if path.origin >= current.origin => best = Some(path),
            Some(_) => {}
        }
    }
    best
}

fn clean(values: [f64; 5]) -> Option<[f64; 5]> {
    if values.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let mut out = values;
    for index in 1..out.len() {
        if out[index] < out[index - 1] {
            out[index] = out[index - 1];
        }
    }
    Some(out)
}

fn value_at(path: &PathQuantiles, step: u32) -> Option<[f64; 5]> {
    let index = path.steps.iter().position(|candidate| *candidate == step)?;
    let values = *path.values.get(index)?;
    clean(values)
}

/// The helper's values at its horizon step, or `None` when the helper omitted it.
pub(crate) fn external_horizon(path: &ExternalPath) -> Option<[f64; 5]> {
    value_at(&path.path, path.horizon)
}

/// The helper's path at `wanted` steps. Steps the helper omitted are filled from the
/// horizon step: the median scales by `t/h` and each spread from the median by `sqrt(t/h)`.
pub(crate) fn interpolate(
    path: &PathQuantiles,
    horizon: u32,
    wanted: &[u32],
) -> Option<PathQuantiles> {
    let horizon_values = value_at(path, horizon)?;
    if horizon == 0 {
        return None;
    }
    let all_present = wanted.iter().all(|step| value_at(path, *step).is_some());
    if all_present {
        let mut values = Vec::with_capacity(wanted.len());
        for step in wanted {
            values.push(value_at(path, *step)?);
        }
        return Some(PathQuantiles {
            steps: wanted.to_vec(),
            values,
        });
    }
    let h = horizon as f64;
    let mut values = Vec::with_capacity(wanted.len());
    for step in wanted {
        let values_at = match value_at(path, *step) {
            Some(values) => values,
            None => {
                let ratio = *step as f64 / h;
                let median = horizon_values[2] * ratio;
                let spread_scale = ratio.sqrt();
                let mut filled = [0.0; 5];
                for index in 0..5 {
                    let spread = horizon_values[index] - horizon_values[2];
                    filled[index] = median + spread * spread_scale;
                }
                clean(filled)?
            }
        };
        values.push(values_at);
    }
    Some(PathQuantiles {
        steps: wanted.to_vec(),
        values,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_scales_median_and_spreads() {
        let path = PathQuantiles {
            steps: vec![20],
            values: vec![[-0.10, -0.05, 0.02, 0.09, 0.14]],
        };
        let out = interpolate(&path, 20, &[5, 10, 20]).expect("interpolated");
        assert_eq!(out.steps, vec![5, 10, 20]);
        let at_five = out.values[0];
        let expected_median = 0.02 * 0.25;
        let spread_scale = 0.25f64.sqrt();
        assert!((at_five[2] - expected_median).abs() < 1e-12);
        assert!((at_five[0] - (expected_median + (-0.12) * spread_scale)).abs() < 1e-12);
        assert!((at_five[4] - (expected_median + 0.12 * spread_scale)).abs() < 1e-12);
        assert_eq!(out.values[2], path.values[0]);
    }

    #[test]
    fn interpolation_keeps_reported_steps() {
        let path = PathQuantiles {
            steps: vec![5, 20],
            values: vec![[-0.1, -0.05, 0.0, 0.05, 0.1], [-0.2, -0.1, 0.02, 0.1, 0.2]],
        };
        let out = interpolate(&path, 20, &[5, 20]).expect("interpolated");
        assert_eq!(out.values[0], path.values[0]);
        assert_eq!(out.values[1], path.values[1]);
    }
}
