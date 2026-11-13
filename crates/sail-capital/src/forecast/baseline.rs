//! Signal v2 baseline features (round 20, shadow record). **This is the frozen feature
//! contract**, not a promoted forecast. It extends the round-10 baselines with the
//! multi-horizon returns and continuous squeeze measurements the v2 plan requires.
//!
//! Every value comes from `bars[..=index]`; the caller chooses the decision session and
//! supplies a point-in-time bar cut. The module is pure: no I/O and no clock.
//!
//! The forecast gate remains unchanged while these features collect a shadow record.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::trade::Bar;

/// Horizons of the v2 baseline, in sessions.
pub const BASELINE_HORIZONS: [u32; 3] = [5, 21, 63];

/// Version of this feature definition. Changing any formula or window bumps it.
pub const FEATURES_VERSION: &str = "capital-baseline-features/1";

/// Fewest bars needed: 252 returns plus the origin bar.
pub const MIN_BASELINE_BARS: usize = 253;

const LOOKBACK: usize = 252;
const EWMA_LAMBDA: f64 = 0.94;

/// The frozen v2 baseline features at one decision session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BaselineFeatures {
    /// Version that produced these numbers.
    pub version: String,
    /// Session of the last bar used.
    pub as_of_session: NaiveDate,
    /// Log return over the last 5 sessions.
    pub return_5: f64,
    /// Log return over the last 21 sessions.
    pub return_21: f64,
    /// Log return over the last 63 sessions.
    pub return_63: f64,
    /// Log return over the last 126 sessions.
    pub return_126: f64,
    /// Log return over the last 252 sessions.
    pub return_252: f64,
    /// Twelve-month return minus the last month (`return_252 - return_21`).
    pub momentum_12_1: f64,
    /// Mean daily log return over the last 252 returns.
    pub trailing_drift_daily: f64,
    /// RiskMetrics EWMA daily log volatility (λ = 0.94).
    pub ewma_volatility_daily: f64,
    /// (20-session high − low) / close at the origin.
    pub squeeze_range_20: f64,
    /// 20-session range divided by the 60-session range (≤ 1 is compressed).
    pub squeeze_compression: f64,
    /// 20-session price range divided by ATR(20); lower is tighter.
    pub squeeze_atr: f64,
    /// Where the close sits in the 20-session range: 0 at the low, 1 at the high.
    pub close_in_range_20: f64,
    /// Bars used (at least [`MIN_BASELINE_BARS`]).
    pub bars: u32,
}

/// Computes the baseline features at the last bar. Bars must be oldest first, strictly
/// increasing in session, with positive finite OHLC values.
pub fn features(bars: &[Bar]) -> Result<BaselineFeatures, String> {
    if bars.len() < MIN_BASELINE_BARS {
        return Err(format!(
            "needs at least {MIN_BASELINE_BARS} bars; has {}",
            bars.len()
        ));
    }
    if bars
        .windows(2)
        .any(|pair| pair[0].session >= pair[1].session)
    {
        return Err("bar sessions must be strictly increasing".into());
    }
    let mut closes = Vec::with_capacity(bars.len());
    for bar in bars {
        let values = [bar.open, bar.high, bar.low, bar.close];
        if values
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
        {
            return Err("a bar had a non-positive or non-finite OHLC value".into());
        }
        if bar.high < bar.low {
            return Err("a bar had a high below its low".into());
        }
        closes.push(bar.close.ln());
    }

    let last = bars.len() - 1;
    let log_return = |sessions: usize| closes[last] - closes[last - sessions];
    let return_5 = log_return(5);
    let return_21 = log_return(21);
    let return_63 = log_return(63);
    let return_126 = log_return(126);
    let return_252 = log_return(252);
    let momentum_12_1 = return_252 - return_21;

    let returns: Vec<f64> = closes.windows(2).map(|pair| pair[1] - pair[0]).collect();
    let trailing = &returns[returns.len() - LOOKBACK..];
    let trailing_drift_daily = trailing.iter().sum::<f64>() / LOOKBACK as f64;
    let mut variance = trailing[0] * trailing[0];
    for value in &trailing[1..] {
        variance = EWMA_LAMBDA * variance + (1.0 - EWMA_LAMBDA) * value * value;
    }
    let ewma_volatility_daily = variance.max(0.0).sqrt();

    let window = &bars[last + 1 - 20..=last];
    let high20 = window
        .iter()
        .map(|bar| bar.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let low20 = window
        .iter()
        .map(|bar| bar.low)
        .fold(f64::INFINITY, f64::min);
    let close = bars[last].close;
    let range20 = high20 - low20;
    let squeeze_range_20 = range20 / close;
    let window60 = &bars[last + 1 - 60..=last];
    let high60 = window60
        .iter()
        .map(|bar| bar.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let low60 = window60
        .iter()
        .map(|bar| bar.low)
        .fold(f64::INFINITY, f64::min);
    let range60 = high60 - low60;
    let squeeze_compression = if range60 > 0.0 {
        range20 / range60
    } else {
        1.0
    };
    let atr20 = atr(&bars[last + 1 - 21..=last]);
    let squeeze_atr = if atr20 > 0.0 { range20 / atr20 } else { 1.0 };
    let close_in_range_20 = if range20 > 0.0 {
        (close - low20) / range20
    } else {
        0.5
    };

    Ok(BaselineFeatures {
        version: FEATURES_VERSION.to_string(),
        as_of_session: bars[last].session,
        return_5,
        return_21,
        return_63,
        return_126,
        return_252,
        momentum_12_1,
        trailing_drift_daily,
        ewma_volatility_daily,
        squeeze_range_20,
        squeeze_compression,
        squeeze_atr,
        close_in_range_20,
        bars: bars.len().min(u32::MAX as usize) as u32,
    })
}

/// ATR over the last `period` sessions of `bars` (which includes one warm-up bar).
fn atr(bars: &[Bar]) -> f64 {
    let mut sum = 0.0;
    let mut count = 0;
    for pair in bars.windows(2) {
        let previous = pair[0].close;
        let bar = pair[1];
        let tr = (bar.high - bar.low)
            .max((bar.high - previous).abs())
            .max((bar.low - previous).abs());
        sum += tr;
        count += 1;
    }
    if count == 0 { 0.0 } else { sum / count as f64 }
}
