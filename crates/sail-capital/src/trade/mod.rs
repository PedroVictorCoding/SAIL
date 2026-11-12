//! Short-term trade signals (round 13, the EPIC's `TradePolicyProposal`, Kilo's slice).
//!
//! The owner wants near-daily long and short ideas, each with an entry zone, a stop loss
//! and profit targets, like a signals channel. The difference here is that every signal
//! carries its own verified track record.
//!
//! A **setup** is a fixed rule on daily bars that may fire on a session. When it fires,
//! the **plan** is fixed too:
//! - entry at the next session's open, if the open is inside the entry zone (otherwise
//!   no trade);
//! - a stop at `k_sl × ATR(14)` from the entry;
//! - three targets at 1R, 2R and 3R (R = the distance from entry to stop);
//! - a time stop after `max_sessions`.
//!
//! The **simulator** replays every past firing as a real trade:
//! - a third of the position is closed at each target;
//! - after target 1 the stop moves to breakeven;
//! - the stop is checked before targets within the same bar (conservative);
//! - costs are spread, FX conversion and overnight financing for CFDs.
//!
//! **Selection.** A setup and side may publish signals only if, over trades that closed
//! before the evaluation date:
//! - it has at least [`MIN_TRADES`] trades;
//! - the lower 95% bound of its expectancy (in R, after costs) is above zero;
//! - it also passes the **holdout**: the same rule judged on the last two years alone, when
//!   the choice was made on the years before.
//!
//! Ranking uses that lower bound, so "most certain" means best worst-case evidence.
//!
//! No look-ahead anywhere. A firing on session `i` uses bars `0..=i` only. Its trade
//! starts at `i + 1`. Selection at date `d` uses trades whose exit is on or before `d`.
//!
//! **Purity.** No I/O and no clock. Prices are `f64` here (statistics, not ledger money).
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Fewest closed trades before a setup and side can be judged.
pub const MIN_TRADES: u32 = 100;

/// Years of history replayed.
pub const LOOKBACK_YEARS: i64 = 8;

/// Most signals published per day (best first).
pub const MAX_SIGNALS_PER_DAY: usize = 8;

/// Long or short.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// Profit when the price rises.
    Long,
    /// Profit when the price falls. At XTB, needs a CFD.
    Short,
}

/// What kind of instrument, which sets costs and whether a CFD is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetClass {
    /// A single stock.
    Stock,
    /// An exchange-traded fund.
    Etf,
    /// A currency pair (CFD at XTB).
    Fx,
    /// A stock index (CFD at XTB).
    Index,
    /// A commodity (CFD at XTB).
    Commodity,
}

/// One daily bar, in the instrument's currency.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    /// Session date.
    pub session: NaiveDate,
    /// Open.
    pub open: f64,
    /// High.
    pub high: f64,
    /// Low.
    pub low: f64,
    /// Close.
    pub close: f64,
}

/// The fixed setups. Each one fires long, short, or not at all on a session. Indicator
/// definitions are standard: Wilder ATR(14) and RSI, simple moving averages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupId {
    /// Close above the 50-day average, which is above the 200; RSI(2) below 10 → long. The
    /// mirror image (below both averages, RSI(2) above 90) → short. Stop 2·ATR, at most
    /// 10 sessions.
    TrendPullback,
    /// Close at a new 20-session high, after the 20-session range was the narrowest of
    /// the last 60 (volatility squeeze) → long. The mirror image with a new low → short.
    /// Stop 1.5·ATR, at most 15 sessions.
    SqueezeBreakout,
    /// Close above the 55-session high (Donchian) with the 200-day average rising →
    /// long. The mirror image → short. Stop 2·ATR, at most 20 sessions.
    Donchian55,
    /// Close below the lower 20-day Bollinger band (2σ) while above the 200-day
    /// average → long. Above the upper band while below the 200 → short. Stop 1.5·ATR,
    /// at most 5 sessions.
    BollingerReversion,
    /// An inside day (its range inside the previous day's) in the direction of the
    /// 20-day average's slope. The trade fires when the next open is beyond the inside
    /// bar's high (long) or low (short). Stop 1·ATR, at most 5 sessions.
    InsideBarBreak,
    /// Three lower closes in a row with the close above the 200-day average → long.
    /// Three higher closes below the 200 → short. Stop 2·ATR, at most 5 sessions.
    ThreeDayReversal,
}

impl SetupId {
    /// Every setup, in report order.
    pub const ALL: [SetupId; 6] = [
        SetupId::TrendPullback,
        SetupId::SqueezeBreakout,
        SetupId::Donchian55,
        SetupId::BollingerReversion,
        SetupId::InsideBarBreak,
        SetupId::ThreeDayReversal,
    ];

    /// Stop distance in ATRs, and the most sessions a trade stays open.
    pub fn plan(self) -> (f64, u32) {
        match self {
            SetupId::TrendPullback => (2.0, 10),
            SetupId::SqueezeBreakout => (1.5, 15),
            SetupId::Donchian55 => (2.0, 20),
            SetupId::BollingerReversion => (1.5, 5),
            SetupId::InsideBarBreak => (1.0, 5),
            SetupId::ThreeDayReversal => (2.0, 5),
        }
    }
}

/// Costs, as fractions of notional unless noted.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Costs {
    /// Round-trip spread and commission.
    pub round_trip: f64,
    /// Round-trip currency conversion when the instrument isn't in the account currency
    /// and isn't a CFD (XTB charges on conversion).
    pub fx_conversion: f64,
    /// Overnight financing per calendar day for a CFD position (long or short).
    pub cfd_daily_financing: f64,
}

impl Costs {
    /// Conservative defaults per class, so real costs aren't worse than assumed.
    pub fn default_for(class: AssetClass) -> Self {
        match class {
            AssetClass::Fx => Costs {
                round_trip: 0.0002,
                fx_conversion: 0.0,
                cfd_daily_financing: 0.00008,
            },
            AssetClass::Index => Costs {
                round_trip: 0.0003,
                fx_conversion: 0.0,
                cfd_daily_financing: 0.00015,
            },
            AssetClass::Commodity => Costs {
                round_trip: 0.0008,
                fx_conversion: 0.0,
                cfd_daily_financing: 0.00015,
            },
            AssetClass::Stock | AssetClass::Etf => Costs {
                round_trip: 0.001,
                fx_conversion: 0.01,
                cfd_daily_financing: 0.0002,
            },
        }
    }
}

/// One instrument in the signal universe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Instrument {
    /// Ledger instrument key, or `watch:<SYMBOL>` for watchlist-only instruments.
    pub key: String,
    /// The symbol as XTB shows it.
    pub symbol: String,
    /// Asset class.
    pub class: AssetClass,
    /// Trades in a currency other than the account's.
    pub foreign_currency: bool,
    /// Daily bars, oldest first, sessions strictly increasing.
    pub bars: Vec<Bar>,
    /// Sessions on which the company reports earnings, when known (past and next).
    pub earnings: Vec<NaiveDate>,
}

impl Instrument {
    /// Whether a trade on this side needs a CFD at XTB. Shorts always do; FX, indices
    /// and commodities only trade as CFDs there.
    pub fn needs_cfd(&self, side: Side) -> bool {
        side == Side::Short
            || matches!(
                self.class,
                AssetClass::Fx | AssetClass::Index | AssetClass::Commodity
            )
    }
}

/// How a replayed trade ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exit {
    /// Stopped out before target 1.
    Stop,
    /// Reached target 1, then the rest stopped at breakeven.
    Target1,
    /// Reached target 2, then the rest stopped at breakeven.
    Target2,
    /// Reached all three targets.
    Target3,
    /// The time stop closed what was left.
    Time,
}

/// One replayed trade.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trade {
    /// Instrument key.
    pub key: String,
    /// Setup.
    pub setup: SetupId,
    /// Side.
    pub side: Side,
    /// Session the setup fired.
    pub signal_session: NaiveDate,
    /// Entry session and price.
    pub entry_session: NaiveDate,
    /// Entry price.
    pub entry: f64,
    /// Stop price at entry.
    pub stop: f64,
    /// Exit session (the last partial exit).
    pub exit_session: NaiveDate,
    /// How it ended.
    pub exit: Exit,
    /// Result in R, after costs (1R = the risk at entry).
    pub r_multiple: f64,
}

/// Evidence for one setup and side over a set of trades.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// Setup.
    pub setup: SetupId,
    /// Side.
    pub side: Side,
    /// Closed trades.
    pub trades: u32,
    /// Share that reached target 1 before the stop.
    pub hit_target1: f64,
    /// Wilson 95% interval on `hit_target1`.
    pub hit_target1_ci: (f64, f64),
    /// Mean R after costs.
    pub expectancy_r: f64,
    /// 95% interval on the mean R. It is a normal interval on the mean, with the standard
    /// error inflated by `sqrt(1 + 2·ρ1)`, where `ρ1` is the lag-1 autocorrelation of the
    /// R series in date order (floored at 0).
    pub expectancy_ci: (f64, f64),
    /// Gross wins over gross losses, in R.
    pub profit_factor: f64,
    /// Largest peak-to-trough fall of cumulative R, trades in exit order.
    pub max_drawdown_r: f64,
    /// Mean sessions held.
    pub mean_sessions: f64,
}

/// Whether a setup and side may publish, and why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    /// The asset group the trades were pooled over: `"fx"`, `"stocks"` (stocks and
    /// ETFs) or `"indices"` (indices and commodities).
    pub group: String,
    /// Evidence over the selection period (all closed trades before the holdout).
    pub selection: Evidence,
    /// Evidence over the holdout (the last two years before the evaluation date).
    pub holdout: Evidence,
    /// True when both pass: trades ≥ [`MIN_TRADES`] in selection, a selection lower bound
    /// above 0, and a holdout mean above 0.
    pub publishes: bool,
    /// One plain sentence: why it publishes or not.
    pub reason: String,
}

/// A published signal, as the owner sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignalCard {
    /// Stable id: a hash of key, setup, side and signal session.
    pub id: String,
    /// Instrument key.
    pub key: String,
    /// XTB symbol.
    pub symbol: String,
    /// Asset class.
    pub class: AssetClass,
    /// Side.
    pub side: Side,
    /// Setup.
    pub setup: SetupId,
    /// The session the setup fired (the latest close).
    pub signal_session: NaiveDate,
    /// Enter at the next open only between these prices (±0.25·ATR around the close).
    pub entry_zone: (f64, f64),
    /// Stop loss.
    pub stop: f64,
    /// Targets 1R, 2R and 3R from the middle of the entry zone.
    pub targets: [f64; 3],
    /// Close what is left after this many sessions.
    pub max_sessions: u32,
    /// Units to trade so that hitting the stop loses `risk_pct` of the account, and that
    /// position's value, in the account currency. `None` when the account value or the
    /// FX rate is unknown.
    pub size: Option<(f64, f64)>,
    /// Percent of the account at risk to the stop.
    pub risk_pct: f64,
    /// Needs a CFD at XTB (leverage and overnight financing).
    pub needs_cfd: bool,
    /// The evidence behind it.
    pub verdict: Verdict,
    /// Ranking score: the selection lower bound of expectancy in R.
    pub score: f64,
    /// How it relates to what the owner holds: e.g. "you hold this; a long adds to it",
    /// or "you hold this; a short works as a hedge on a CFD".
    pub relation: Option<String>,
    /// Warnings, e.g. "earnings on 2026-11-20, inside the trade window".
    pub warnings: Vec<String>,
}

/// The day's signals and the scoreboard behind them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignalDay {
    /// The latest session across the universe.
    pub session: NaiveDate,
    /// Published cards, best first, at most [`MAX_SIGNALS_PER_DAY`].
    pub cards: Vec<SignalCard>,
    /// Every setup and side, with its verdict, publishing or not.
    pub scoreboard: Vec<Verdict>,
    /// Setups that fired today but don't publish, as `(symbol, setup, side)`.
    pub suppressed: Vec<(String, SetupId, Side)>,
    /// Plain notes on the limits: survivorship, daily bars only, costs assumed.
    pub notes: Vec<String>,
}

/// A protective stop and view for one held position.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HoldingGuard {
    /// Instrument key.
    pub key: String,
    /// Symbol.
    pub symbol: String,
    /// Latest close.
    pub close: f64,
    /// A trailing stop: the highest close of the last 22 sessions minus 3·ATR(22)
    /// (chandelier exit).
    pub trailing_stop: f64,
    /// Distance from the close to the stop, percent.
    pub stop_distance_pct: f64,
    /// The close is below the trailing stop (the trend that held it has broken).
    pub stop_hit: bool,
    /// Above or below the 200-day average, if known.
    pub above_200d: Option<bool>,
    /// The next earnings date, if known and in the future.
    pub next_earnings: Option<NaiveDate>,
}

/// Every firing of `setup` on the instrument's bars: `(session index, side)`, using only
/// bars up to that index.
pub fn fire(setup: SetupId, bars: &[Bar]) -> Vec<(usize, Side)> {
    // Kilo implements this; the stub keeps the crate building.
    let _ = (setup, bars);
    Vec::new()
}

/// Replays every firing of every setup on `instrument` as trades with `costs`. Trades
/// still open at the last bar are left out.
pub fn replay(instrument: &Instrument, costs: Costs) -> Vec<Trade> {
    // Kilo implements this; the stub keeps the crate building.
    let _ = (instrument, costs);
    Vec::new()
}

/// Evidence over `trades` (any mix of instruments) for one setup and side.
pub fn evidence(setup: SetupId, side: Side, trades: &[Trade]) -> Evidence {
    // Kilo implements this; the stub keeps the crate building.
    let _ = trades;
    Evidence {
        setup,
        side,
        trades: 0,
        hit_target1: 0.0,
        hit_target1_ci: (0.0, 0.0),
        expectancy_r: 0.0,
        expectancy_ci: (0.0, 0.0),
        profit_factor: 0.0,
        max_drawdown_r: 0.0,
        mean_sessions: 0.0,
    }
}

/// The group an asset class pools with (see [`Verdict::group`]).
pub fn group_of(class: AssetClass) -> &'static str {
    match class {
        AssetClass::Fx => "fx",
        AssetClass::Stock | AssetClass::Etf => "stocks",
        AssetClass::Index | AssetClass::Commodity => "indices",
    }
}

/// The verdict for one setup and side at `as_of`, from trades that exited on or before
/// it. The holdout is the two years before `as_of`, and selection is everything earlier.
pub fn verdict(
    group: &str,
    setup: SetupId,
    side: Side,
    trades: &[Trade],
    as_of: NaiveDate,
) -> Verdict {
    let _ = as_of;
    let e = evidence(setup, side, trades);
    Verdict {
        group: group.into(),
        selection: e.clone(),
        holdout: e,
        publishes: false,
        reason: "not implemented".into(),
    }
}

/// Today's signals.
/// - Replay every instrument and pool trades **per asset class group**: FX alone; stocks
///   and ETFs together; indices and commodities together.
/// - Judge each setup, side and group at the latest session.
/// - Build a card for every setup that fired on its instrument's latest session and
///   publishes in that group.
/// - Drop a card when earnings fall inside its window (stocks and ETFs).
/// - Rank by score, keep the best [`MAX_SIGNALS_PER_DAY`], and at most one per instrument.
///
/// `account_value` and `risk_pct` size the cards. `fx_to_account` converts one unit of
/// an instrument's currency to the account currency (`None` if unknown). `held` maps keys
/// the owner holds to a short description used in `relation`.
pub fn signal_day(
    universe: &[Instrument],
    account_value: Option<f64>,
    risk_pct: f64,
    fx_to_account: &dyn Fn(&Instrument) -> Option<f64>,
    held: &dyn Fn(&str) -> Option<String>,
) -> SignalDay {
    // Kilo implements this; the stub keeps the crate building.
    let _ = (universe, account_value, risk_pct, fx_to_account, held);
    SignalDay {
        session: NaiveDate::MIN,
        cards: Vec::new(),
        scoreboard: Vec::new(),
        suppressed: Vec::new(),
        notes: Vec::new(),
    }
}

/// A protective trailing stop for a held instrument.
pub fn guard(instrument: &Instrument) -> Option<HoldingGuard> {
    // Kilo implements this; the stub keeps the crate building.
    let _ = instrument;
    None
}

/// Grades a published card against the bars after it: the same rules as `replay`.
/// `None` while the trade is still open or never filled.
pub fn grade(card: &SignalCard, bars_after: &[Bar], costs: Costs) -> Option<Trade> {
    // Kilo implements this; the stub keeps the crate building.
    let _ = (card, bars_after, costs);
    None
}
