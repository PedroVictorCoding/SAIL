//! Asset outlook (round 12, CAP-17/22): one blended forecast per holding, drawn as a
//! fan (past prices, then a widening band of likely prices), with a **replay** that cuts
//! off a recent period and shows what SAIL would have predicted against what happened.
//!
//! **Components.** Each gives a full distribution of the log return at every step up to
//! the horizon, as quantiles ([`LEVELS`] at [`steps`]):
//! - `gbm`: geometric Brownian motion, closed form. Drift and volatility come from the
//!   last 252 daily log returns. Quantile `q` at step `t` is `μt + z_q·σ√t`, where
//!   `μ` is the mean daily log return.
//! - `ewma`: zero drift, RiskMetrics EWMA volatility (λ = 0.94), `z_q·σ√t`.
//! - `historical`: historical simulation. The empirical quantiles, at each step `t`, of
//!   every overlapping `t`-session log return in the last 756 sessions before the origin.
//!   Cheap, deterministic, and keeps fat tails and skew.
//! - `chronos`: Chronos-Bolt quantile path, from a Python helper. Only up to 64 steps.
//! - `gbdt`: gradient-boosted quantile regression (scikit-learn
//!   `HistGradientBoostingRegressor`, `loss="quantile"`), from a Python helper. It is
//!   trained only on rows whose outcome was known by the origin, pooled over the signal
//!   universe (holdings plus the watchlist's stocks and ETFs), with features from past prices: returns over 5/21/63/126/252 sessions,
//!   21- and 63-session volatility, distance from the 50/200 averages, drawdown from the
//!   252 high, and RSI(14).
//!
//! **Blend.** For each horizon the components are pooled linearly,
//! `F(x) = Σ_m w_m F_m(x)`. The weights lie on the simplex and minimize the mean CRPS of
//! the pool on walk-forward forecasts made **before the origin**: the last 156 weekly
//! origins whose outcome was known by then. CRPS is approximated by the mean pinball loss
//! over 99 levels read from each component's piecewise-linear CDF. A component without a
//! value at an origin is left out of that origin, and the weights are renormalized. With
//! fewer than 20 scored origins the weights are equal.
//!
//! **Direction tilt.** If signals passed validation (round 11, now including YOLO
//! trend/patterns), the pooled distribution is shifted by `α_j(h)` from
//! `signals::CombinedScore`, scaled linearly in steps (`α·t/h`). If nothing passed, there
//! is no shift.
//!
//! **Replay.** Exactly the same computation, with every input cut at `cutoff`: prices,
//! component values, weights and signal validation. The actual prices after the cutoff
//! are then laid over the fan. Nothing after the cutoff is visible to any part of it.
//!
//! **Cost.** Component paths at weekly origins depend only on the instrument and the
//! origin, never on the cutoff. Compute them once per call and reuse them for every
//! cutoff's weights. A track record over 36 instruments must take seconds, not minutes.
//!
//! **Tails.** Pooled quantiles are found by bisection on the mixture CDF. Each component's
//! CDF is piecewise linear through its quantiles. Beyond 5% and 95% it uses a normal tail
//! fitted to the 16–84% spread.
//!
//! **Track record.** Replays at monthly cutoffs over the last three years, reporting:
//! - how often the actual price ended inside the 68% and 90% bands;
//! - the average percentile the actual landed at (50 is unbiased);
//! - CRPS skill against `ewma` alone, which acts as "no change".
//!
//! **Purity.** No I/O and no clock. Component values from Python helpers are passed in.
mod dist;
mod engine;
mod fit;
mod paths;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Horizons offered, in sessions: about 1 month, 3 months, 6 months and 1 year.
pub const HORIZONS: [u32; 4] = [21, 63, 126, 252];

/// Quantile levels reported: the 90% band (5–95), the 68% band (16–84) and the median.
pub const LEVELS: [f64; 5] = [0.05, 0.16, 0.5, 0.84, 0.95];

/// Steps (sessions after the origin) at which quantiles are given for a horizon: every
/// 5 sessions, and always the horizon itself.
pub fn steps(horizon: u32) -> Vec<u32> {
    let mut out: Vec<u32> = (1..=horizon / 5).map(|k| k * 5).collect();
    if out.last() != Some(&horizon) {
        out.push(horizon);
    }
    out
}

/// A forecast component.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentId {
    /// Geometric Brownian motion.
    Gbm,
    /// Zero drift, EWMA volatility (the "no change" reference).
    Ewma,
    /// Historical simulation over past windows of the same length.
    Historical,
    /// Chronos-Bolt (Python helper).
    Chronos,
    /// Gradient-boosted quantile regression (Python helper).
    Gbdt,
}

/// Quantiles of the log return at each step, for one component, instrument and origin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PathQuantiles {
    /// Steps, increasing (from [`steps`]).
    pub steps: Vec<u32>,
    /// `values[i][k]`: the log return at `steps[i]` for `LEVELS[k]`, non-decreasing in k.
    pub values: Vec<[f64; 5]>,
}

/// One component's forecast from a Python helper, keyed for the store.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExternalPath {
    /// Component.
    pub component: ComponentId,
    /// Ledger instrument key.
    pub instrument_key: String,
    /// The origin session.
    pub origin: NaiveDate,
    /// Horizon in sessions.
    pub horizon: u32,
    /// The path.
    pub path: PathQuantiles,
}

/// A point on the drawn fan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FanPoint {
    /// Sessions after the origin.
    pub step: u32,
    /// Prices at [`LEVELS`] (5%, 16%, median, 84%, 95%).
    pub prices: [f64; 5],
    /// The mean price (the "average" line), `Σ w_m · E_m[P]`.
    pub mean: f64,
}

/// The blended outlook for one instrument, origin and horizon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outlook {
    /// Ledger instrument key.
    pub instrument_key: String,
    /// Broker symbol.
    pub symbol: String,
    /// The origin session (the cutoff for a replay).
    pub origin: NaiveDate,
    /// Close at the origin.
    pub origin_close: f64,
    /// Horizon in sessions.
    pub horizon: u32,
    /// Ok, or why not.
    pub status: crate::forecast::ForecastStatus,
    /// The fan, by step.
    pub fan: Vec<FanPoint>,
    /// Weights used, by component (zero-weight components included).
    pub weights: Vec<(ComponentId, f64)>,
    /// Scored origins behind the weights.
    pub weight_origins: u32,
    /// The direction tilt applied at the horizon (log return), 0 when none.
    pub tilt: f64,
    /// Signals behind the tilt, e.g. `"chart_trend h63 t=2.1"`.
    pub tilt_signals: Vec<String>,
    /// For a replay: actual closes after the origin, up to the horizon or today.
    pub actual: Vec<(NaiveDate, f64)>,
    /// For a replay that reached the horizon: the percentile the actual close landed at.
    pub actual_percentile: Option<f64>,
}

/// Out-of-sample record of the blend at one horizon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackRecord {
    /// Horizon in sessions.
    pub horizon: u32,
    /// Replays scored (monthly cutoffs, last three years, outcome known).
    pub replays: u32,
    /// Share of actual closes inside the 68% band (aim: 0.68).
    pub inside_68: f64,
    /// Share inside the 90% band (aim: 0.90).
    pub inside_90: f64,
    /// Mean percentile of the actual close (aim: 50).
    pub mean_percentile: f64,
    /// `1 − CRPS(blend) / CRPS(ewma)` on the same replays.
    pub skill_vs_no_change: f64,
    /// Share of replays where the median got the direction right (non-zero outcomes).
    pub direction_hit_rate: f64,
}

/// One instrument's input: its closes (oldest first, the same as `forecast`), plus any
/// component paths from Python helpers at past and present origins.
#[derive(Clone, Debug)]
pub struct Instrument<'a> {
    /// The closes.
    pub series: crate::forecast::CloseSeries<'a>,
    /// Helper paths for this instrument (any origins and horizons). A helper may give
    /// only the horizon step. Missing steps are filled from it: the median scales by
    /// `t/h` and each spread from the median by `sqrt(t/h)`.
    pub external: Vec<ExternalPath>,
    /// The direction tilt at each horizon, known at `cutoff` (from validated signals).
    pub tilt: Vec<(u32, f64)>,
    /// Signals behind the tilt, for display.
    pub tilt_signals: Vec<String>,
}

/// The built-in components (`gbm`, `ewma`, `historical`) for the series cut at index
/// `origin` (inclusive), or `None` for each when there are fewer than 252 returns.
pub fn builtin_paths(
    series: &crate::forecast::CloseSeries<'_>,
    origin: usize,
    horizon: u32,
) -> Vec<(ComponentId, Option<PathQuantiles>)> {
    let Ok(series) = paths::Series::new(series.closes) else {
        return vec![
            (ComponentId::Gbm, None),
            (ComponentId::Ewma, None),
            (ComponentId::Historical, None),
        ];
    };
    paths::builtin_paths(&series, origin, horizon).to_vec()
}

/// The blended outlook at `horizon` from `cutoff` (the last session on or before it), or
/// from the latest close when `cutoff` is `None`. Nothing after the cutoff is used. When
/// the cutoff is in the past, `actual` and `actual_percentile` are filled from the closes
/// after it.
pub fn outlook(instrument: &Instrument<'_>, horizon: u32, cutoff: Option<NaiveDate>) -> Outlook {
    engine::outlook_with_calendar(
        instrument,
        horizon,
        cutoff,
        &crate::calendar::WeekdayCalendar,
    )
}

/// Like [`outlook`], using `calendar` for origin selection and session labels.
pub fn outlook_with_calendar(
    instrument: &Instrument<'_>,
    horizon: u32,
    cutoff: Option<NaiveDate>,
    calendar: &dyn crate::calendar::ExchangeCalendar,
) -> Outlook {
    engine::outlook_with_calendar(instrument, horizon, cutoff, calendar)
}

/// The blend's out-of-sample record at `horizon`: replays at the last session of each
/// month over the three years before `today`, pooled over `instruments`, keeping those
/// whose outcome is known. The instruments' `tilt` is ignored: it is known only at the
/// caller's cutoff, and a replay must not see it.
pub fn track_record(instruments: &[Instrument<'_>], horizon: u32, today: NaiveDate) -> TrackRecord {
    engine::track_record(instruments, horizon, today)
}
