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
//! **Candidates.** That bar is strict (it is also corrected for testing 36 setups at
//! once), so on most days nothing clears it. A setup and side that made money after costs
//! in the selection years *and* in the holdout, with enough trades of each, gives
//! [`Tier::Candidate`] cards when it fires: the same entry, stop and targets, sized at
//! [`CANDIDATE_RISK_SHARE`] of the usual risk, and marked as not yet clear of luck.
//! Candidates keep their own live record and never count towards the verified one.
//!
//! No look-ahead anywhere. A firing on session `i` uses bars `0..=i` only. Its trade
//! starts at `i + 1`. Selection at date `d` uses trades whose exit is on or before `d`.
//!
//! **Purity.** No I/O and no clock. Prices are `f64` here (statistics, not ledger money).
pub mod cache;
mod indicators;
pub mod quality;
pub mod select;
mod setups;
mod sim;
mod stats;

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, Days, NaiveDate, Weekday};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Fewest closed trades before a setup and side can be judged.
pub const MIN_TRADES: u32 = 100;

/// Years of history replayed.
pub const LOOKBACK_YEARS: i64 = 8;

/// Most signals published per day (best first).
pub const MAX_SIGNALS_PER_DAY: usize = 8;

/// Fewest closed selection trades before a setup and side can give candidates.
pub const MIN_CANDIDATE_TRADES: u32 = 40;

/// Fewest closed holdout trades before a setup and side can give candidates.
pub const MIN_CANDIDATE_HOLDOUT_TRADES: u32 = 10;

/// Most candidates per day (best first), on instruments without a verified signal.
pub const MAX_CANDIDATES_PER_DAY: usize = 8;

/// Candidates are sized to risk this share of the owner's risk per trade.
pub const CANDIDATE_RISK_SHARE: f64 = 0.5;

/// How much evidence stands behind a card.
///
/// - **Verified** (`Proven`): the full publish bar in the module doc.
/// - **Candidate:** the setup and side made money after costs in the selection years
///   *and* in the holdout, with at least [`MIN_CANDIDATE_TRADES`] and
///   [`MIN_CANDIDATE_HOLDOUT_TRADES`] trades, but isn't yet clear of luck. Candidates are
///   ideas to watch, kept on their own live record so they can earn the full bar in the
///   open. They never count towards the verified record.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Passed the full publish bar.
    #[default]
    Proven,
    /// Positive in both periods, not yet clear of luck.
    Candidate,
}

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
    /// Round 19: a research-universe name. Its trades count as evidence for its group,
    /// but it never gets a card, a candidate or a suppressed entry.
    #[serde(default)]
    pub evidence_only: bool,
    /// Round 19: index membership intervals `[from, to)` (`to = None`: still a member).
    /// A replayed trade counts as evidence only when its `signal_session` falls inside
    /// one. Empty means always.
    #[serde(default)]
    pub membership: Vec<(NaiveDate, Option<NaiveDate>)>,
}

impl Instrument {
    /// Whether a trade signalled on `session` counts as evidence (see `membership`).
    pub fn member_on(&self, session: NaiveDate) -> bool {
        self.membership.is_empty()
            || self
                .membership
                .iter()
                .any(|(from, to)| *from <= session && to.is_none_or(|to| session < to))
    }

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
    /// Doesn't publish, but positive after costs in both periods with enough trades:
    /// its firings become [`Tier::Candidate`] cards.
    #[serde(default)]
    pub candidate: bool,
    /// One plain sentence: why it publishes or not.
    pub reason: String,
    /// Round 19: the statistics behind the verdict ([`select`]). `None` on runs stored
    /// before round 19.
    #[serde(default)]
    pub judgement: Option<select::Judgement>,
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
    /// Verified or candidate. Cards stored before candidates existed are verified.
    #[serde(default)]
    pub tier: Tier,
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
    /// Candidate cards, best first, at most [`MAX_CANDIDATES_PER_DAY`], on instruments
    /// without a verified card.
    #[serde(default)]
    pub candidates: Vec<SignalCard>,
    /// Setups that fired today and give neither a card nor a candidate, as
    /// `(symbol, setup, side)`.
    pub suppressed: Vec<(String, SetupId, Side)>,
    /// Plain notes on the limits: survivorship, daily bars only, costs assumed.
    pub notes: Vec<String>,
    /// Round 19: what the run did. `None` on runs stored before round 19.
    #[serde(default)]
    pub run_stats: Option<RunStats>,
}

/// Round 19: counts and timing of one run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunStats {
    /// Instruments that can get cards.
    pub instruments: u32,
    /// Evidence-only (research) instruments.
    pub evidence_only: u32,
    /// Replayed trades pooled as evidence (after membership filtering).
    pub trades: u32,
    /// Instruments whose replay came from the cache (set by the service).
    pub replay_cache_hits: u32,
    /// Instruments replayed in full (set by the service).
    pub replay_cache_misses: u32,
    /// Wall time of the whole run in milliseconds (set by the service).
    pub run_ms: u64,
}

/// Round 19: everything [`signal_day_with`] needs.
pub struct RunInputs<'a> {
    /// Instruments, including evidence-only ones.
    pub universe: &'a [Instrument],
    /// Replayed trades per instrument key, as [`replay`] (or [`replay_from`] plus the
    /// cached prefix) would give them. Instruments missing here are replayed inline.
    pub replays: &'a HashMap<String, Vec<Trade>>,
    /// Closed live cards, for [`select::judge`].
    pub live: &'a [select::LiveTrade],
    /// Account value in the account currency, for sizing.
    pub account_value: Option<f64>,
    /// Percent of the account at risk per verified trade.
    pub risk_pct: f64,
    /// Units of the account currency per unit of the instrument's currency.
    pub fx_to_account: &'a dyn Fn(&Instrument) -> Option<f64>,
    /// Short description of a held key, used in `relation`.
    pub held: &'a dyn Fn(&str) -> Option<String>,
}

/// A setup that fired on a checked instrument but could not publish, and why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SuppressedSignal {
    /// Setup.
    pub setup: SetupId,
    /// Side.
    pub side: Side,
    /// Plain reason it didn't publish (verdict or earnings window).
    pub reason: String,
}

/// One instrument checked against an existing scoreboard, for a one-off asset search.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstrumentCheck {
    /// Session of the instrument's latest bar.
    pub session: NaiveDate,
    /// Cards that publish for this instrument on its latest bar.
    pub cards: Vec<SignalCard>,
    /// Candidate cards for this instrument on its latest bar.
    #[serde(default)]
    pub candidates: Vec<SignalCard>,
    /// Setups that fired but gave neither.
    pub suppressed: Vec<SuppressedSignal>,
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
    setups::fire(setup, bars)
}

/// Replays every firing of every setup on `instrument` as trades with `costs`. Trades
/// still open at the last bar are left out.
pub fn replay(instrument: &Instrument, costs: Costs) -> Vec<Trade> {
    sim::replay(instrument, costs)
}

/// Round 19: the trades of [`replay`] whose `signal_session >= from`, simulating only
/// those firings (indicators still see every bar). For any `from`,
/// `replay` == (trades of a past `replay` with `signal_session < from`) ++ `replay_from`,
/// provided the bars before `from` are unchanged and `from` is at least
/// [`REPLAY_SAFE_SESSIONS`] sessions before the past replay's last bar. Kilo, round 19:
/// until then this filters a full replay (correct, not faster).
pub fn replay_from(instrument: &Instrument, costs: Costs, from: NaiveDate) -> Vec<Trade> {
    sim::replay(instrument, costs)
        .into_iter()
        .filter(|trade| trade.signal_session >= from)
        .collect()
}

/// Sessions before a replay's last bar from which [`replay_from`] must restart: longer than
/// any setup's `max_sessions` plus the entry session, so every earlier trade had closed.
pub const REPLAY_SAFE_SESSIONS: usize = 25;

/// Evidence over `trades` (any mix of instruments) for one setup and side.
pub fn evidence(setup: SetupId, side: Side, trades: &[Trade]) -> Evidence {
    stats::evidence(setup, side, trades)
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
    stats::verdict(group, setup, side, trades, as_of)
}

/// Today's signals.
/// - Replay every instrument and pool trades **per asset class group**: FX alone; stocks
///   and ETFs together; indices and commodities together.
/// - Judge each setup, side and group at the latest session.
/// - Build a card for every setup that fired on its instrument's latest session and
///   publishes in that group.
/// - Drop a card when earnings fall inside its window (stocks and ETFs).
/// - Rank by score, keep the best [`MAX_SIGNALS_PER_DAY`], and at most one per instrument.
/// - Candidates the same way, up to [`MAX_CANDIDATES_PER_DAY`], skipping instruments that
///   already have a verified card.
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
    signal_day_with(&RunInputs {
        universe,
        replays: &HashMap::new(),
        live: &[],
        account_value,
        risk_pct,
        fx_to_account,
        held,
    })
}

/// Round 19: [`signal_day`] with cached replays, live evidence and research instruments.
/// - The session is the latest bar over instruments that can get cards (all instruments
///   when every one is evidence-only).
/// - Each instrument's trades come from `replays` or an inline [`replay`]; only trades
///   whose `signal_session` passes [`Instrument::member_on`] are pooled.
/// - The scoreboard is [`select::judge`] over the pooled trades and `live`.
/// - Evidence-only instruments never get cards, candidates or suppressed entries.
/// - `run_stats` has `instruments`, `evidence_only` and `trades` filled; the service
///   fills the cache counts and the timing.
pub fn signal_day_with(inputs: &RunInputs) -> SignalDay {
    let universe = inputs.universe;
    let notes = vec![
        "History covers the current watchlist only; names that disappeared are not included."
            .to_string(),
        "Daily bars only; a stop and a target in the same session count as the stop.".to_string(),
        "Costs are assumptions: spread, FX conversion and CFD overnight financing.".to_string(),
    ];
    let tradable = universe.iter().filter(|i| !i.evidence_only);
    let latest = |it: &mut dyn Iterator<Item = &Instrument>| {
        it.filter_map(|instrument| instrument.bars.last().map(|bar| bar.session))
            .max()
    };
    let session = latest(&mut tradable.clone()).or_else(|| latest(&mut universe.iter()));
    let Some(session) = session else {
        return SignalDay {
            session: NaiveDate::MIN,
            cards: Vec::new(),
            candidates: Vec::new(),
            scoreboard: Vec::new(),
            suppressed: Vec::new(),
            notes,
            run_stats: Some(RunStats::default()),
        };
    };

    let mut pooled: std::collections::BTreeMap<String, Vec<Trade>> =
        std::collections::BTreeMap::new();
    let mut pooled_count = 0u32;
    for instrument in universe {
        let inline;
        let trades = match inputs.replays.get(&instrument.key) {
            Some(trades) => trades,
            None => {
                inline = sim::replay(instrument, Costs::default_for(instrument.class));
                &inline
            }
        };
        let entry = pooled
            .entry(group_of(instrument.class).to_string())
            .or_default();
        for trade in trades {
            if instrument.member_on(trade.signal_session) {
                entry.push(trade.clone());
                pooled_count += 1;
            }
        }
    }

    let scoreboard = select::judge(&pooled, inputs.live, session);

    let mut cards = Vec::new();
    let mut candidates = Vec::new();
    let mut suppressed = Vec::new();
    for instrument in tradable {
        let signals = instrument_signals(
            instrument,
            &scoreboard,
            inputs.account_value,
            inputs.risk_pct,
            inputs.fx_to_account,
            inputs.held,
        );
        cards.extend(signals.cards);
        candidates.extend(signals.candidates);
        suppressed.extend(
            signals
                .suppressed
                .into_iter()
                .map(|(setup, side, _)| (instrument.symbol.clone(), setup, side)),
        );
    }

    cards.sort_by(|left, right| right.score.total_cmp(&left.score));
    let mut seen = HashSet::new();
    cards.retain(|card| seen.insert(card.key.clone()));
    cards.truncate(MAX_SIGNALS_PER_DAY);

    let verified: HashSet<String> = cards.iter().map(|card| card.key.clone()).collect();
    candidates.sort_by(|left, right| right.score.total_cmp(&left.score));
    let mut seen = HashSet::new();
    candidates.retain(|card| !verified.contains(&card.key) && seen.insert(card.key.clone()));
    candidates.truncate(MAX_CANDIDATES_PER_DAY);

    let evidence_only = universe.iter().filter(|i| i.evidence_only).count() as u32;
    SignalDay {
        session,
        cards,
        candidates,
        scoreboard,
        suppressed,
        notes,
        run_stats: Some(RunStats {
            instruments: universe.len() as u32 - evidence_only,
            evidence_only,
            trades: pooled_count,
            ..RunStats::default()
        }),
    }
}

/// One instrument's cards and non-publishing firings against an existing scoreboard.
struct InstrumentSignals {
    session: NaiveDate,
    cards: Vec<SignalCard>,
    candidates: Vec<SignalCard>,
    suppressed: Vec<(SetupId, Side, String)>,
}

/// Builds the cards for one instrument. `session` is taken from its latest bar, which lets
/// a one-off check use fresh data even when the daily run is older.
fn instrument_signals(
    instrument: &Instrument,
    scoreboard: &[Verdict],
    account_value: Option<f64>,
    risk_pct: f64,
    fx_to_account: &dyn Fn(&Instrument) -> Option<f64>,
    held: &dyn Fn(&str) -> Option<String>,
) -> InstrumentSignals {
    let Some(index) = instrument.bars.len().checked_sub(1) else {
        return InstrumentSignals {
            session: NaiveDate::MIN,
            cards: Vec::new(),
            candidates: Vec::new(),
            suppressed: Vec::new(),
        };
    };
    let session_date = instrument.bars[index].session;
    let mut out = InstrumentSignals {
        session: session_date,
        cards: Vec::new(),
        candidates: Vec::new(),
        suppressed: Vec::new(),
    };
    let atr = indicators::atr(&instrument.bars, 14);
    let Some(atr) = atr[index] else {
        return out;
    };
    if !(atr.is_finite() && atr > 0.0) {
        return out;
    }
    let close = instrument.bars[index].close;
    let group = group_of(instrument.class);
    for setup in SetupId::ALL {
        for (fired, side) in fire(setup, &instrument.bars) {
            if fired != index {
                continue;
            }
            let Some(verdict) = verdict_for(scoreboard, group, setup, side) else {
                continue;
            };
            let tier = if verdict.publishes {
                Tier::Proven
            } else if verdict.candidate {
                Tier::Candidate
            } else {
                out.suppressed.push((setup, side, verdict.reason.clone()));
                continue;
            };
            if let Some(date) = earnings_in_window(instrument, session_date, setup.plan().1) {
                out.suppressed.push((
                    setup,
                    side,
                    format!("earnings on {date} falls inside the trade window"),
                ));
                continue;
            }
            let (stop_atr, max_sessions) = setup.plan();
            let zone = (close - 0.25 * atr, close + 0.25 * atr);
            let risk = stop_atr * atr;
            if !(risk.is_finite() && risk > 0.0) {
                continue;
            }
            let mid = close;
            let (stop, targets) = match side {
                Side::Long => (mid - risk, [mid + risk, mid + 2.0 * risk, mid + 3.0 * risk]),
                Side::Short => (mid + risk, [mid - risk, mid - 2.0 * risk, mid - 3.0 * risk]),
            };
            let card_risk_pct = match tier {
                Tier::Proven => risk_pct,
                Tier::Candidate => risk_pct * CANDIDATE_RISK_SHARE,
            };
            let size = account_value.and_then(|value| {
                fx_to_account(instrument).and_then(|fx| {
                    if fx > 0.0 && value > 0.0 && risk > 0.0 {
                        let units = card_risk_pct / 100.0 * value / (risk * fx);
                        Some((units, units * mid * fx))
                    } else {
                        None
                    }
                })
            });
            let relation = held(&instrument.key).map(|description| match side {
                Side::Long if description.trim().is_empty() => {
                    "you hold this; a long adds to it".to_string()
                }
                Side::Long => {
                    format!("you hold this ({}); a long adds to it", description.trim())
                }
                Side::Short if description.trim().is_empty() => {
                    "you hold this; a short works as a hedge on a CFD".to_string()
                }
                Side::Short => format!(
                    "you hold this ({}); a short works as a hedge on a CFD",
                    description.trim()
                ),
            });
            let card = SignalCard {
                id: card_id(&instrument.key, setup, side, session_date),
                key: instrument.key.clone(),
                symbol: instrument.symbol.clone(),
                class: instrument.class,
                side,
                setup,
                signal_session: session_date,
                entry_zone: zone,
                stop,
                targets,
                max_sessions,
                size,
                risk_pct: card_risk_pct,
                needs_cfd: instrument.needs_cfd(side),
                score: verdict.selection.expectancy_ci.0,
                verdict,
                relation,
                warnings: Vec::new(),
                tier,
            };
            match tier {
                Tier::Proven => out.cards.push(card),
                Tier::Candidate => out.candidates.push(card),
            }
        }
    }
    out
}

fn verdict_for(scoreboard: &[Verdict], group: &str, setup: SetupId, side: Side) -> Option<Verdict> {
    scoreboard
        .iter()
        .find(|verdict| {
            verdict.group == group
                && verdict.selection.setup == setup
                && verdict.selection.side == side
        })
        .cloned()
}

/// Checks one instrument against an existing scoreboard, without the daily cap and
/// without adding it to the universe. Used for searching an asset outside the watchlist.
pub fn check_instrument(
    instrument: &Instrument,
    scoreboard: &[Verdict],
    account_value: Option<f64>,
    risk_pct: f64,
    fx_to_account: &dyn Fn(&Instrument) -> Option<f64>,
    held: &dyn Fn(&str) -> Option<String>,
) -> InstrumentCheck {
    let signals = instrument_signals(
        instrument,
        scoreboard,
        account_value,
        risk_pct,
        fx_to_account,
        held,
    );
    let mut cards = signals.cards;
    cards.sort_by(|left, right| right.score.total_cmp(&left.score));
    let mut candidates = signals.candidates;
    candidates.sort_by(|left, right| right.score.total_cmp(&left.score));
    InstrumentCheck {
        session: signals.session,
        cards,
        candidates,
        suppressed: signals
            .suppressed
            .into_iter()
            .map(|(setup, side, reason)| SuppressedSignal {
                setup,
                side,
                reason,
            })
            .collect(),
    }
}

/// A protective trailing stop for a held instrument.
pub fn guard(instrument: &Instrument) -> Option<HoldingGuard> {
    sim::guard(instrument)
}

/// Grades a published card against the bars after it: the same rules as `replay`.
/// `None` while the trade is still open or never filled.
pub fn grade(card: &SignalCard, bars_after: &[Bar], costs: Costs) -> Option<Trade> {
    sim::grade(card, bars_after, costs)
}

fn card_id(key: &str, setup: SetupId, side: Side, session: NaiveDate) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hasher.update(b"|");
    hasher.update(format!("{setup:?}").as_bytes());
    hasher.update(b"|");
    hasher.update(format!("{side:?}").as_bytes());
    hasher.update(b"|");
    hasher.update(session.to_string().as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn earnings_in_window(
    instrument: &Instrument,
    session: NaiveDate,
    max_sessions: u32,
) -> Option<NaiveDate> {
    if !matches!(instrument.class, AssetClass::Stock | AssetClass::Etf) {
        return None;
    }
    let end = add_weekdays(session, max_sessions as i64);
    instrument
        .earnings
        .iter()
        .copied()
        .filter(|date| *date > session && *date <= end)
        .min()
}

fn add_weekdays(mut date: NaiveDate, mut count: i64) -> NaiveDate {
    while count > 0 {
        date = date + Days::new(1);
        if !matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
            count -= 1;
        }
    }
    date
}
