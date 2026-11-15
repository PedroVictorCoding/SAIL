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
//! features, L2 penalty, fitted by Newton or gradient steps to convergence. It is
//! scored with the same purged walk-forward folds as the setups (no fold trains on a
//! trade whose holding window overlaps the test fold).
//!
//! **Weight.** The model earns weight only when its out-of-sample Brier score beats
//! the base rate's (predicting the training hit rate for everyone) by more than two
//! standard errors of the paired difference. Otherwise [`MetaReport::model`] is
//! `None` and every card is left exactly as it was.
//!
//! **What it may change.** Only the order of cards inside a tier and a size factor in
//! `[MIN_SIZE_FACTOR, 1.0]` that scales a card's risk down. It never creates, removes
//! or upgrades a card, never raises risk, and runs before the heat cap so the cap sees
//! the reduced risk.
//!
//! **Purity.** No I/O and no clock.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Instrument, SetupId, Side, SignalCard, Trade};

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

/// Features for a firing of `setup`/`side` on `instrument` at bar `index` (the signal
/// session), or `None` when there aren't enough bars.
pub fn features(
    instrument: &Instrument,
    index: usize,
    setup: SetupId,
    side: Side,
    agrees_calm: bool,
) -> Option<MetaFeatures> {
    let _ = (instrument, index, setup, side, agrees_calm);
    None
}

/// Fits and scores the model on `pooled` replayed trades (by group, as
/// [`super::signal_day_with`] pools them) of `universe`.
pub fn fit(universe: &[Instrument], pooled: &BTreeMap<String, Vec<Trade>>) -> MetaReport {
    let _ = (universe, pooled);
    MetaReport {
        notes: vec!["Meta-labeling is not built yet.".into()],
        ..MetaReport::default()
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
    let _ = (report, universe, cards, candidates);
}
