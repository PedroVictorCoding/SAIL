//! Round 24: market regime, for conditioning setups (Kilo's slice, A3). **This module doc
//! is the spec.**
//!
//! A regime is read from a **reference series** at the close of each session:
//! - `trend`: [`Trend::Up`] when the close is above its 200-session simple average, else
//!   [`Trend::Down`];
//! - `vol`: [`Vol::Stressed`] when the annualisation-free 20-session realized volatility
//!   (standard deviation of daily log returns) is above the 80th percentile of its own
//!   values over the previous 252 sessions (that session included), else [`Vol::Calm`].
//!
//! Both need 200 and 20 + 252 sessions of history; before that the regime is `None`.
//! Thresholds are fixed a priori (200-day trend and an 80th-percentile volatility cut are
//! the conventional choices; nothing was tuned on the owner's data).
//!
//! **Reference** ([`reference_for`]): stocks and ETFs listed in the US use
//! [`US_REFERENCE`], European listings [`EU_REFERENCE`], others none; indices and
//! commodities use their own bars; FX has none (unconditioned).
//!
//! A reference session maps to an instrument session by date: the instrument's regime on
//! session `t` is the reference's regime on the latest reference session `≤ t`, and only
//! if that reference session is at most 5 calendar days older. No look-ahead.
//!
//! Pure: no I/O, no clock.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{AssetClass, Bar, Instrument, Region, Side, Trade};

/// Reference key for US listings (the S&P 500 ETF, in the default watchlist).
pub const US_REFERENCE: &str = "SPY";
/// Reference key for European listings (the Euro Stoxx 50 index).
pub const EU_REFERENCE: &str = "^STOXX50E";

/// Above or below the 200-session average.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trend {
    /// Close above its 200-session average.
    Up,
    /// Close at or below it.
    Down,
}

/// Whether recent volatility is high for this market.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vol {
    /// At or below the 80th percentile of the past year.
    Calm,
    /// Above it.
    Stressed,
}

/// The regime on one session. JSON: `{"trend": "up"|"down", "vol": "calm"|"stressed"}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Regime {
    /// Trend.
    pub trend: Trend,
    /// Volatility.
    pub vol: Vol,
}

/// Which firings a verdict judges. JSON: `"none"` or `"agrees_calm"`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    /// Every firing.
    #[default]
    None,
    /// Only firings whose regime is known, calm, and trending with the side (Up for a
    /// long, Down for a short).
    AgreesCalm,
}

impl Condition {
    /// Every condition, in scoreboard order.
    pub const ALL: [Condition; 2] = [Condition::None, Condition::AgreesCalm];

    /// Whether a firing on `side` in `regime` belongs to this condition's trades.
    pub fn admits(self, regime: Option<Regime>, side: Side) -> bool {
        match self {
            Condition::None => true,
            Condition::AgreesCalm => regime.is_some_and(|r| {
                r.vol == Vol::Calm
                    && match side {
                        Side::Long => r.trend == Trend::Up,
                        Side::Short => r.trend == Trend::Down,
                    }
            }),
        }
    }
}

/// Where an instrument's regime comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reference {
    /// Its own bars (indices and commodities).
    Own,
    /// The reference series with this key.
    Key(&'static str),
    /// No regime (FX, and stocks listed outside the US and Europe).
    Unconditioned,
}

/// The reference for an instrument (see the module doc).
pub fn reference_for(instrument: &Instrument) -> Reference {
    match instrument.class {
        AssetClass::Index | AssetClass::Commodity => Reference::Own,
        AssetClass::Fx => Reference::Unconditioned,
        AssetClass::Stock | AssetClass::Etf => match instrument.context.region {
            Region::Us => Reference::Key(US_REFERENCE),
            Region::Eu => Reference::Key(EU_REFERENCE),
            Region::Other => Reference::Unconditioned,
        },
    }
}

/// Kilo, round 24, *pure*: the regime at each of `bars`' sessions (module doc). M0 stub:
/// all `None`.
pub fn series(bars: &[Bar]) -> Vec<Option<Regime>> {
    vec![None; bars.len()]
}

/// Kilo, round 24, *pure*: the regime at each of the instrument's sessions, aligned with
/// `instrument.bars`, from its [`reference_for`] (`references` holds the keyed series).
/// M0 stub: all `None`.
pub fn for_instrument(
    instrument: &Instrument,
    references: &HashMap<String, Vec<Bar>>,
) -> Vec<Option<Regime>> {
    let _ = references;
    vec![None; instrument.bars.len()]
}

/// Kilo, round 24, *pure*: sets each trade's `regime` to the instrument's regime on its
/// `signal_session` ([`for_instrument`]). M0 stub: leaves them `None`.
pub fn tag(trades: &mut [Trade], instrument: &Instrument, references: &HashMap<String, Vec<Bar>>) {
    let _ = (trades, instrument, references);
}
