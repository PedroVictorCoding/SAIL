//! Expected dividends with reinvestment (round 14, Kilo's slice).
//!
//! The owner wants to see how dividend income could grow when dividends are reinvested.
//! This module projects it month by month over 1–30 years, from:
//! - what each holding pays: its per-share history from Yahoo, plus what the ledger
//!   actually received, which gives the real withholding rate;
//! - how fast each holding's payout has grown;
//! - a reinvestment rule chosen by the owner.
//!
//! **Reinvestment.** A share `reinvest_pct` of each net payment buys more units the same
//! month. Fractional units are allowed (XTB sells fractional shares). Two allocations:
//! - `Proportional` (the default): spread across all paying holdings in proportion to
//!   their current share of the projected portfolio value, so the mix stays stable;
//! - `SameHolding`: each holding's dividends buy more of that holding (a DRIP).
//!
//! The rest of each payment is kept as cash and does not earn anything.
//!
//! **Growth.** For each holding, the annual dividend per share (calendar years, complete
//! years only) gives:
//! - `g`: the compound annual growth over up to the last 5 complete years;
//! - `σ`: the standard deviation of year-on-year log changes.
//!
//! `g` is shrunk toward 0 by `n/(n+3)`, where `n` is the number of yearly changes, and
//! capped at ±15%. With fewer than 2 complete years, `g = 0` and `σ = 0.10`. Three
//! scenarios use `g − σ`, `g` and `g + σ` (per holding, with the same caps). They are
//! scenarios, not a probability band, and the page says so.
//!
//! **Prices.** When the service measured a price trend (`Holding::price_growth`), the
//! price follows it; otherwise it grows at the holding's dividend growth, so its yield
//! stays where it is now. A holding yielding more than [`HIGH_YIELD`] pays a constant
//! share of its *current* price, so its payout falls when its price falls. The reinvestment price for month `m` is
//! `price₀ · (1 + g)^(m/12)`. Prices are the instrument's trading currency, converted
//! with today's FX rate, held constant.
//!
//! **Tax.** Each payment loses its withholding rate: the one observed in the ledger
//! (`withheld / gross` over the last 24 months), else a default by country. Portuguese tax
//! is set aside only if `reserve_pt_tax` is on: 28% of the gross minus what was withheld,
//! floored at 0 (the foreign tax credit, simplified).
//!
//! **Purity.** No I/O and no clock. Money here is `f64` in the account currency, because
//! it is a projection and not ledger money.
mod growth;
mod project;

pub use self::growth::growth;
pub use self::project::project;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Most years projected.
pub const MAX_YEARS: u32 = 30;

/// Cap on |annual dividend growth| used in the projection.
pub const MAX_GROWTH: f64 = 0.15;

/// Portuguese flat rate on dividends (taxa liberatória / autónoma).
pub const PT_TAX: f64 = 0.28;

/// Most holdings a top-value or top-yield strategy may select.
pub const MAX_TOP_N: u32 = 10;

/// Default count for the top-value and top-yield strategies.
pub const DEFAULT_TOP_N: u32 = 5;

/// Where reinvested dividends go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Allocation {
    /// Across paying holdings, in proportion to their current share of value.
    Proportional,
    /// Back into the holding that paid it.
    SameHolding,
    /// Across the largest paying holdings by current value, in proportion to value.
    TopValue,
    /// Equally across every paying holding, regardless of current weight.
    EqualWeight,
    /// Across the highest-yielding paying holdings, in proportion to value.
    TopYield,
    /// Toward the paying holdings furthest below their equal-weight target.
    Underweight,
}

fn default_top_n() -> u32 {
    DEFAULT_TOP_N
}

/// The owner's choices.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Years to project, 1..=[`MAX_YEARS`].
    pub years: u32,
    /// Share of each net payment reinvested, 0..=100.
    pub reinvest_pct: f64,
    /// Where it goes.
    pub allocation: Allocation,
    /// How many holdings top-value or top-yield may select, 1..=[`MAX_TOP_N`].
    #[serde(default = "default_top_n")]
    pub top_n: u32,
    /// Set aside Portuguese tax before reinvesting.
    pub reserve_pt_tax: bool,
    /// First month of the projection (the month after the latest data).
    pub start: NaiveDate,
}

/// One past payment per share, in the trading currency, by ex-date.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Payment {
    /// Ex-dividend date.
    pub ex_date: NaiveDate,
    /// Amount per share.
    pub per_share: f64,
}

/// One holding's inputs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Holding {
    /// Ledger instrument key.
    pub key: String,
    /// Broker symbol.
    pub symbol: String,
    /// Units held now.
    pub units: f64,
    /// Latest price, in the trading currency.
    pub price: f64,
    /// Account currency per unit of the trading currency (today's rate).
    pub fx: f64,
    /// Per-share payment history, oldest first (may be empty: accumulating funds).
    pub history: Vec<Payment>,
    /// Withholding rate applied to gross payments, 0..1.
    pub withholding: f64,
    /// Where `withholding` came from: "observed" or "default".
    pub withholding_source: String,
    /// Annual price trend measured from stored prices (shrunk and capped by the
    /// service), or `None` to keep the yield constant (the price grows with dividends).
    #[serde(default)]
    pub price_growth: Option<f64>,
}

/// Trailing yield above which a holding is treated as paying out of its price (option-
/// income and similar funds): its payout per share follows its price instead of growing
/// on its own, so reinvesting it can't compound at its headline yield.
pub const HIGH_YIELD: f64 = 0.12;

/// The most total return (price trend plus yield) a high-yield holding is assumed to
/// earn a year. Its price is assumed to fall at least by `yield − this` a year, so a
/// fund paying out of its capital can't compound at its headline yield.
pub const HIGH_YIELD_TOTAL_RETURN: f64 = 0.08;

/// Growth estimate for one holding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Growth {
    /// Annual dividend per share for each complete calendar year used, oldest first.
    pub annual: Vec<(i32, f64)>,
    /// Raw compound growth (before shrinkage), if computable.
    pub raw: Option<f64>,
    /// Growth used in the base scenario (shrunk, capped).
    pub g: f64,
    /// Volatility of yearly changes.
    pub sigma: f64,
    /// Payment months (1..=12) seen in the last complete year, the schedule projected.
    pub months: Vec<u32>,
    /// Trailing-12-month dividend per share, the starting level.
    pub ttm_per_share: f64,
}

/// Which scenario.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    /// `g − σ`.
    Low,
    /// `g`.
    Base,
    /// `g + σ`.
    High,
}

/// One projected year (12 months from the start, not calendar years).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct YearRow {
    /// 1, 2, …
    pub year: u32,
    /// Gross dividends received, in the account currency.
    pub gross: f64,
    /// Withheld at source.
    pub withheld: f64,
    /// Portuguese tax set aside (0 unless `reserve_pt_tax`).
    pub pt_tax: f64,
    /// Net after withholding and reserve.
    pub net: f64,
    /// Reinvested.
    pub reinvested: f64,
    /// Kept as cash.
    pub cash: f64,
    /// Value of the dividend-paying holdings at year end.
    pub value: f64,
    /// Net income of the year divided by the original cost of the starting holdings
    /// (yield on the money the owner put in), when the cost is known.
    pub yield_on_cost: Option<f64>,
}

/// One holding's share of the projection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HoldingRow {
    /// Symbol.
    pub symbol: String,
    /// Net income in year 1.
    pub first_year_net: f64,
    /// Net income in the last year.
    pub last_year_net: f64,
    /// Units at the end.
    pub end_units: f64,
    /// The growth used.
    pub growth: Growth,
    /// Withholding used, and its source.
    pub withholding: (f64, String),
}

/// One scenario's projection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Projection {
    /// Which scenario.
    pub scenario: Scenario,
    /// One row per year.
    pub years: Vec<YearRow>,
    /// Net income per month, `years × 12` values, for the chart.
    pub monthly_net: Vec<f64>,
    /// The year in which net income first reaches twice year 1's, if it does.
    pub doubling_year: Option<u32>,
}

/// The full result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DividendOutlook {
    /// Settings used.
    pub settings: Settings,
    /// Low, base and high.
    pub scenarios: Vec<Projection>,
    /// Per holding, base scenario.
    pub holdings: Vec<HoldingRow>,
    /// Holdings that pay nothing (accumulating funds) or have no history, by symbol.
    pub non_paying: Vec<String>,
    /// The same base projection with 0% reinvested, for comparison.
    pub without_reinvestment: Projection,
    /// Plain notes on the assumptions and limits.
    pub notes: Vec<String>,
}

/// Withholding by the listing's country when nothing is observed, from the XTB suffix:
/// - US 15% (W-8BEN treaty rate);
/// - UK 0%;
/// - IE 0% (Irish funds);
/// - DE 26.375%;
/// - FR 25%;
/// - NL 15%;
/// - PT 25%;
/// - ES 19%;
/// - IT 26%;
/// - anything else 15%.
///
/// Note that UK-listed (`.UK`) Irish UCITS ETFs pay 0%.
pub fn default_withholding(symbol: &str) -> f64 {
    match symbol
        .rsplit_once('.')
        .map(|(_, s)| s.to_ascii_uppercase())
        .as_deref()
    {
        Some("US") => 0.15,
        Some("UK") | Some("IE") => 0.0,
        Some("DE") => 0.26375,
        Some("FR") => 0.25,
        Some("NL") => 0.15,
        Some("PT") => 0.25,
        Some("ES") => 0.19,
        Some("IT") => 0.26,
        _ => 0.15,
    }
}
