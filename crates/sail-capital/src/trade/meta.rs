//! Round 25 (A8, Kilo's slice): meta-labeling. The setups decide *whether* a trade
//! exists; this model only judges *how good* each firing looks, from what was known
//! when it fired.
//!
//! **Label.** A replayed trade is a hit when it reached target 1 (1R) before its stop.
//!
//! **Features** ([`MetaFeatures`]), all known at the signal session's close:
//! - setup and side (one-hot);
//! - whether the market condition was "calm and trending the trade's way";
//! - relative volume (missing → 1.0 plus a missing flag);
//! - ATR(14) as a percentile of its own last 252 sessions;
//! - distance of the close from its 200-session average, in ATRs;
//! - the signal session's close-to-open gap, in ATRs.
//!
//! **Model.** Logistic regression in pure Rust (no new dependencies): standardized
//! features, L2 penalty, fitted by 100 batch-gradient steps (fixed a priori). It is
//! scored with the same purged walk-forward folds as the setups (no fold trains on a
//! trade whose holding window overlaps the test fold).
//!
//! **Weight.** The model earns weight only when its out-of-sample Brier score beats
//! the base rate's by more than two standard errors of the paired difference **and**
//! by at least 0.01. The base rate is each trade's own setup and side hit rate in
//! training, not one rate for everyone: a model that only re-learns "this setup hits
//! more often" has not beaten the scoreboard. Otherwise [`MetaReport::model`] is
//! `None` and every card is left exactly as it was.
//!
//! **What it may change.** Only the order of cards inside a tier and a size factor in
//! `[MIN_SIZE_FACTOR, 1.0]` that scales a card's risk down. It never creates, removes
//! or upgrades a card, never raises risk, and runs before the heat cap so the cap sees
//! the reduced risk.
//!
//! **Purity.** No I/O and no clock.

use std::collections::BTreeMap;

use chrono::{Days, NaiveDate};
use serde::{Deserialize, Serialize};

use super::indicators::{adjust, atr, relative_volume, sma};
use super::regime::Condition;
use super::{Exit, Instrument, SetupId, Side, SignalCard, Trade};

/// The smallest size factor: a card the model dislikes is halved, never dropped.
pub const MIN_SIZE_FACTOR: f64 = 0.5;

/// Fewest replayed trades before a model is fitted at all.
pub const MIN_TRAIN_TRADES: usize = 400;

/// What the model sees for one firing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetaFeatures {
    /// The setup.
    pub setup: SetupId,
    /// The side.
    pub side: Side,
    /// The market was calm and trending the trade's way.
    pub agrees_calm: bool,
    /// Volume over the median of the previous 20 sessions, if known.
    pub relative_volume: Option<f64>,
    /// ATR(14) percentile in its last 252 sessions, 0..1.
    pub atr_percentile: f64,
    /// `(close − SMA200) / ATR14`.
    pub distance_200d_atr: f64,
    /// `(open − previous close) / ATR14` on the signal session.
    pub gap_atr: f64,
}

/// The fitted model and how it did out of sample.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetaModel {
    /// Feature names, in the order of `coefficients`.
    pub features: Vec<String>,
    /// Coefficients on the standardized features.
    pub coefficients: Vec<f64>,
    /// Intercept.
    pub intercept: f64,
    /// Per-feature mean and standard deviation used to standardize.
    pub scaling: Vec<(f64, f64)>,
}

/// One run's meta-labeling result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MetaReport {
    /// The model, only when it beat the base rate (see the module docs).
    pub model: Option<MetaModel>,
    /// Replayed trades with features.
    pub n: u32,
    /// Share of them that were hits.
    pub base_rate: f64,
    /// Out-of-sample Brier score of the model, when fitted.
    pub brier: Option<f64>,
    /// Out-of-sample Brier score of the base rate.
    pub base_brier: Option<f64>,
    /// Standard error of the paired Brier difference.
    pub brier_diff_se: Option<f64>,
    /// Plain notes: why there is no model, or what it learned.
    pub notes: Vec<String>,
}

/// The model's view of one card.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetaScore {
    /// Predicted chance of reaching 1R before the stop.
    pub p_hit: f64,
    /// The base rate it is compared with.
    pub base_rate: f64,
    /// Factor applied to the card's risk, in `[MIN_SIZE_FACTOR, 1.0]`.
    pub size_factor: f64,
}

/// Adjusted bars, ATR and 200-session averages, computed once per instrument so `fit`
/// does not redo them for every replayed trade.
struct Prepared {
    adjusted: super::Adjusted,
    atr: Vec<Option<f64>>,
    slow: Vec<Option<f64>>,
}

impl Prepared {
    fn new(instrument: &Instrument) -> Self {
        let adjusted = adjust(&instrument.bars);
        let atr = atr(&adjusted.bars, 14);
        let slow = sma(&adjusted.bars, 200);
        Self {
            adjusted,
            atr,
            slow,
        }
    }

    fn features(
        &self,
        instrument: &Instrument,
        index: usize,
        setup: SetupId,
        side: Side,
        agrees_calm: bool,
    ) -> Option<MetaFeatures> {
        if index >= instrument.bars.len() {
            return None;
        }
        let value = raw_atr(&self.adjusted, &self.atr, index)?;
        let slow = self.slow.get(index).copied().flatten()?;
        if !(slow.is_finite() && slow > 0.0) {
            return None;
        }
        let close = self.adjusted.bars[index].close;
        let gap_atr = if index == 0 {
            0.0
        } else {
            (instrument.bars[index].open - instrument.bars[index - 1].close) / value
        };
        Some(MetaFeatures {
            setup,
            side,
            agrees_calm,
            relative_volume: relative_volume(&instrument.bars, index, 20),
            atr_percentile: atr_percentile(&self.atr, index)?,
            distance_200d_atr: (close - slow) / value,
            gap_atr,
        })
    }
}

/// Features for a firing of `setup`/`side` on `instrument` at bar `index` (the signal
/// session), or `None` when there aren't enough bars or the ATR/200-day average is
/// unavailable. Prices are adjusted for dividends, as the triggers are.
pub fn features(
    instrument: &Instrument,
    index: usize,
    setup: SetupId,
    side: Side,
    agrees_calm: bool,
) -> Option<MetaFeatures> {
    Prepared::new(instrument).features(instrument, index, setup, side, agrees_calm)
}

/// ATR in raw prices: the adjusted ATR divided by the bar's back-adjustment factor.
fn raw_atr(adjusted: &super::Adjusted, atr: &[Option<f64>], index: usize) -> Option<f64> {
    let value = atr.get(index).copied().flatten()?;
    let factor = adjusted.factor.get(index).copied()?;
    (factor.is_finite() && factor > 0.0 && value.is_finite() && value > 0.0).then(|| value / factor)
}

/// Percentile of the current ATR inside its trailing 252 values, 0..=1. `None` before
/// 252 complete values. The percentile is the share strictly below the current value.
fn atr_percentile(atr: &[Option<f64>], index: usize) -> Option<f64> {
    if index + 1 < 252 {
        return None;
    }
    let mut values: Vec<f64> = atr[index + 1 - 252..=index]
        .iter()
        .copied()
        .collect::<Option<Vec<f64>>>()?;
    let current = values.pop()?;
    if !current.is_finite() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let below = values.iter().filter(|value| **value < current).count();
    Some(below as f64 / values.len() as f64)
}

/// The feature names, in coefficient order (fixed a priori).
fn feature_names() -> Vec<String> {
    let mut names: Vec<String> = SetupId::ALL
        .iter()
        .map(|setup| format!("setup:{setup:?}"))
        .collect();
    names.push("side:long".into());
    names.push("side:short".into());
    names.push("agrees_calm".into());
    names.push("relative_volume".into());
    names.push("relative_volume_missing".into());
    names.push("atr_percentile".into());
    names.push("distance_200d_atr".into());
    names.push("gap_atr".into());
    names
}

/// The fixed encoding. Relative volume is capped at 5 and missing volume counts as 1.0
/// plus a missing flag (fixed a priori, so a single huge print cannot dominate).
fn encode(features: &MetaFeatures) -> Vec<f64> {
    let mut out = vec![0.0; feature_names().len()];
    let mut at = 0usize;
    if let Some(index) = SetupId::ALL
        .iter()
        .position(|setup| *setup == features.setup)
    {
        out[at + index] = 1.0;
    }
    at += SetupId::ALL.len();
    if features.side == Side::Short {
        out[at + 1] = 1.0;
    } else {
        out[at] = 1.0;
    }
    at += 2;
    out[at] = f64::from(features.agrees_calm);
    at += 1;
    out[at] = features.relative_volume.map_or(1.0, |value| value.min(5.0));
    out[at + 1] = f64::from(features.relative_volume.is_none());
    at += 2;
    out[at] = features.atr_percentile;
    out[at + 1] = features.distance_200d_atr;
    out[at + 2] = features.gap_atr;
    out
}

/// One training observation from a replayed trade.
struct Sample {
    session: NaiveDate,
    setup: SetupId,
    side: Side,
    encoded: Vec<f64>,
    hit: bool,
}

/// Fits and scores the model on `pooled` replayed trades (by group, as
/// [`super::signal_day_with`] pools them) of `universe`.
pub fn fit(universe: &[Instrument], pooled: &BTreeMap<String, Vec<Trade>>) -> MetaReport {
    let mut prepared: BTreeMap<&str, Prepared> = BTreeMap::new();
    let mut samples: Vec<Sample> = Vec::new();
    for trades in pooled.values() {
        for trade in trades {
            let Some(instrument) = universe.iter().find(|item| item.key == trade.key) else {
                continue;
            };
            let Some(index) = instrument
                .bars
                .iter()
                .position(|bar| bar.session == trade.signal_session)
            else {
                continue;
            };
            let agrees_calm = Condition::AgreesCalm.admits(trade.regime, trade.side);
            let prepared = prepared
                .entry(instrument.key.as_str())
                .or_insert_with(|| Prepared::new(instrument));
            let Some(features) =
                prepared.features(instrument, index, trade.setup, trade.side, agrees_calm)
            else {
                continue;
            };
            samples.push(Sample {
                session: trade.signal_session,
                setup: trade.setup,
                side: trade.side,
                encoded: encode(&features),
                hit: matches!(trade.exit, Exit::Target1 | Exit::Target2 | Exit::Target3),
            });
        }
    }
    let n = samples.len() as u32;
    let base_rate = if samples.is_empty() {
        0.0
    } else {
        samples.iter().filter(|sample| sample.hit).count() as f64 / samples.len() as f64
    };
    let mut report = MetaReport {
        n,
        base_rate,
        notes: Vec::new(),
        ..MetaReport::default()
    };
    if samples.len() < MIN_TRAIN_TRADES {
        report.notes.push(format!(
            "Needs {MIN_TRAIN_TRADES} replayed trades with features; has {}.",
            samples.len()
        ));
        return report;
    }
    if !(0.0 < base_rate && base_rate < 1.0) {
        report
            .notes
            .push("Every replayed trade has the same outcome; no model.".into());
        return report;
    }
    let Some(latest) = samples.iter().map(|sample| sample.session).max() else {
        return report;
    };
    // Purged walk-forward: four one-year folds ending at the latest signal, with a
    // 35-day embargo (longer than the 20-session hold), so no training trade's outcome
    // overlaps a test fold.
    let mut oos: Vec<(f64, f64, bool)> = Vec::new();
    let mut fold_diffs: Vec<f64> = Vec::new();
    for step in (0..4u32).rev() {
        let end = latest
            .checked_sub_days(Days::new(step as u64 * 365))
            .unwrap_or(NaiveDate::MIN);
        let start = end
            .checked_sub_days(Days::new(365))
            .unwrap_or(NaiveDate::MIN);
        let purge = start
            .checked_sub_days(Days::new(35))
            .unwrap_or(NaiveDate::MIN);
        let train: Vec<&Sample> = samples
            .iter()
            .filter(|sample| sample.session < purge)
            .collect();
        let test: Vec<&Sample> = samples
            .iter()
            .filter(|sample| sample.session >= start && sample.session < end)
            .collect();
        if train.is_empty() || test.is_empty() {
            continue;
        }
        let fold_rate =
            train.iter().filter(|sample| sample.hit).count() as f64 / train.len() as f64;
        // Base rate by setup and side: a model that only re-learns which setup hits more
        // often has not earned weight over what the scoreboard already shows.
        let mut cell_rates: BTreeMap<(SetupId, Side), (u32, u32)> = BTreeMap::new();
        for sample in &train {
            let entry = cell_rates.entry((sample.setup, sample.side)).or_default();
            entry.0 += 1;
            entry.1 += u32::from(sample.hit);
        }
        if let Some(model) = fit_rows(
            train
                .iter()
                .map(|sample| (sample.encoded.as_slice(), sample.hit)),
        ) {
            let mut fold_brier = 0.0;
            let mut fold_base = 0.0;
            for sample in &test {
                let probability = model.predict_encoded(&sample.encoded);
                let base = cell_rates
                    .get(&(sample.setup, sample.side))
                    .filter(|(count, _)| *count > 0)
                    .map_or(fold_rate, |(count, hits)| {
                        f64::from(*hits) / f64::from(*count)
                    });
                fold_brier += (probability - f64::from(sample.hit)).powi(2);
                fold_base += (base - f64::from(sample.hit)).powi(2);
                oos.push((probability, base, sample.hit));
            }
            fold_diffs.push(fold_brier / test.len() as f64 - fold_base / test.len() as f64);
        }
    }
    if oos.is_empty() {
        report
            .notes
            .push("No fold had both training and test trades; no out-of-sample score.".into());
        return report;
    }
    let brier = oos
        .iter()
        .map(|(p, _, hit)| (p - f64::from(*hit)).powi(2))
        .sum::<f64>()
        / oos.len() as f64;
    let base_brier = oos
        .iter()
        .map(|(_, rate, hit)| (rate - f64::from(*hit)).powi(2))
        .sum::<f64>()
        / oos.len() as f64;
    // Paired by fold: each fold's model Brier minus its base-rate Brier. The fold is the
    // unit, so many correlated trades in one fold cannot make a spurious edge look
    // significant.
    let mean_diff = if fold_diffs.is_empty() {
        0.0
    } else {
        fold_diffs.iter().sum::<f64>() / fold_diffs.len() as f64
    };
    let se = if fold_diffs.len() >= 2 {
        let variance = fold_diffs
            .iter()
            .map(|value| (value - mean_diff).powi(2))
            .sum::<f64>()
            / (fold_diffs.len() - 1) as f64;
        (variance / fold_diffs.len() as f64).sqrt()
    } else {
        f64::INFINITY
    };
    report.brier = Some(brier);
    report.base_brier = Some(base_brier);
    report.brier_diff_se = Some(se);
    if mean_diff + 2.0 * se < 0.0 && base_brier - brier >= 0.01 {
        report.model = fit_rows(
            samples
                .iter()
                .map(|sample| (sample.encoded.as_slice(), sample.hit)),
        );
        report.notes.push(format!(
            "Out-of-sample Brier {brier:.4} beats the base rate's {base_brier:.4} by more than two standard errors ({se:.4}) and at least 0.01; the model may shrink risk."
        ));
    } else {
        report.notes.push(format!(
            "Out-of-sample Brier {brier:.4} does not beat the base rate's {base_brier:.4} by two standard errors ({se:.4}) and 0.01; no weight."
        ));
    }
    report
}

/// Fits a standardized logistic model on encoded rows. `None` when the rows are empty
/// or a feature has no spread.
fn fit_rows<'a>(rows: impl Iterator<Item = (&'a [f64], bool)>) -> Option<MetaModel> {
    let rows: Vec<(&[f64], bool)> = rows.collect();
    if rows.is_empty() {
        return None;
    }
    let width = rows[0].0.len();
    if rows.iter().any(|(row, _)| row.len() != width) {
        return None;
    }
    let count = rows.len() as f64;
    let mut mean = vec![0.0; width];
    for (row, _) in &rows {
        for (index, value) in row.iter().enumerate() {
            mean[index] += value / count;
        }
    }
    let mut sd = vec![0.0; width];
    for (row, _) in &rows {
        for (index, value) in row.iter().enumerate() {
            sd[index] += (value - mean[index]).powi(2) / count;
        }
    }
    for value in &mut sd {
        *value = value.sqrt();
    }
    let standardized: Vec<Vec<f64>> = rows
        .iter()
        .map(|(row, _)| {
            row.iter()
                .enumerate()
                .map(|(index, value)| {
                    if sd[index] > 1e-12 {
                        (value - mean[index]) / sd[index]
                    } else {
                        0.0
                    }
                })
                .collect()
        })
        .collect();
    let labels: Vec<f64> = rows.iter().map(|(_, hit)| f64::from(*hit)).collect();
    let mut coefficients = vec![0.0; width];
    let mut intercept = 0.0;
    // Fixed a priori: 100 batch-gradient steps (to keep a 550-name run under a minute),
    // learning rate 0.1, L2 0.01.
    for _ in 0..100 {
        let mut gradient = vec![0.0; width];
        let mut bias_gradient = 0.0;
        for (row, label) in standardized.iter().zip(&labels) {
            let mut z = intercept;
            for (index, value) in row.iter().enumerate() {
                z += coefficients[index] * value;
            }
            let error = sigmoid(z) - label;
            bias_gradient += error / count;
            for (index, value) in row.iter().enumerate() {
                gradient[index] += error * value / count;
            }
        }
        for (index, value) in coefficients.iter_mut().enumerate() {
            *value -= 0.1 * (gradient[index] + 0.01 * *value);
        }
        intercept -= 0.1 * bias_gradient;
    }
    let scaling: Vec<(f64, f64)> = mean.into_iter().zip(sd).collect();
    Some(MetaModel {
        features: feature_names(),
        coefficients,
        intercept,
        scaling,
    })
}

fn sigmoid(value: f64) -> f64 {
    let value = value.clamp(-30.0, 30.0);
    1.0 / (1.0 + (-value).exp())
}

impl MetaModel {
    /// Probability that this firing reaches 1R before its stop.
    pub fn predict(&self, features: &MetaFeatures) -> f64 {
        self.predict_encoded(&encode(features))
    }

    fn predict_encoded(&self, encoded: &[f64]) -> f64 {
        if encoded.len() != self.coefficients.len() || self.scaling.len() != encoded.len() {
            return 0.5;
        }
        let mut z = self.intercept;
        for (index, value) in encoded.iter().enumerate() {
            let (mean, sd) = self.scaling[index];
            let sd = if sd > 1e-12 { sd } else { 1.0 };
            z += self.coefficients[index] * ((value - mean) / sd);
        }
        sigmoid(z)
    }
}

/// Scores `cards` and `candidates` with `report`'s model: sets [`SignalCard::meta`],
/// scales risk (and size) by the factor, and re-orders each list by `p_hit` within its
/// tier. Leaves everything unchanged when `report.model` is `None`.
pub fn apply(
    report: &MetaReport,
    universe: &[Instrument],
    cards: &mut [SignalCard],
    candidates: &mut [SignalCard],
) {
    let Some(model) = &report.model else {
        return;
    };
    let mut prepared: BTreeMap<&str, Prepared> = BTreeMap::new();
    for list in [cards, candidates] {
        for card in list.iter_mut() {
            let Some(instrument) = universe.iter().find(|item| item.key == card.key) else {
                continue;
            };
            let Some(index) = instrument
                .bars
                .iter()
                .position(|bar| bar.session == card.signal_session)
            else {
                continue;
            };
            let agrees_calm = card.verdict.condition == Condition::AgreesCalm;
            let prepared = prepared
                .entry(instrument.key.as_str())
                .or_insert_with(|| Prepared::new(instrument));
            let Some(features) =
                prepared.features(instrument, index, card.setup, card.side, agrees_calm)
            else {
                continue;
            };
            let p_hit = model.predict(&features);
            if !p_hit.is_finite() {
                continue;
            }
            let size_factor = p_hit.clamp(MIN_SIZE_FACTOR, 1.0);
            card.meta = Some(MetaScore {
                p_hit,
                base_rate: report.base_rate,
                size_factor,
            });
            card.risk_pct *= size_factor;
            if let Some((units, value)) = card.size {
                card.size = Some((units * size_factor, value * size_factor));
            }
        }
        // Re-rank within the tier; a card without a model view stays at the end.
        list.sort_by(|left, right| {
            right
                .meta
                .as_ref()
                .map_or(f64::NEG_INFINITY, |meta| meta.p_hit)
                .total_cmp(
                    &left
                        .meta
                        .as_ref()
                        .map_or(f64::NEG_INFINITY, |meta| meta.p_hit),
                )
        });
    }
}
