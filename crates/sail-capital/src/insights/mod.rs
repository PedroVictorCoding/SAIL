//! Portfolio insights (round 9, Antigravity's slice): what the owner holds, how
//! concentrated it is, what it earns and how fresh the data is, computed from a ledger
//! [`Projection`]. Pure and deterministic: no I/O, no clock (the caller passes `now`), no
//! prices beyond the broker's own figures, and no model.
//!
//! Every amount stays in its currency. A figure that would need two currencies combined is
//! `None` with a warning, never converted, because there is no FX feed.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

use crate::contracts::{Currency, EntryKind, InstrumentKind, LedgerRecord, Money, TaxKind};
use crate::ledger::Projection;

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

/// The account in the broker's own terms, so the figures can be checked against XTB.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountView {
    /// Free funds plus owned positions' broker value (XTB's account value, without CFDs).
    pub account_value: Money,
    /// Ledger cash (XTB's free funds, before any margin in use).
    pub free_funds: Money,
    /// What the open owned positions cost: the sum of their cost bases. Units received
    /// free count at their stated cost (zero by default).
    pub invested: Option<Money>,
    /// `owned value - invested`: profit or loss on the open positions.
    pub open_result: Option<Money>,
}

/// Money in, money out and what it is worth now, in one currency.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TotalReturn {
    /// Every deposit, added up.
    pub deposits: Money,
    /// Every withdrawal, added up (negative or zero).
    pub withdrawals: Money,
    /// Deposits plus withdrawals. Transfers between the owner's own sub-accounts are
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
    /// The account in the broker's terms, when cash and values share one currency.
    pub account: Option<AccountView>,
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

/// Parses a `"YYYY-MM"` string to the first day of that month.
fn parse_month(s: &str) -> Option<NaiveDate> {
    let (y_str, m_str) = s.split_once('-')?;
    let y = y_str.parse::<i32>().ok()?;
    let m = m_str.parse::<u32>().ok()?;
    NaiveDate::from_ymd_opt(y, m, 1)
}

/// Helper to accumulate monthly income parts per (month, currency).
#[derive(Default)]
struct MonthAccumulator {
    dividends_gross: Decimal,
    withheld: Decimal,
    interest_net: Decimal,
}

/// Computes the insights for `projection` as of `now`. `records` are the ledger rows the
/// projection was built from (already filtered to its `AsOf`); they are read only for
/// interest and its tax, by month. Dividends come from `projection.dividends`, and
/// deposits and withdrawals from `projection.flows` (labels `"deposit"`, `"withdrawal"`).
pub fn insights(projection: &Projection, records: &[LedgerRecord], now: DateTime<Utc>) -> Insights {
    let mut warnings = Vec::new();

    // 1. Freshness
    let (snapshot_age_hours, stale) = match projection.snapshot_as_of {
        Some(snap) => {
            let hours = (now - snap).num_hours().max(0);
            (Some(hours), hours > STALE_AFTER_HOURS)
        }
        None => (None, true),
    };
    let freshness = Freshness {
        snapshot_as_of: projection.snapshot_as_of,
        snapshot_age_hours,
        stale,
    };

    // 2. Owned Value & Allocation
    // Exclude CFDs (kept apart in cfd_positions). Use only positions in projection.positions.
    let mut unvalued = Vec::new();
    let mut valued_positions = Vec::new();

    for pos in &projection.positions {
        match &pos.broker_market_value {
            Some(mv) => valued_positions.push((pos, mv)),
            None => unvalued.push(pos.instrument.symbol.clone()),
        }
    }
    unvalued.sort();

    let mut val_currencies = BTreeSet::new();
    for (_, mv) in &valued_positions {
        val_currencies.insert(&mv.currency);
    }

    let (currency, owned_value, allocation, by_kind, concentration) = if val_currencies.len() > 1 {
        warnings.push(
            "Owned positions use multiple currencies; allocation and concentration cannot be computed"
                .to_string(),
        );
        (None, None, Vec::new(), Vec::new(), None)
    } else if let Some(common_curr) = val_currencies.into_iter().next() {
        let total_amount: Decimal = valued_positions.iter().map(|(_, mv)| mv.amount).sum();
        let common_curr_clone = common_curr.clone();
        let total_money = Money::new(total_amount, common_curr_clone.clone());

        if total_amount > Decimal::ZERO {
            // Allocation slices
            let mut alloc_slices = Vec::new();
            for (pos, mv) in &valued_positions {
                let unrounded = mv.amount / total_amount;
                let share =
                    unrounded.round_dp_with_strategy(4, RoundingStrategy::MidpointNearestEven);
                alloc_slices.push(AllocationSlice {
                    instrument_key: pos.instrument.key(),
                    symbol: pos.instrument.symbol.clone(),
                    name: pos.instrument.name.clone(),
                    kind: pos.instrument.kind,
                    value: (*mv).clone(),
                    share,
                });
            }

            // Sort allocation: value descending, then symbol ascending
            alloc_slices.sort_by(|a, b| {
                b.value
                    .amount
                    .cmp(&a.value.amount)
                    .then_with(|| a.symbol.cmp(&b.symbol))
            });

            // Group by kind
            let mut kind_groups: HashMap<InstrumentKind, (Decimal, u32)> = HashMap::new();
            for (pos, mv) in &valued_positions {
                let entry = kind_groups
                    .entry(pos.instrument.kind)
                    .or_insert((Decimal::ZERO, 0));
                entry.0 += mv.amount;
                entry.1 += 1;
            }

            let mut by_kind_slices = Vec::new();
            for (kind, (kind_val, count)) in kind_groups {
                let share = (kind_val / total_amount)
                    .round_dp_with_strategy(4, RoundingStrategy::MidpointNearestEven);
                by_kind_slices.push(KindSlice {
                    kind,
                    value: Money::new(kind_val, common_curr_clone.clone()),
                    share,
                    positions: count,
                });
            }
            by_kind_slices.sort_by(|a, b| {
                b.value
                    .amount
                    .cmp(&a.value.amount)
                    .then_with(|| format!("{:?}", a.kind).cmp(&format!("{:?}", b.kind)))
            });

            // Concentration
            let largest_share = alloc_slices[0].share;
            let largest_symbol = alloc_slices[0].symbol.clone();

            let top_n_unrounded: Decimal = alloc_slices
                .iter()
                .take(TOP_N)
                .map(|s| s.value.amount / total_amount)
                .sum();
            let top_n_share =
                top_n_unrounded.round_dp_with_strategy(4, RoundingStrategy::MidpointNearestEven);

            let hhi_unrounded: Decimal = valued_positions
                .iter()
                .map(|(_, mv)| {
                    let s = mv.amount / total_amount;
                    s * s
                })
                .sum();
            let hhi =
                hhi_unrounded.round_dp_with_strategy(4, RoundingStrategy::MidpointNearestEven);

            let effective_positions = if hhi > Decimal::ZERO {
                (Decimal::ONE / hhi)
                    .round_dp_with_strategy(1, RoundingStrategy::MidpointNearestEven)
            } else {
                Decimal::ZERO
            };

            let concentration_opt = Some(Concentration {
                largest_share,
                largest_symbol,
                top_n_share,
                hhi,
                effective_positions,
                positions: valued_positions.len() as u32,
            });

            (
                Some(common_curr_clone),
                Some(total_money),
                alloc_slices,
                by_kind_slices,
                concentration_opt,
            )
        } else {
            (
                Some(common_curr_clone),
                Some(total_money),
                Vec::new(),
                Vec::new(),
                None,
            )
        }
    } else {
        (None, None, Vec::new(), Vec::new(), None)
    };

    // 3. Income
    let mut months_map: BTreeMap<(String, Currency), MonthAccumulator> = BTreeMap::new();

    // 3a. Dividends from projection.dividends, excluding CFDs
    for div in &projection.dividends {
        if div.instrument.as_ref().map(|i| i.kind) == Some(InstrumentKind::Cfd) {
            continue;
        }
        let paid = &div.paid;
        let month = format!("{:04}-{:02}", paid.at.year(), paid.at.month());
        let acc = months_map
            .entry((month.clone(), div.gross.currency.clone()))
            .or_default();
        acc.dividends_gross += div.gross.amount;

        if let Some(w) = &div.withheld {
            if w.currency == div.gross.currency {
                acc.withheld -= w.amount.abs();
            } else {
                let w_acc = months_map.entry((month, w.currency.clone())).or_default();
                w_acc.withheld -= w.amount.abs();
            }
        }
    }

    // 3b. Interest and Tax on Interest from records
    for rec in records {
        match &rec.entry.kind {
            EntryKind::Interest => {
                if let Some(cash) = &rec.entry.cash_effect {
                    let month = format!(
                        "{:04}-{:02}",
                        rec.entry.occurred.at.year(),
                        rec.entry.occurred.at.month()
                    );
                    let acc = months_map
                        .entry((month, cash.currency.clone()))
                        .or_default();
                    acc.interest_net += cash.amount.abs();
                }
            }
            EntryKind::Tax {
                kind: TaxKind::OnInterest,
                ..
            } => {
                if let Some(cash) = &rec.entry.cash_effect {
                    let month = format!(
                        "{:04}-{:02}",
                        rec.entry.occurred.at.year(),
                        rec.entry.occurred.at.month()
                    );
                    let acc = months_map
                        .entry((month, cash.currency.clone()))
                        .or_default();
                    acc.interest_net -= cash.amount.abs();
                }
            }
            _ => {}
        }
    }

    // Build by_month
    let mut by_month: Vec<MonthIncome> = Vec::new();
    for ((month, curr), acc) in months_map {
        let net = acc.dividends_gross + acc.withheld + acc.interest_net;
        by_month.push(MonthIncome {
            month,
            dividends_gross: Money::new(acc.dividends_gross, curr.clone()),
            withheld: Money::new(acc.withheld, curr.clone()),
            interest_net: Money::new(acc.interest_net, curr.clone()),
            net: Money::new(net, curr),
        });
    }

    // Sort by_month: oldest first, then currency ascending
    by_month.sort_by(|a, b| {
        a.month.cmp(&b.month).then_with(|| {
            a.dividends_gross
                .currency
                .as_str()
                .cmp(b.dividends_gross.currency.as_str())
        })
    });

    if by_month.len() > 120 {
        by_month = by_month[by_month.len() - 120..].to_vec();
    }

    // Trailing 12 months: sum net over months whose start is no more than 12 months before now
    let naive_now = now.date_naive();
    let cutoff = naive_now
        .checked_sub_months(chrono::Months::new(12))
        .unwrap_or(naive_now);

    let mut trailing_map: BTreeMap<Currency, Decimal> = BTreeMap::new();
    for row in &by_month {
        if parse_month(&row.month).is_some_and(|ms| ms >= cutoff && ms <= naive_now) {
            let curr = &row.net.currency;
            let sum = trailing_map.entry(curr.clone()).or_insert(Decimal::ZERO);
            *sum += row.net.amount;
        }
    }

    let mut trailing_12m_net: Vec<Money> = trailing_map
        .into_iter()
        .map(|(curr, amt)| Money::new(amt, curr))
        .collect();
    trailing_12m_net.sort_by(|a, b| a.currency.as_str().cmp(b.currency.as_str()));

    // Trailing yield: trailing net over owned value when both are in one currency
    let trailing_yield = if let Some(ov) = &owned_value {
        if trailing_12m_net.is_empty() {
            Some(Decimal::ZERO)
        } else if trailing_12m_net.len() == 1 && trailing_12m_net[0].currency == ov.currency {
            if ov.amount > Decimal::ZERO {
                Some(
                    (trailing_12m_net[0].amount / ov.amount)
                        .round_dp_with_strategy(4, RoundingStrategy::MidpointNearestEven),
                )
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    let income = Income {
        trailing_12m_net,
        by_month,
        trailing_yield,
    };

    // 4. Total Return
    let mut deposit_withdrawal_currencies = BTreeSet::new();
    let mut deposits_by_curr: BTreeMap<Currency, Decimal> = BTreeMap::new();

    for flow in &projection.flows {
        if flow.label == "deposit" || flow.label == "withdrawal" {
            deposit_withdrawal_currencies.insert(flow.total.currency.clone());
            let sum = deposits_by_curr
                .entry(flow.total.currency.clone())
                .or_insert(Decimal::ZERO);
            *sum += flow.total.amount;
        }
    }

    let mut non_zero_cash_currencies = BTreeSet::new();
    for c in &projection.cash {
        if c.amount != Decimal::ZERO {
            non_zero_cash_currencies.insert(c.currency.clone());
        }
    }

    let total_return = if deposit_withdrawal_currencies.len() > 1 {
        warnings.push(
            "Deposits and withdrawals use multiple currencies; total return cannot be computed"
                .to_string(),
        );
        None
    } else {
        let target_currency =
            if let Some(dep_curr) = deposit_withdrawal_currencies.into_iter().next() {
                Some(dep_curr)
            } else if let Some(c) = &currency {
                Some(c.clone())
            } else if non_zero_cash_currencies.len() == 1 {
                non_zero_cash_currencies.into_iter().next()
            } else {
                None
            };

        if let Some(target_curr) = target_currency {
            let has_other_cash = projection
                .cash
                .iter()
                .any(|c| c.currency != target_curr && c.amount != Decimal::ZERO);

            let (ov_amount, ov_ok) = if projection.positions.is_empty() {
                (Decimal::ZERO, true)
            } else if let Some(ov) = &owned_value {
                if ov.currency == target_curr {
                    (ov.amount, true)
                } else {
                    (Decimal::ZERO, false)
                }
            } else {
                (Decimal::ZERO, false)
            };

            if has_other_cash {
                warnings.push(
                    "Cash balance uses multiple currencies; total return cannot be computed"
                        .to_string(),
                );
                None
            } else if !ov_ok {
                warnings.push(
                    "Portfolio value and deposits use different currencies; total return cannot be computed"
                        .to_string(),
                );
                None
            } else {
                let net_dep_amount = deposits_by_curr
                    .get(&target_curr)
                    .copied()
                    .unwrap_or(Decimal::ZERO);
                let cash_amount = projection
                    .cash
                    .iter()
                    .find(|c| c.currency == target_curr)
                    .map(|c| c.amount)
                    .unwrap_or(Decimal::ZERO);
                if !unvalued.is_empty() {
                    warnings.push(format!(
                        "Current value leaves out {} position(s) with no broker value: {}",
                        unvalued.len(),
                        unvalued.join(", ")
                    ));
                }
                let curr_val_amount = cash_amount + ov_amount;
                let gain_amount = curr_val_amount - net_dep_amount;

                let gain_ratio = if net_dep_amount > Decimal::ZERO {
                    Some(
                        (gain_amount / net_dep_amount)
                            .round_dp_with_strategy(4, RoundingStrategy::MidpointNearestEven),
                    )
                } else {
                    None
                };

                let flow_sum = |label: &str| {
                    projection
                        .flows
                        .iter()
                        .filter(|f| f.label == label && f.total.currency == target_curr)
                        .map(|f| f.total.amount)
                        .sum::<Decimal>()
                };
                Some(TotalReturn {
                    deposits: Money::new(flow_sum("deposit"), target_curr.clone()),
                    withdrawals: Money::new(flow_sum("withdrawal"), target_curr.clone()),
                    net_deposits: Money::new(net_dep_amount, target_curr.clone()),
                    current_value: Money::new(curr_val_amount, target_curr.clone()),
                    gain: Money::new(gain_amount, target_curr),
                    gain_ratio,
                })
            }
        } else {
            None
        }
    };

    let account = account_view(projection, owned_value.as_ref(), &unvalued, &mut warnings);

    Insights {
        account,
        currency,
        owned_value,
        allocation,
        by_kind,
        concentration,
        income,
        total_return,
        freshness,
        unvalued,
        warnings,
    }
}

/// The account in XTB's terms: account value = free funds + owned value; invested = the
/// open positions' cost; open result = owned value - invested. `None` unless the cash is in
/// one currency and the owned value, if any, is in the same one.
fn account_view(
    projection: &Projection,
    owned_value: Option<&Money>,
    unvalued: &[String],
    warnings: &mut Vec<String>,
) -> Option<AccountView> {
    let cash: Vec<&Money> = projection.cash.iter().collect();
    let free_funds = match cash.as_slice() {
        [only] => (*only).clone(),
        [] => Money::new(Decimal::ZERO, owned_value?.currency.clone()),
        _ => {
            warnings.push(
                "Cash is held in more than one currency; the account view isn't shown".to_string(),
            );
            return None;
        }
    };
    let currency = free_funds.currency.clone();
    let owned = match owned_value {
        Some(v) if v.currency == currency => v.amount,
        Some(_) => {
            warnings.push(
                "Cash and positions use different currencies; the account view isn't shown"
                    .to_string(),
            );
            return None;
        }
        None if projection.positions.is_empty() => Decimal::ZERO,
        None => return None,
    };
    let mut invested = Some(Decimal::ZERO);
    for position in &projection.positions {
        if unvalued.contains(&position.instrument.symbol) {
            continue;
        }
        invested = match (invested, &position.cost_basis) {
            (Some(sum), Some(cost)) if cost.currency == currency => Some(sum + cost.amount),
            _ => None,
        };
    }
    if invested.is_none() {
        warnings.push(
            "Some open positions have no cost basis in the account currency; invested isn't shown"
                .to_string(),
        );
    }
    Some(AccountView {
        account_value: Money::new(free_funds.amount + owned, currency.clone()),
        free_funds,
        invested: invested.map(|i| Money::new(i, currency.clone())),
        open_result: invested.map(|i| Money::new(owned - i, currency.clone())),
    })
}
