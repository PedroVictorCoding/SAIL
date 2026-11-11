//! Deterministic projections of the private ledger (Kilo's slice, round 7).
//!
//! [`project`] replays the [`LedgerRecord`]s an [`AsOf`] can see into positions, cash,
//! dividends, fees and realized results, then reconciles them against the latest
//! [`StoredSnapshot`] the same `AsOf` can see. It is a pure function: same input, same
//! output, no clock, no I/O, no model. The output types below are the M0 contract; the
//! HTTP facade serializes them as-is.
//!
//! Rules (see the handoff file for the full list):
//! - Money is `Decimal`. Amounts in different currencies are never added together; totals
//!   are per currency. There is no FX feed this round.
//! - CFDs (`InstrumentKind::Cfd`) never merge with owned positions.
//! - Lots are matched first-in, first-out, the method Portuguese capital-gains rules use
//!   for securities. A sell larger than the known lots is a variance, not a short.
//! - Anything that can't be computed from stated facts is `None` plus a warning, never a
//!   guess.
use crate::contracts::*;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod dividends;
mod fifo;
mod reconcile;

#[cfg(test)]
mod tests;

/// One open lot after FIFO matching.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lot {
    /// When the lot was bought.
    pub opened: EventTime,
    /// Units still open, positive.
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Price per unit paid, if the buy row stated it.
    pub unit_cost: Option<Money>,
    /// Ledger record id of the buy.
    pub record_id: String,
}

/// A position derived from the ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    /// The security or contract.
    pub instrument: Instrument,
    /// Units open, the sum of `lots`.
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Open lots, oldest first.
    pub lots: Vec<Lot>,
    /// Total cost of the open lots, when every lot has a unit cost in one currency.
    pub cost_basis: Option<Money>,
    /// The broker's latest market value for it from the visible snapshot, if any. Labelled
    /// with the snapshot time in `Projection::snapshot_as_of`; never refreshed silently.
    pub broker_market_value: Option<Money>,
}

/// One dividend event, with any withholding matched to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DividendView {
    /// The paying security, if known.
    pub instrument: Option<Instrument>,
    /// When it was credited.
    pub paid: EventTime,
    /// Gross amount credited (positive).
    pub gross: Money,
    /// Tax withheld that the ledger matched to it (positive), if any.
    pub withheld: Option<Money>,
    /// `gross - withheld` when both are in the same currency.
    pub net: Option<Money>,
    /// Ledger record ids that make it up.
    pub record_ids: Vec<String>,
}

/// One FIFO-matched sale.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Realized {
    /// The security.
    pub instrument: Instrument,
    /// When it was sold.
    pub sold: EventTime,
    /// Units sold.
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Proceeds before fees, if the sell row stated price or amount.
    pub proceeds: Option<Money>,
    /// FIFO cost of the units sold, if every matched lot had a cost in the same currency.
    pub cost: Option<Money>,
    /// `proceeds - cost` when both are known in the same currency. Gross of fees and tax.
    pub result: Option<Money>,
    /// Ledger record id of the sell.
    pub record_id: String,
}

/// Sum of one kind of flow in one currency.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowTotal {
    /// `"deposit"`, `"withdrawal"`, `"dividend"`, `"withholding"`, `"interest"`,
    /// `"fee:<kind>"`, `"tax:<kind>"`, `"derivative_result"`, `"unsupported"`, …
    pub label: String,
    /// The signed total of `cash_effect` for those rows.
    pub total: Money,
    /// How many rows.
    pub count: u32,
}

/// Whether the ledger agrees with the broker's own snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationStatus {
    /// Every compared figure matches exactly.
    Matched,
    /// At least one figure differs; see the variances.
    Variance,
    /// There is no snapshot to compare with, or nothing comparable in it.
    Unverified,
}

/// One figure where ledger and broker differ.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Variance {
    /// What was compared, e.g. `"quantity isin:US0378331005"` or `"cash EUR"`.
    pub subject: String,
    /// Ledger figure, as exact decimal text, or `None` if the ledger has nothing.
    pub ledger: Option<String>,
    /// Broker figure, as exact decimal text, or `None` if the broker lists nothing.
    pub broker: Option<String>,
    /// Plain-language explanation of likely causes (e.g. history exported from a later
    /// date than the first trade).
    pub note: String,
}

/// Reconciliation of a projection against the visible snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reconciliation {
    /// Overall result.
    pub status: ReconciliationStatus,
    /// Every difference found.
    pub variances: Vec<Variance>,
    /// How many figures were compared.
    pub compared: u32,
}

/// The ledger as seen at one [`AsOf`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projection {
    /// The account.
    pub account: AccountId,
    /// The bounds used.
    pub as_of: AsOf,
    /// Open owned positions (stocks, ETFs, unknown), by instrument key.
    pub positions: Vec<Position>,
    /// Open CFD positions, kept apart.
    pub cfd_positions: Vec<Position>,
    /// Cash per currency: the signed sum of every visible `cash_effect`.
    pub cash: Vec<Money>,
    /// Dividends, oldest first.
    pub dividends: Vec<DividendView>,
    /// FIFO-matched sales, oldest first.
    pub realized: Vec<Realized>,
    /// Totals per flow kind and currency.
    pub flows: Vec<FlowTotal>,
    /// The snapshot time used for reconciliation and market values, if any.
    pub snapshot_as_of: Option<DateTime<Utc>>,
    /// Comparison with the broker.
    pub reconciliation: Reconciliation,
    /// Things the owner should know (unmatched withholding, sells without lots, …).
    pub warnings: Vec<String>,
    /// How many records were visible.
    pub records_used: u32,
}

/// Replay `records` and reconcile against `snapshots`, both filtered by `as_of`.
///
/// Entries are ordered by event time (a date-only row before a timed row on the same
/// instant), then by natural key, so the result never depends on the input order. Every
/// total is per currency; nothing is converted. The projection reads no clock and keeps no
/// state, so the same records and bounds always give the same answer.
pub fn project(
    account: &AccountId,
    records: &[LedgerRecord],
    snapshots: &[StoredSnapshot],
    as_of: AsOf,
) -> Projection {
    let visible = visible_records(account, records, &as_of);
    let visible_snapshot = visible_snapshot(account, snapshots, &as_of);

    let mut warnings: Vec<String> = Vec::new();
    let mut internal_variances: Vec<Variance> = Vec::new();
    let (books, realized) = fifo::build_positions(&visible, &mut warnings, &mut internal_variances);

    let market_values = match visible_snapshot {
        Some(snapshot) => market_values_by_key(snapshot),
        None => BTreeMap::new(),
    };
    let mut positions: Vec<Position> = Vec::new();
    let mut cfd_positions: Vec<Position> = Vec::new();
    for book in books.into_values() {
        let Some(position) = book.into_position(&market_values) else {
            continue;
        };
        if position.instrument.kind == InstrumentKind::Cfd {
            cfd_positions.push(position);
        } else {
            positions.push(position);
        }
    }

    let dividends = dividends::build_dividends(&visible, &mut warnings);
    let cash = cash_totals(&visible);
    let flows = flow_totals(&visible);
    let (reconciliation, snapshot_as_of) = reconcile::reconcile(
        &positions,
        &cfd_positions,
        &cash,
        visible_snapshot,
        internal_variances,
    );

    Projection {
        account: account.clone(),
        as_of,
        positions,
        cfd_positions,
        cash,
        dividends,
        realized,
        flows,
        snapshot_as_of,
        reconciliation,
        warnings,
        records_used: u32::try_from(visible.len()).unwrap_or(u32::MAX),
    }
}

/// Records this account may see at `as_of`, in the canonical order. Both bounds apply: the
/// row must have happened by `occurred_by` and have been stored by `known_by`.
fn visible_records<'a>(
    account: &AccountId,
    records: &'a [LedgerRecord],
    as_of: &AsOf,
) -> Vec<&'a LedgerRecord> {
    let mut visible: Vec<&LedgerRecord> = records
        .iter()
        .filter(|record| &record.account == account)
        .filter(|record| record.entry.occurred.at <= as_of.occurred_by)
        .filter(|record| record.ingested_at <= as_of.known_by)
        .collect();
    visible.sort_by(|left, right| {
        left.entry
            .occurred
            .at
            .cmp(&right.entry.occurred.at)
            .then_with(|| {
                precision_rank(left.entry.occurred.precision)
                    .cmp(&precision_rank(right.entry.occurred.precision))
            })
            .then_with(|| left.entry.natural_key.cmp(&right.entry.natural_key))
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| left.batch_id.cmp(&right.batch_id))
    });
    visible
}

/// A date-only row comes before a timed row at the same instant: it says less, so it must
/// not be treated as if it had happened later in the day.
fn precision_rank(precision: TimePrecision) -> u8 {
    match precision {
        TimePrecision::DateOnly => 0,
        TimePrecision::Exact => 1,
    }
}

/// The latest snapshot this account knew by `known_by` that describes current holdings
/// (open positions or a balance), provided the snapshot's own `as_of` isn't after
/// `occurred_by`. A later import must not make an earlier answer look newer.
fn visible_snapshot<'a>(
    account: &AccountId,
    snapshots: &'a [StoredSnapshot],
    as_of: &AsOf,
) -> Option<&'a StoredSnapshot> {
    snapshots
        .iter()
        .filter(|snapshot| &snapshot.account == account)
        .filter(|snapshot| snapshot.ingested_at <= as_of.known_by)
        // XTB exports cash, closed and open positions as separate reports. A snapshot with
        // neither open positions nor a balance (e.g. a closed-positions export) says
        // nothing about current holdings, so it must not stand in for "holds nothing".
        .filter(|snapshot| {
            !snapshot.snapshot.open_positions.is_empty()
                || snapshot.snapshot.balance.is_some()
                || snapshot.snapshot.equity.is_some()
        })
        .filter(|snapshot| {
            snapshot
                .snapshot
                .as_of
                .is_none_or(|at| at <= as_of.occurred_by)
        })
        .max_by(|left, right| {
            left.ingested_at
                .cmp(&right.ingested_at)
                .then_with(|| left.batch_id.cmp(&right.batch_id))
        })
}

/// Sum the broker's market value per instrument key. A key is only labelled when every
/// position on it states a value in one currency; a partial sum would misstate the account.
fn market_values_by_key(snapshot: &StoredSnapshot) -> BTreeMap<String, Money> {
    let mut values: BTreeMap<String, Vec<Option<Money>>> = BTreeMap::new();
    for position in &snapshot.snapshot.open_positions {
        values
            .entry(position.instrument.key())
            .or_default()
            .push(position.market_value.clone());
    }
    values
        .into_iter()
        .filter_map(|(key, items)| {
            let mut total: Option<Money> = None;
            for item in items {
                let value = item?;
                match &mut total {
                    None => total = Some(value),
                    Some(sum) if sum.currency == value.currency => sum.amount += value.amount,
                    Some(_) => return None,
                }
            }
            total.map(|money| (key, money))
        })
        .collect()
}

/// Cash per currency: the signed sum of every visible `cash_effect`, including rows the
/// importer couldn't classify. This is the broker's cash as reported, never an FX total.
fn cash_totals(records: &[&LedgerRecord]) -> Vec<Money> {
    let mut totals: BTreeMap<Currency, Decimal> = BTreeMap::new();
    for record in records {
        if let Some(cash) = &record.entry.cash_effect {
            *totals.entry(cash.currency.clone()).or_default() += cash.amount;
        }
    }
    totals
        .into_iter()
        .map(|(currency, amount)| Money::new(amount, currency))
        .collect()
}

/// Signed cash totals per flow label and currency. The label says what the row was, so the
/// owner can see, for example, commissions apart from the trades they belong to.
fn flow_totals(records: &[&LedgerRecord]) -> Vec<FlowTotal> {
    let mut totals: BTreeMap<(String, Currency), (Decimal, u32)> = BTreeMap::new();
    for record in records {
        let Some(cash) = &record.entry.cash_effect else {
            continue;
        };
        let entry = totals
            .entry((flow_label(&record.entry.kind), cash.currency.clone()))
            .or_default();
        entry.0 += cash.amount;
        entry.1 = entry.1.saturating_add(1);
    }
    totals
        .into_iter()
        .map(|((label, currency), (amount, count))| FlowTotal {
            label,
            total: Money::new(amount, currency),
            count,
        })
        .collect()
}

/// Stable label for one row in the flow totals.
fn flow_label(kind: &EntryKind) -> String {
    match kind {
        EntryKind::Deposit => "deposit".to_string(),
        EntryKind::Withdrawal => "withdrawal".to_string(),
        EntryKind::Transfer => "transfer".to_string(),
        EntryKind::Trade { side, .. } => match side {
            Side::Buy => "trade:buy".to_string(),
            Side::Sell => "trade:sell".to_string(),
        },
        EntryKind::Dividend { .. } => "dividend".to_string(),
        EntryKind::Tax {
            kind: TaxKind::Withholding,
            ..
        } => "withholding".to_string(),
        EntryKind::Tax { kind, .. } => format!("tax:{}", tax_kind_label(*kind)),
        EntryKind::Interest => "interest".to_string(),
        EntryKind::Fee { kind, .. } => format!("fee:{}", fee_kind_label(*kind)),
        EntryKind::DerivativeResult { .. } => "derivative_result".to_string(),
        EntryKind::CorporateAction { .. } => "corporate_action".to_string(),
        EntryKind::Grant { .. } => "grant".to_string(),
        EntryKind::Unsupported { .. } => "unsupported".to_string(),
    }
}

/// Lowercase name of a tax kind, matching its serde name.
fn tax_kind_label(kind: TaxKind) -> &'static str {
    match kind {
        TaxKind::Withholding => "withholding",
        TaxKind::OnInterest => "on_interest",
        TaxKind::Other => "other",
    }
}

/// Lowercase name of a fee kind, matching its serde name.
fn fee_kind_label(kind: FeeKind) -> &'static str {
    match kind {
        FeeKind::Commission => "commission",
        FeeKind::Regulatory => "regulatory",
        FeeKind::Swap => "swap",
        FeeKind::FxConversion => "fx_conversion",
        FeeKind::Custody => "custody",
        FeeKind::Other => "other",
    }
}
