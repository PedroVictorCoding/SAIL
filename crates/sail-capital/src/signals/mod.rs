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
use std::collections::{BTreeMap, BTreeSet};

use crate::forecast::CloseSeries;
use crate::forecast::stats::{mean, sample_std, spearman};
use chrono::NaiveDate;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

mod matrix;

/// Fewest instruments for a cross-section. Below it, nothing is scored or combined.
pub const MIN_CROSS_SECTION: usize = 5;

/// Horizons, in sessions, that signals are scored and combined at.
pub const SIGNAL_HORIZONS: [u32; 2] = [20, 60];

/// Fewest cross-sections before an IC is judged. A single cross-section has no sampling
/// spread, so it never shrinks to a usable IC.
const MIN_ORIGINS: usize = 2;

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
    if !SignalId::PRICE.contains(&signal) {
        return None;
    }
    let closes: Vec<f64> = series
        .closes
        .iter()
        .map(|(_, close)| close.to_f64())
        .collect::<Option<Vec<f64>>>()?;
    if closes.is_empty()
        || closes
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return None;
    }
    let log_closes: Vec<f64> = closes.iter().map(|value| value.ln()).collect();
    value_from(&closes, &log_closes, signal, closes.len() - 1)
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
    let mut instruments = Vec::with_capacity(series.len());
    let mut skipped_series = 0usize;
    for item in series {
        match Instrument::new(item) {
            Some(instrument) => instruments.push(instrument),
            None => skipped_series += 1,
        }
    }

    let external_by_signal = group_externals(external);
    let signals = collect_signals(&external_by_signal);
    let cross_sections = build_cross_sections(&instruments, &signals, &external_by_signal, config);
    let available: Vec<bool> = (0..signals.len())
        .map(|signal| {
            cross_sections
                .iter()
                .any(|cross_section| !cross_section.values[signal].is_empty())
        })
        .collect();
    let available_indices: Vec<usize> = (0..signals.len())
        .filter(|index| available[*index])
        .collect();
    let correlations = correlation_matrix(&cross_sections, &available_indices);
    let inverse = matrix::invert_with_ridge(&correlations, available_indices.len(), 0.1);

    let mut skill: Vec<SignalSkill> = Vec::new();
    let mut combined_ic: Vec<(u32, f64)> = Vec::new();
    let mut horizon_weights: Vec<(u32, Vec<f64>)> = Vec::new();

    for &horizon in &config.horizons {
        let horizon_sessions = horizon as usize;
        if horizon_sessions == 0 {
            combined_ic.push((horizon, 0.0));
            continue;
        }
        let mut rows: Vec<SignalSkill> = Vec::with_capacity(signals.len());
        for (signal_index, signal) in signals.iter().enumerate() {
            let mut ics = Vec::new();
            if available[signal_index] {
                for cross_section in &cross_sections {
                    let scores = &cross_section.values[signal_index];
                    if scores.len() < MIN_CROSS_SECTION {
                        continue;
                    }
                    let mut pairs = Vec::with_capacity(scores.len());
                    for score in scores {
                        let instrument = &instruments[score.instrument];
                        let next = match score.index.checked_add(horizon_sessions) {
                            Some(next) if next < instrument.log_closes.len() => next,
                            _ => continue,
                        };
                        let outcome =
                            instrument.log_closes[next] - instrument.log_closes[score.index];
                        if outcome.is_finite() {
                            pairs.push((score.z, outcome));
                        }
                    }
                    if pairs.len() < MIN_CROSS_SECTION {
                        continue;
                    }
                    if let Some(rho) = spearman(&pairs) {
                        ics.push(rho);
                    }
                }
            }
            let origins = ics.len();
            let ic_mean = if origins == 0 { 0.0 } else { mean(&ics) };
            let overlap = (horizon_sessions as f64 / config.step_days.max(1) as f64).max(1.0);
            let ic_se = if origins >= 2 {
                sample_std(&ics, ic_mean) / (origins as f64 / overlap).sqrt()
            } else {
                0.0
            };
            let t_stat = if ic_se > 0.0 && ic_se.is_finite() {
                ic_mean / ic_se
            } else {
                0.0
            };
            let ic_used = if origins >= MIN_ORIGINS && ic_se.is_finite() {
                ic_mean.signum() * (ic_mean.abs() - 2.0 * ic_se).max(0.0)
            } else {
                0.0
            };
            rows.push(SignalSkill {
                signal: *signal,
                horizon_sessions: horizon,
                origins: origins.min(u32::MAX as usize) as u32,
                ic_mean,
                ic_se,
                t_stat,
                ic_used,
                weight: 0.0,
            });
        }

        let ic_star: Vec<f64> = available_indices
            .iter()
            .map(|signal_index| rows[*signal_index].ic_used)
            .collect();
        let w_full = match &inverse {
            Some(inverse) if !available_indices.is_empty() => {
                matrix::mat_vec(inverse, available_indices.len(), &ic_star)
            }
            _ => ic_star.iter().map(|value| value / 1.1).collect(),
        };
        let combined = matrix::dot(&ic_star, &w_full).max(0.0).sqrt();
        let combined_ic_value = if combined.is_finite() { combined } else { 0.0 };

        let mut weights = vec![0.0; signals.len()];
        for (position, signal_index) in available_indices.iter().enumerate() {
            if ic_star[position] != 0.0 {
                weights[*signal_index] = w_full[position];
                rows[*signal_index].weight = w_full[position];
            }
        }
        skill.extend(rows);
        combined_ic.push((horizon, combined_ic_value));
        horizon_weights.push((horizon, weights));
    }

    let latest = latest_scores(
        &instruments,
        &signals,
        &external_by_signal,
        &horizon_weights,
        &combined_ic,
        sigma,
    );

    let notes = report_notes(
        &signals,
        &skill,
        config,
        skipped_series,
        instruments.len(),
        cross_sections.len(),
        latest.is_empty(),
    );

    SignalReport {
        config: config.clone(),
        skill,
        combined_ic,
        latest,
        notes,
    }
}

/// External scores for one signal, keyed by instrument and sorted by session.
type SeriesScores = BTreeMap<String, Vec<(NaiveDate, f64)>>;

/// External scores for every signal that has any.
type ExternalScoresBySignal = BTreeMap<SignalId, SeriesScores>;

/// One instrument's closes, converted once.
struct Instrument {
    key: String,
    symbol: String,
    dates: Vec<NaiveDate>,
    closes: Vec<f64>,
    log_closes: Vec<f64>,
}

impl Instrument {
    fn new(series: &CloseSeries<'_>) -> Option<Self> {
        if series.closes.is_empty() {
            return None;
        }
        let mut dates = Vec::with_capacity(series.closes.len());
        let mut closes = Vec::with_capacity(series.closes.len());
        let mut log_closes = Vec::with_capacity(series.closes.len());
        for (date, close) in series.closes {
            let value = close.to_f64()?;
            if !value.is_finite() || value <= 0.0 {
                return None;
            }
            let log = value.ln();
            if !log.is_finite() {
                return None;
            }
            dates.push(*date);
            closes.push(value);
            log_closes.push(log);
        }
        Some(Self {
            key: series.instrument_key.to_string(),
            symbol: series.symbol.to_string(),
            dates,
            closes,
            log_closes,
        })
    }

    fn index_on_or_before(&self, date: NaiveDate) -> Option<usize> {
        let position = self.dates.partition_point(|session| *session <= date);
        position.checked_sub(1)
    }
}

/// One instrument's combined value before rescaling, and the scores behind it.
struct CombinedInput {
    instrument: usize,
    combined: f64,
    z: Vec<(SignalId, f64)>,
}

/// One cross-section: per signal, the standardized values for the instruments that had
/// one, with the session index the value was read from.
struct CrossSection {
    values: Vec<Vec<Score>>,
}

/// One instrument's standardized value in a cross-section.
#[derive(Clone)]
struct Score {
    instrument: usize,
    z: f64,
    index: usize,
}

fn value_from(closes: &[f64], log_closes: &[f64], signal: SignalId, index: usize) -> Option<f64> {
    if index >= closes.len() {
        return None;
    }
    let value = match signal {
        SignalId::Momentum12_1 => {
            if index < 252 {
                return None;
            }
            (closes[index - 21] / closes[index - 252]).ln()
        }
        SignalId::ShortTermReversal => {
            if index < 21 {
                return None;
            }
            -(closes[index] / closes[index - 21]).ln()
        }
        SignalId::LowVolatility => {
            if index < 252 {
                return None;
            }
            let returns: Vec<f64> = (index - 252..index)
                .map(|session| log_closes[session + 1] - log_closes[session])
                .collect();
            let returns_mean = mean(&returns);
            -sample_std(&returns, returns_mean)
        }
        SignalId::Trend => {
            if index < 199 {
                return None;
            }
            let window = &closes[index - 199..=index];
            let window_mean = mean(window);
            if window_mean <= 0.0 {
                return None;
            }
            (closes[index] / window_mean).ln()
        }
        _ => return None,
    };
    value.is_finite().then_some(value)
}

fn group_externals(external: &[ExternalScores]) -> ExternalScoresBySignal {
    let mut grouped: ExternalScoresBySignal = BTreeMap::new();
    for scores in external {
        let by_key = grouped.entry(scores.signal).or_default();
        for (key, session, value) in &scores.values {
            if !value.is_finite() {
                continue;
            }
            by_key
                .entry(key.clone())
                .or_default()
                .push((*session, *value));
        }
    }
    for by_key in grouped.values_mut() {
        for rows in by_key.values_mut() {
            rows.sort_by_key(|left| left.0);
            rows.dedup_by_key(|(session, _)| *session);
        }
    }
    grouped
}

fn collect_signals(external: &ExternalScoresBySignal) -> Vec<SignalId> {
    let mut signals: Vec<SignalId> = SignalId::PRICE
        .into_iter()
        .filter(|signal| !external.contains_key(signal))
        .collect();
    signals.extend(external.keys().copied());
    signals.sort();
    signals.dedup();
    signals
}

fn signal_value(
    instrument: &Instrument,
    signal: SignalId,
    external: Option<&SeriesScores>,
    index: usize,
    date: NaiveDate,
) -> Option<f64> {
    match external {
        Some(by_key) => {
            let rows = by_key.get(&instrument.key)?;
            let position = rows.partition_point(|(session, _)| *session <= date);
            let (_, value) = rows.get(position.checked_sub(1)?)?;
            value.is_finite().then_some(*value)
        }
        None => value_from(&instrument.closes, &instrument.log_closes, signal, index),
    }
}

fn build_cross_sections(
    instruments: &[Instrument],
    signals: &[SignalId],
    external: &ExternalScoresBySignal,
    config: &SignalEvalConfig,
) -> Vec<CrossSection> {
    let mut dates: BTreeSet<NaiveDate> = BTreeSet::new();
    for instrument in instruments {
        dates.extend(instrument.dates.iter().copied());
    }
    let Some(&anchor) = dates.first() else {
        return Vec::new();
    };
    let step_days = config.step_days.max(1) as i64;
    let start = config.start.unwrap_or(anchor).max(anchor);
    let mut cross_sections = Vec::new();
    for &date in &dates {
        if date < start || (date - anchor).num_days() % step_days != 0 {
            continue;
        }
        let mut values: Vec<Vec<Score>> = vec![Vec::new(); signals.len()];
        let mut any = false;
        for (signal_index, signal) in signals.iter().enumerate() {
            let external_rows = external.get(signal);
            let mut raw: Vec<(usize, f64, usize)> = Vec::new();
            for (instrument_index, instrument) in instruments.iter().enumerate() {
                let Some(index) = instrument.index_on_or_before(date) else {
                    continue;
                };
                if let Some(value) = signal_value(instrument, *signal, external_rows, index, date) {
                    raw.push((instrument_index, value, index));
                }
            }
            if raw.len() < MIN_CROSS_SECTION {
                continue;
            }
            let raw_values: Vec<f64> = raw.iter().map(|(_, value, _)| *value).collect();
            let raw_mean = mean(&raw_values);
            let raw_sd = sample_std(&raw_values, raw_mean);
            if !raw_sd.is_finite() || raw_sd <= 0.0 {
                continue;
            }
            values[signal_index] = raw
                .into_iter()
                .map(|(instrument, value, index)| Score {
                    instrument,
                    z: ((value - raw_mean) / raw_sd).clamp(-3.0, 3.0),
                    index,
                })
                .collect();
            any = true;
        }
        if any {
            cross_sections.push(CrossSection { values });
        }
    }
    cross_sections
}

fn correlation_matrix(cross_sections: &[CrossSection], available: &[usize]) -> Vec<f64> {
    let count = available.len();
    let mut correlations = vec![0.0; count * count];
    for index in 0..count {
        correlations[index * count + index] = 1.0;
    }
    for left in 0..count {
        for right in (left + 1)..count {
            let mut pairs = Vec::new();
            for cross_section in cross_sections {
                let left_scores = &cross_section.values[available[left]];
                let right_scores = &cross_section.values[available[right]];
                let mut i = 0;
                let mut j = 0;
                while i < left_scores.len() && j < right_scores.len() {
                    match left_scores[i].instrument.cmp(&right_scores[j].instrument) {
                        std::cmp::Ordering::Less => i += 1,
                        std::cmp::Ordering::Greater => j += 1,
                        std::cmp::Ordering::Equal => {
                            pairs.push((left_scores[i].z, right_scores[j].z));
                            i += 1;
                            j += 1;
                        }
                    }
                }
            }
            let correlation = matrix::pearson(&pairs).unwrap_or(0.0);
            correlations[left * count + right] = correlation;
            correlations[right * count + left] = correlation;
        }
    }
    correlations
}

fn latest_date(
    instruments: &[Instrument],
    signals: &[SignalId],
    external: &ExternalScoresBySignal,
) -> Option<NaiveDate> {
    let mut dates: BTreeSet<NaiveDate> = BTreeSet::new();
    for instrument in instruments {
        dates.extend(instrument.dates.iter().copied());
    }
    for &date in dates.iter().rev() {
        let mut count = 0usize;
        for instrument in instruments {
            let Some(index) = instrument.index_on_or_before(date) else {
                continue;
            };
            let mut has_value = false;
            for signal in signals {
                if signal_value(instrument, *signal, external.get(signal), index, date).is_some() {
                    has_value = true;
                    break;
                }
            }
            if has_value {
                count += 1;
            }
        }
        if count >= MIN_CROSS_SECTION {
            return Some(date);
        }
    }
    None
}

fn latest_scores(
    instruments: &[Instrument],
    signals: &[SignalId],
    external: &ExternalScoresBySignal,
    horizon_weights: &[(u32, Vec<f64>)],
    combined_ic: &[(u32, f64)],
    sigma: &dyn Fn(&str, u32) -> Option<f64>,
) -> Vec<CombinedScore> {
    let Some(date) = latest_date(instruments, signals, external) else {
        return Vec::new();
    };
    let mut z_by_instrument: Vec<Vec<(usize, f64)>> = vec![Vec::new(); instruments.len()];
    for (signal_index, signal) in signals.iter().enumerate() {
        let mut raw: Vec<(usize, f64)> = Vec::new();
        for (instrument_index, instrument) in instruments.iter().enumerate() {
            let Some(index) = instrument.index_on_or_before(date) else {
                continue;
            };
            if let Some(value) =
                signal_value(instrument, *signal, external.get(signal), index, date)
            {
                raw.push((instrument_index, value));
            }
        }
        if raw.len() < MIN_CROSS_SECTION {
            continue;
        }
        let raw_values: Vec<f64> = raw.iter().map(|(_, value)| *value).collect();
        let raw_mean = mean(&raw_values);
        let raw_sd = sample_std(&raw_values, raw_mean);
        if !raw_sd.is_finite() || raw_sd <= 0.0 {
            continue;
        }
        for (instrument_index, value) in raw {
            z_by_instrument[instrument_index]
                .push((signal_index, ((value - raw_mean) / raw_sd).clamp(-3.0, 3.0)));
        }
    }

    let mut scores = Vec::new();
    for (horizon, weights) in horizon_weights {
        let ic = combined_ic
            .iter()
            .find(|(value, _)| value == horizon)
            .map(|(_, value)| *value)
            .unwrap_or(0.0);
        let mut sums: Vec<CombinedInput> = Vec::new();
        for (instrument_index, entries) in z_by_instrument.iter().enumerate() {
            if entries.is_empty() {
                continue;
            }
            let mut combined = 0.0;
            let mut z_list = Vec::with_capacity(entries.len());
            for (signal_index, z) in entries {
                combined += weights[*signal_index] * z;
                z_list.push((signals[*signal_index], *z));
            }
            sums.push(CombinedInput {
                instrument: instrument_index,
                combined,
                z: z_list,
            });
        }
        if sums.is_empty() {
            continue;
        }
        let raw: Vec<f64> = sums.iter().map(|input| input.combined).collect();
        let raw_mean = mean(&raw);
        let raw_sd = sample_std(&raw, raw_mean);
        for input in sums {
            let combined = if raw_sd.is_finite() && raw_sd > 0.0 {
                (input.combined - raw_mean) / raw_sd
            } else {
                0.0
            };
            let instrument = &instruments[input.instrument];
            let instrument_sigma = sigma(&instrument.key, *horizon)
                .filter(|value| value.is_finite() && *value >= 0.0)
                .unwrap_or(0.0);
            let alpha = ic * instrument_sigma * combined;
            scores.push(CombinedScore {
                instrument_key: instrument.key.clone(),
                symbol: instrument.symbol.clone(),
                horizon_sessions: *horizon,
                z: input.z,
                combined,
                alpha: if alpha.is_finite() { alpha } else { 0.0 },
                sigma: instrument_sigma,
            });
        }
    }
    scores.sort_by(|left, right| {
        left.horizon_sessions
            .cmp(&right.horizon_sessions)
            .then_with(|| left.symbol.cmp(&right.symbol))
    });
    scores
}

fn signal_label(signal: SignalId) -> &'static str {
    match signal {
        SignalId::Momentum12_1 => "momentum 12-1",
        SignalId::ShortTermReversal => "short-term reversal",
        SignalId::LowVolatility => "low volatility",
        SignalId::Trend => "trend",
        SignalId::ChartPattern => "chart patterns",
        SignalId::FoundationForecast => "foundation forecast",
        SignalId::FilingSentiment => "filing sentiment",
        SignalId::InsiderBuying => "insider buying",
    }
}

fn join_names(names: &[&str]) -> String {
    if names.is_empty() {
        "none".into()
    } else {
        names.join(", ")
    }
}

fn report_notes(
    signals: &[SignalId],
    skill: &[SignalSkill],
    config: &SignalEvalConfig,
    skipped_series: usize,
    instrument_count: usize,
    cross_section_count: usize,
    latest_empty: bool,
) -> Vec<String> {
    let mut notes = Vec::new();
    if instrument_count == 0 {
        notes.push("There are no usable price series, so no signal could be scored.".into());
    }
    if skipped_series > 0 {
        notes.push(format!(
            "{skipped_series} price series were skipped because their closes were not positive finite numbers."
        ));
    }
    if cross_section_count == 0 && instrument_count > 0 {
        notes.push(format!(
            "No cross-section had at least {MIN_CROSS_SECTION} instruments with a value and enough history, so nothing was scored."
        ));
    }
    for &horizon in &config.horizons {
        if horizon == 0 {
            continue;
        }
        let rows: Vec<&SignalSkill> = skill
            .iter()
            .filter(|row| row.horizon_sessions == horizon)
            .collect();
        if rows.is_empty() {
            continue;
        }
        let passed: Vec<&str> = rows
            .iter()
            .filter(|row| row.ic_used > 0.0)
            .map(|row| signal_label(row.signal))
            .collect();
        let failed: Vec<&str> = rows
            .iter()
            .filter(|row| row.ic_used <= 0.0)
            .map(|row| signal_label(row.signal))
            .collect();
        notes.push(format!(
            "At {horizon} sessions, passed validation: {}; contributed nothing: {}.",
            join_names(&passed),
            join_names(&failed)
        ));
    }
    if !signals.is_empty() && skill.iter().all(|row| row.ic_used <= 0.0) && instrument_count > 0 {
        notes.push(
            "No signal passed validation, so every alpha is exactly zero and ideas reduce to doing nothing."
                .into(),
        );
    }
    notes.push(
        "Only the instruments you hold are scored, so the universe is small and survivorship-biased."
            .into(),
    );
    let max_horizon = config.horizons.iter().copied().max().unwrap_or(0);
    if max_horizon as usize > config.step_days.max(1) as usize {
        notes.push(format!(
            "Outcomes overlap because cross-sections are {} calendar days apart while horizons reach {max_horizon} sessions; the standard errors are widened for that overlap.",
            config.step_days.max(1)
        ));
    }
    notes.push(
        "The IC uses prices only and leaves out spreads, fees, taxes and FX; the optimizer accounts for trading costs separately."
            .into(),
    );
    if signals
        .iter()
        .any(|signal| !SignalId::PRICE.contains(signal))
    {
        notes.push(
            "Scores supplied from outside this module are scored exactly like the price signals, and only added through their own helpers."
                .into(),
        );
    }
    if latest_empty && cross_section_count > 0 {
        notes.push(
            "The latest date did not have enough instruments with values to show a combined score."
                .into(),
        );
    }
    notes
}
