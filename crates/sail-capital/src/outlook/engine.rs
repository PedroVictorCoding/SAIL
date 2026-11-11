//! The outlook engine: component distributions at an origin, walk-forward weight fitting,
//! the blended fan, replay actuals and the monthly track record.
//!
//! No clock and no I/O. External helper paths are inputs and are looked up by origin on or
//! before the cutoff, so a replay never sees a path from the future.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};

use super::dist::{self, Dist};
use super::fit::{self, OriginScore};
use super::paths::{self, Series};
use super::{
    ComponentId, FanPoint, Instrument, LEVELS, Outlook, PathQuantiles, TrackRecord, steps,
};
use crate::forecast::ForecastStatus;

const MIN_RETURNS: usize = 252;
const MAX_WEIGHT_ORIGINS: usize = 156;
const WEIGHT_SPACING: usize = 5;
const TILT_LIMIT: f64 = 1.0;

#[derive(Default)]
struct Cache {
    dists: BTreeMap<usize, [Option<Dist>; 5]>,
    scores: BTreeMap<usize, OriginScore>,
}

struct Ctx<'a, 'b> {
    instrument: &'a Instrument<'b>,
    series: Series,
}

impl<'a, 'b> Ctx<'a, 'b> {
    fn new(instrument: &'a Instrument<'b>) -> Result<Self, String> {
        Ok(Self {
            instrument,
            series: Series::new(instrument.series.closes)?,
        })
    }

    fn dists_at(&self, origin: usize, horizon: u32) -> [Option<Dist>; 5] {
        let mut out: [Option<Dist>; 5] = Default::default();
        if origin >= self.series.len() {
            return out;
        }
        let builtin = paths::builtin_horizon(&self.series, origin, horizon);
        for (index, values) in builtin.into_iter().enumerate() {
            out[index] = values.and_then(Dist::new);
        }
        let date = self.series.dates[origin];
        for (component, index) in [(ComponentId::Chronos, 3), (ComponentId::Gbdt, 4)] {
            out[index] = paths::external_at(&self.instrument.external, component, horizon, date)
                .and_then(paths::external_horizon)
                .and_then(Dist::new);
        }
        out
    }

    fn dists_cached(&self, origin: usize, horizon: u32, cache: &mut Cache) -> [Option<Dist>; 5] {
        if let Some(dists) = cache.dists.get(&origin) {
            return dists.clone();
        }
        let dists = self.dists_at(origin, horizon);
        cache.dists.insert(origin, dists.clone());
        dists
    }

    fn score_cached(&self, origin: usize, horizon: u32, cache: &mut Cache) -> OriginScore {
        if let Some(score) = cache.scores.get(&origin) {
            return score.clone();
        }
        let dists = self.dists_cached(origin, horizon, cache);
        let outcome = self.series.log_return(origin, origin + horizon as usize);
        let score = fit::score_origin(&dists, outcome);
        cache.scores.insert(origin, score.clone());
        score
    }

    fn current_paths(&self, origin: usize, horizon: u32) -> [Option<PathQuantiles>; 5] {
        let mut out: [Option<PathQuantiles>; 5] = Default::default();
        if origin >= self.series.len() {
            return out;
        }
        for (component, path) in paths::builtin_paths(&self.series, origin, horizon) {
            if let Some(index) = fit::COMPONENTS.iter().position(|id| *id == component) {
                out[index] = path;
            }
        }
        let date = self.series.dates[origin];
        let wanted = steps(horizon);
        for (component, index) in [(ComponentId::Chronos, 3), (ComponentId::Gbdt, 4)] {
            out[index] = paths::external_at(&self.instrument.external, component, horizon, date)
                .and_then(|path| paths::interpolate(&path.path, path.horizon, &wanted));
        }
        out
    }

    fn weights_at(
        &self,
        origin: usize,
        horizon: u32,
        initial: [f64; 5],
        cache: &mut Cache,
    ) -> ([f64; 5], u32) {
        let current = self.dists_cached(origin, horizon, cache);
        let active = available(&current);
        if active.iter().all(|value| !value) {
            return ([0.0; 5], 0);
        }
        let mut scores = Vec::with_capacity(MAX_WEIGHT_ORIGINS);
        for index in 0..MAX_WEIGHT_ORIGINS {
            let offset = horizon as usize + index * WEIGHT_SPACING;
            let Some(scored_origin) = origin.checked_sub(offset) else {
                break;
            };
            let score = self.score_cached(scored_origin, horizon, cache);
            if score.available.iter().any(|value| *value) {
                scores.push(score);
            }
        }
        let (fitted, used) = fit::fit_weights(&scores, active, initial);
        (shrink_to_equal(fitted, active, used, horizon), used)
    }

    fn alpha(&self, horizon: u32) -> f64 {
        self.instrument
            .tilt
            .iter()
            .find(|(sessions, _)| *sessions == horizon)
            .map(|(_, alpha)| {
                if alpha.is_finite() {
                    alpha.clamp(-TILT_LIMIT, TILT_LIMIT)
                } else {
                    0.0
                }
            })
            .unwrap_or(0.0)
    }
}

/// Prior strength of the equal-weight blend, in independent horizon-length windows.
const SHRINK_WINDOWS: f64 = 20.0;

/// Weekly origins overlap when the horizon is longer than a week, so `used` origins hold
/// only about `used·5/h` independent outcomes. Fitted weights on so little data swing to
/// one component (seen on real holdings), so they are trusted in proportion:
/// `λ = n_eff / (n_eff + 20)`, and the rest goes to equal weights over active components.
fn shrink_to_equal(fitted: [f64; 5], active: [bool; 5], used: u32, horizon: u32) -> [f64; 5] {
    let count = active.iter().filter(|a| **a).count();
    if count == 0 || fitted.iter().sum::<f64>() <= 0.0 {
        return fitted;
    }
    let n_eff = used as f64 * WEIGHT_SPACING as f64 / (horizon.max(1) as f64);
    let lambda = n_eff / (n_eff + SHRINK_WINDOWS);
    let equal = 1.0 / count as f64;
    let mut out = [0.0; 5];
    for i in 0..5 {
        if active[i] {
            out[i] = lambda * fitted[i] + (1.0 - lambda) * equal;
        }
    }
    let total: f64 = out.iter().sum();
    out.map(|w| w / total)
}

fn available(dists: &[Option<Dist>; 5]) -> [bool; 5] {
    [
        dists[0].is_some(),
        dists[1].is_some(),
        dists[2].is_some(),
        dists[3].is_some(),
        dists[4].is_some(),
    ]
}

fn mix_items(dists: &[Option<Dist>; 5], weights: &[f64; 5]) -> Vec<(usize, Dist, f64)> {
    let mut items = Vec::with_capacity(5);
    let mut total = 0.0;
    for component in 0..5 {
        let weight = weights[component];
        if weight.is_finite()
            && weight > 0.0
            && let Some(dist) = &dists[component]
        {
            items.push((component, dist.clone(), weight));
            total += weight;
        }
    }
    if total > 0.0 {
        for item in &mut items {
            item.2 /= total;
        }
    }
    items
}

fn mean_factor(component: usize, dist: &Dist) -> f64 {
    if fit::COMPONENTS[component] == ComponentId::Gbm
        || fit::COMPONENTS[component] == ComponentId::Ewma
    {
        dist.normal_mean_factor()
    } else {
        dist.mean_factor()
    }
}

fn build_fan(
    origin_close: f64,
    horizon: u32,
    alpha: f64,
    dists_by_step: &[Vec<(usize, Dist, f64)>],
) -> Vec<FanPoint> {
    let step_list = steps(horizon);
    let mut fan = Vec::with_capacity(step_list.len());
    for (index, step) in step_list.iter().enumerate() {
        let Some(items) = dists_by_step.get(index) else {
            continue;
        };
        if items.is_empty() {
            continue;
        }
        let dists: Vec<&Dist> = items.iter().map(|item| &item.1).collect();
        let weights: Vec<f64> = items.iter().map(|item| item.2).collect();
        let tilt = alpha * *step as f64 / horizon as f64;
        let mut prices = [0.0; 5];
        let mut valid = true;
        for level in 0..5 {
            let log_return = dist::mixture_quantile(&dists, &weights, LEVELS[level]) + tilt;
            let price = origin_close * log_return.exp();
            if !price.is_finite() || price <= 0.0 {
                valid = false;
                break;
            }
            prices[level] = price;
        }
        if !valid {
            continue;
        }
        for level in 1..5 {
            if prices[level] < prices[level - 1] {
                prices[level] = prices[level - 1];
            }
        }
        let mut factor = 0.0;
        for (component, dist, weight) in items {
            factor += weight * mean_factor(*component, dist);
        }
        let mean = origin_close * factor * tilt.exp();
        fan.push(FanPoint {
            step: *step,
            prices,
            mean,
        });
    }
    fan
}

fn path_items(
    paths: &[Option<PathQuantiles>; 5],
    weights: &[f64; 5],
    index: usize,
) -> Vec<(usize, Dist, f64)> {
    let mut items = Vec::with_capacity(5);
    let mut total = 0.0;
    for component in 0..5 {
        let weight = weights[component];
        if !weight.is_finite() || weight <= 0.0 {
            continue;
        }
        let Some(path) = &paths[component] else {
            continue;
        };
        let Some(values) = path.values.get(index) else {
            continue;
        };
        let Some(dist) = Dist::new(*values) else {
            continue;
        };
        items.push((component, dist, weight));
        total += weight;
    }
    if total > 0.0 {
        for item in &mut items {
            item.2 /= total;
        }
    }
    items
}

fn insufficient(
    instrument: &Instrument<'_>,
    horizon: u32,
    origin: NaiveDate,
    origin_close: f64,
    reason: String,
) -> Outlook {
    Outlook {
        instrument_key: instrument.series.instrument_key.to_string(),
        symbol: instrument.series.symbol.to_string(),
        origin,
        origin_close,
        horizon,
        status: ForecastStatus::InsufficientEvidence { reason },
        fan: Vec::new(),
        weights: Vec::new(),
        weight_origins: 0,
        tilt: 0.0,
        tilt_signals: Vec::new(),
        actual: Vec::new(),
        actual_percentile: None,
    }
}

pub(crate) fn outlook(
    instrument: &Instrument<'_>,
    horizon: u32,
    cutoff: Option<NaiveDate>,
) -> Outlook {
    if instrument.series.closes.is_empty() {
        return insufficient(
            instrument,
            horizon,
            NaiveDate::MIN,
            0.0,
            "there are no closes".into(),
        );
    }
    if horizon == 0 {
        let date = instrument
            .series
            .closes
            .last()
            .map(|(date, _)| *date)
            .unwrap_or(NaiveDate::MIN);
        return insufficient(
            instrument,
            horizon,
            date,
            0.0,
            "the horizon must be at least one session".into(),
        );
    }
    let origin_index = match cutoff {
        Some(date) => instrument
            .series
            .closes
            .iter()
            .rposition(|(session, _)| *session <= date),
        None => Some(instrument.series.closes.len() - 1),
    };
    let Some(origin_index) = origin_index else {
        let date = cutoff.expect("cutoff present");
        return insufficient(
            instrument,
            horizon,
            date,
            0.0,
            format!("no close on or before {date}"),
        );
    };
    let ctx = match Ctx::new(instrument) {
        Ok(ctx) => ctx,
        Err(reason) => {
            let date = instrument.series.closes[origin_index].0;
            return insufficient(instrument, horizon, date, 0.0, reason);
        }
    };
    let origin_close = ctx.series.closes[origin_index];
    let origin_date = ctx.series.dates[origin_index];
    if origin_index < MIN_RETURNS {
        return insufficient(
            instrument,
            horizon,
            origin_date,
            origin_close,
            format!(
                "needs at least {MIN_RETURNS} daily returns before the origin; has {origin_index}"
            ),
        );
    }
    let mut cache = Cache::default();
    let (weights, weight_origins) = ctx.weights_at(origin_index, horizon, [0.0; 5], &mut cache);
    if !weights.iter().any(|value| *value > 0.0) {
        return insufficient(
            instrument,
            horizon,
            origin_date,
            origin_close,
            "no component could be computed".into(),
        );
    }
    let alpha = ctx.alpha(horizon);
    let paths = ctx.current_paths(origin_index, horizon);
    let step_list = steps(horizon);
    let dists_by_step: Vec<Vec<(usize, Dist, f64)>> = (0..step_list.len())
        .map(|index| path_items(&paths, &weights, index))
        .collect();
    let fan = build_fan(origin_close, horizon, alpha, &dists_by_step);

    let mut actual = Vec::new();
    let mut actual_percentile = None;
    if cutoff.is_some() {
        let last = ctx.series.len() - 1;
        let end = (origin_index + horizon as usize).min(last);
        for index in origin_index + 1..=end {
            actual.push((ctx.series.dates[index], ctx.series.closes[index]));
        }
        if origin_index + horizon as usize <= last {
            let outcome = ctx
                .series
                .log_return(origin_index, origin_index + horizon as usize);
            let dists = ctx.dists_cached(origin_index, horizon, &mut cache);
            let items = mix_items(&dists, &weights);
            if !items.is_empty() {
                let refs: Vec<&Dist> = items.iter().map(|item| &item.1).collect();
                let mix_weights: Vec<f64> = items.iter().map(|item| item.2).collect();
                let percentile = dist::mixture_cdf(&refs, &mix_weights, outcome - alpha) * 100.0;
                actual_percentile = Some(percentile.clamp(0.0, 100.0));
            }
        }
    }

    Outlook {
        instrument_key: instrument.series.instrument_key.to_string(),
        symbol: instrument.series.symbol.to_string(),
        origin: origin_date,
        origin_close,
        horizon,
        status: ForecastStatus::Ok,
        fan,
        weights: fit::COMPONENTS
            .iter()
            .zip(weights.iter())
            .map(|(component, weight)| (*component, *weight))
            .collect(),
        weight_origins,
        tilt: alpha,
        tilt_signals: instrument.tilt_signals.clone(),
        actual,
        actual_percentile,
    }
}

fn month_index(date: NaiveDate) -> i32 {
    date.year() * 12 + date.month0() as i32
}

fn monthly_cutoffs(series: &Series, horizon: usize, today: NaiveDate) -> Vec<usize> {
    let today_month = month_index(today);
    let mut cutoffs = Vec::new();
    let mut start = 0;
    while start < series.len() {
        let month = (series.dates[start].year(), series.dates[start].month0());
        let mut end = start;
        while end + 1 < series.len() {
            let next = series.dates[end + 1];
            if (next.year(), next.month0()) != month {
                break;
            }
            end += 1;
        }
        let date = series.dates[end];
        let index = month_index(date);
        let outcome = end + horizon;
        if date <= today
            && index < today_month
            && index >= today_month - 36
            && outcome < series.len()
            && series.dates[outcome] <= today
        {
            cutoffs.push(end);
        }
        start = end + 1;
    }
    cutoffs
}

pub(crate) fn track_record(
    instruments: &[Instrument<'_>],
    horizon: u32,
    today: NaiveDate,
) -> TrackRecord {
    let mut record = TrackRecord {
        horizon,
        replays: 0,
        inside_68: 0.0,
        inside_90: 0.0,
        mean_percentile: 0.0,
        skill_vs_no_change: 0.0,
        direction_hit_rate: 0.0,
    };
    if horizon == 0 {
        return record;
    }
    let h = horizon as usize;
    let mut replays = 0usize;
    let mut inside_68 = 0usize;
    let mut inside_90 = 0usize;
    let mut percentile_sum = 0.0;
    let mut skill_blend = 0.0;
    let mut skill_ewma = 0.0;
    let mut skill_replays = 0usize;
    let mut direction_hits = 0usize;
    let mut direction_total = 0usize;

    for instrument in instruments {
        let Ok(ctx) = Ctx::new(instrument) else {
            continue;
        };
        if ctx.series.len() <= MIN_RETURNS || ctx.series.len() <= h {
            continue;
        }
        let mut cache = Cache::default();
        let mut initial = [0.0; 5];
        for origin in monthly_cutoffs(&ctx.series, h, today) {
            if origin < MIN_RETURNS || origin + h >= ctx.series.len() {
                continue;
            }
            let dists = ctx.dists_cached(origin, horizon, &mut cache);
            if available(&dists).iter().all(|value| !value) {
                continue;
            }
            let (weights, _) = ctx.weights_at(origin, horizon, initial, &mut cache);
            if weights.iter().sum::<f64>() <= 0.0 {
                continue;
            }
            initial = weights;
            let items = mix_items(&dists, &weights);
            if items.is_empty() {
                continue;
            }
            let refs: Vec<&Dist> = items.iter().map(|item| &item.1).collect();
            let mix_weights: Vec<f64> = items.iter().map(|item| item.2).collect();
            let outcome = ctx.series.log_return(origin, origin + h);
            let q05 = dist::mixture_quantile(&refs, &mix_weights, 0.05);
            let q16 = dist::mixture_quantile(&refs, &mix_weights, 0.16);
            let q50 = dist::mixture_quantile(&refs, &mix_weights, 0.5);
            let q84 = dist::mixture_quantile(&refs, &mix_weights, 0.84);
            let q95 = dist::mixture_quantile(&refs, &mix_weights, 0.95);
            if outcome >= q16 && outcome <= q84 {
                inside_68 += 1;
            }
            if outcome >= q05 && outcome <= q95 {
                inside_90 += 1;
            }
            percentile_sum += dist::mixture_cdf(&refs, &mix_weights, outcome) * 100.0;
            let score = ctx.score_cached(origin, horizon, &mut cache);
            let blend = fit::score_crps(&score, &weights);
            if score.available[1] {
                skill_blend += blend;
                skill_ewma += score.gram[1][1];
                skill_replays += 1;
            }
            if outcome != 0.0 {
                direction_total += 1;
                if (q50 > 0.0 && outcome > 0.0) || (q50 < 0.0 && outcome < 0.0) {
                    direction_hits += 1;
                }
            }
            replays += 1;
        }
    }

    if replays > 0 {
        let count = replays as f64;
        record.replays = replays as u32;
        record.inside_68 = inside_68 as f64 / count;
        record.inside_90 = inside_90 as f64 / count;
        record.mean_percentile = percentile_sum / count;
    }
    if skill_replays > 0 && skill_ewma > 0.0 {
        record.skill_vs_no_change = 1.0 - skill_blend / skill_ewma;
    }
    if direction_total > 0 {
        record.direction_hit_rate = direction_hits as f64 / direction_total as f64;
    }
    record
}
