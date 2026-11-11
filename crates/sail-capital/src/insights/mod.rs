//! Portfolio insights (round 9, Antigravity's slice): what the owner holds, how
//! concentrated it is, what it earns and how fresh the data is, computed from a ledger
//! [`Projection`]. Pure and deterministic: no I/O, no clock (the caller passes `now`), no
//! prices beyond the broker's own figures, and no model.
//!
//! Every amount stays in its currency. A figure that would need two currencies combined is
//! `None` with a warning, never converted, because there is no FX feed.
use crate::contracts::{Currency, InstrumentKind, LedgerRecord, Money};
use crate::ledger::Projection;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Hours after which the broker snapshot is called stale.
pub const STALE_AFTER_HOURS: i64 = 72;

/// How many positions the "top N" concentration figure covers.
pub const TOP_N: usize = 5;

/// One owned position's slice of the portfolio, by the broker's value at its snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocationSlice {
    /// Ledger instrument key.
    pub instrument_key: String,
    /// Broker symbol.
    pub symbol: String,
    /// Security name, if known.
    pub name: Option<String>,
    /// Stock, ETF or unknown (CFDs are never included).
    pub kind: InstrumentKind,
    /// Broker market value.
    pub value: Money,
    /// Share of all owned value, 0..=1, rounded to 4 places.
    #[serde(with = "rust_decimal::serde::str")]
    pub share: Decimal,
}

/// Owned value grouped by instrument kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindSlice {
    /// The kind.
    pub kind: InstrumentKind,
    /// Total broker value.
    pub value: Money,
    /// Share of all owned value, 0..=1, rounded to 4 places.
    #[serde(with = "rust_decimal::serde::str")]
    pub share: Decimal,
    /// Number of positions.
    pub positions: u32,
}

/// How concentrated the owned positions are.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Concentration {
    /// Largest position's share, 0..=1.
    #[serde(with = "rust_decimal::serde::str")]
    pub largest_share: Decimal,
    /// Symbol of the largest position.
    pub largest_symbol: String,
    /// Combined share of the [`TOP_N`] largest, 0..=1.
    #[serde(with = "rust_decimal::serde::str")]
    pub top_n_share: Decimal,
    /// Herfindahl–Hirschman index: the sum of squared shares, 0..=1. 1 is a single
    /// position; 1/n is n equal positions.
    #[serde(with = "rust_decimal::serde::str")]
    pub hhi: Decimal,
    /// `1 / hhi`, rounded to 1 place: how many equal positions would be this concentrated.
    #[serde(with = "rust_decimal::serde::str")]
    pub effective_positions: Decimal,
    /// Number of owned positions with a value.
    pub positions: u32,
}

/// Income in one calendar month (UTC), in one currency.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonthIncome {
    /// `"YYYY-MM"`.
    pub month: String,
    /// Owned-share dividends before withholding.
    pub dividends_gross: Money,
    /// Withholding matched to those dividends (negative or zero).
    pub withheld: Money,
    /// Interest on free cash, after its tax.
    pub interest_net: Money,
    /// `dividends_gross + withheld + interest_net`.
    pub net: Money,
}

/// Income from owned holdings and cash. CFD dividend equivalents are excluded: they are
/// payments on a contract, not income from owning shares.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Income {
    /// Net income in the 12 months before `now`, per currency.
    pub trailing_12m_net: Vec<Money>,
    /// Every month with income, oldest first, at most 120.
    pub by_month: Vec<MonthIncome>,
    /// Trailing net income over current owned value, when both are in one currency.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub trailing_yield: Option<Decimal>,
}

/// Money in, money out and what it is worth now, in one currency.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TotalReturn {
    /// Deposits minus withdrawals. Transfers between the owner's own sub-accounts are
    /// excluded.
    pub net_deposits: Money,
    /// Ledger cash plus owned positions' broker value.
    pub current_value: Money,
    /// `current_value - net_deposits`.
    pub gain: Money,
    /// `gain / net_deposits`, when net deposits are positive.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub gain_ratio: Option<Decimal>,
}

/// How current the data is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Freshness {
    /// The broker snapshot's own time.
    pub snapshot_as_of: Option<DateTime<Utc>>,
    /// Whole hours from the snapshot to `now`.
    pub snapshot_age_hours: Option<i64>,
    /// True when there is no snapshot or it is older than [`STALE_AFTER_HOURS`].
    pub stale: bool,
}

/// Everything the Insights section shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Insights {
    /// The currency of the value figures, when every owned position's value is in one.
    pub currency: Option<Currency>,
    /// Total owned value.
    pub owned_value: Option<Money>,
    /// Owned positions by value, largest first.
    pub allocation: Vec<AllocationSlice>,
    /// Owned value by kind, largest first.
    pub by_kind: Vec<KindSlice>,
    /// Concentration, when there is at least one valued position.
    pub concentration: Option<Concentration>,
    /// Income.
    pub income: Income,
    /// Return on money put in, when cash, values and flows share one currency.
    pub total_return: Option<TotalReturn>,
    /// Data freshness.
    pub freshness: Freshness,
    /// Owned positions left out of the value figures (no broker value), by symbol.
    pub unvalued: Vec<String>,
    /// Plain-language notes on anything left out or not computable.
    pub warnings: Vec<String>,
}

/// Computes the insights for `projection` as of `now`. `records` are the ledger rows the
/// projection was built from (already filtered to its `AsOf`); they are read only for
/// interest and its tax, by month. Dividends come from `projection.dividends`, and
/// deposits and withdrawals from `projection.flows` (labels `"deposit"`, `"withdrawal"`).
pub fn insights(projection: &Projection, records: &[LedgerRecord], now: DateTime<Utc>) -> Insights {
    // Antigravity implements this; the stub keeps the crate building.
    let _ = (records, now);
    Insights {
        currency: None,
        owned_value: None,
        allocation: Vec::new(),
        by_kind: Vec::new(),
        concentration: None,
        income: Income {
            trailing_12m_net: Vec::new(),
            by_month: Vec::new(),
            trailing_yield: None,
        },
        total_return: None,
        freshness: Freshness {
            snapshot_as_of: projection.snapshot_as_of,
            snapshot_age_hours: None,
            stale: true,
        },
        unvalued: Vec::new(),
        warnings: Vec::new(),
    }
}
