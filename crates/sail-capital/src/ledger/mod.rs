//! Deterministic projections of the private ledger (Antigravity's slice, round 7).
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
/// M0 stub: returns an empty, unverified projection with a warning. Antigravity replaces
/// the body; the signature is the contract.
pub fn project(
    account: &AccountId,
    records: &[LedgerRecord],
    snapshots: &[StoredSnapshot],
    as_of: AsOf,
) -> Projection {
    let _ = (records, snapshots);
    Projection {
        account: account.clone(),
        as_of,
        positions: Vec::new(),
        cfd_positions: Vec::new(),
        cash: Vec::new(),
        dividends: Vec::new(),
        realized: Vec::new(),
        flows: Vec::new(),
        snapshot_as_of: None,
        reconciliation: Reconciliation {
            status: ReconciliationStatus::Unverified,
            variances: Vec::new(),
            compared: 0,
        },
        warnings: vec!["ledger projection not implemented yet (M0 stub)".to_string()],
        records_used: 0,
    }
}
