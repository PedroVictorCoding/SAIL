//! Comparing the ledger with the broker's own snapshot.
//!
//! The broker is evidence, not truth: a difference is reported with both figures and a
//! short explanation, never smoothed away. Quantities are compared per instrument key, and
//! CFDs stay separate because their key carries the `#cfd` suffix. Cash is compared only
//! when the snapshot states a balance, and only in that currency.
use super::{Position, Reconciliation, ReconciliationStatus, Variance};
use crate::contracts::{Money, StoredSnapshot};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, BTreeSet};

/// Compare a projection with the latest visible snapshot, folding in any variance the
/// projection itself found (for example a sell larger than the open lots).
pub(super) fn reconcile(
    positions: &[Position],
    cfd_positions: &[Position],
    cash: &[Money],
    snapshot: Option<&StoredSnapshot>,
    internal_variances: Vec<Variance>,
) -> (Reconciliation, Option<DateTime<Utc>>) {
    let Some(snapshot) = snapshot else {
        return (
            Reconciliation {
                status: ReconciliationStatus::Unverified,
                variances: internal_variances,
                compared: 0,
            },
            None,
        );
    };

    let mut ledger_quantities: BTreeMap<String, Decimal> = BTreeMap::new();
    for position in positions.iter().chain(cfd_positions) {
        *ledger_quantities
            .entry(position.instrument.key())
            .or_default() += position.quantity;
    }
    let mut broker_quantities: BTreeMap<String, Decimal> = BTreeMap::new();
    for position in &snapshot.snapshot.open_positions {
        *broker_quantities
            .entry(position.instrument.key())
            .or_default() += position.quantity;
    }

    let mut keys: BTreeSet<String> = ledger_quantities.keys().cloned().collect();
    keys.extend(broker_quantities.keys().cloned());

    let mut variances: Vec<Variance> = Vec::new();
    let mut compared: u32 = 0;
    for key in keys {
        let ledger = ledger_quantities.get(&key).copied();
        let broker = broker_quantities.get(&key).copied();
        let ledger_value = ledger.unwrap_or(Decimal::ZERO);
        let broker_value = broker.unwrap_or(Decimal::ZERO);
        compared = compared.saturating_add(1);
        if ledger_value != broker_value {
            let note = match (ledger.is_some(), broker.is_some()) {
                (true, false) => {
                    "the broker's open positions don't list this instrument; it may have been \
                     closed since the export, the export may predate the buy, or the row wasn't \
                     read"
                }
                (false, true) => {
                    "the ledger has no buy for this position; the export usually starts after \
                     the first trade, so the opening purchase is missing"
                }
                _ => {
                    "the ledger and the broker disagree; a trade may be missing from the export, \
                     or the export may start after the first buy"
                }
            };
            variances.push(Variance {
                subject: format!("quantity {key}"),
                ledger: ledger.map(|q| q.to_string()),
                broker: broker.map(|q| q.to_string()),
                note: note.to_string(),
            });
        }
    }

    if let Some(balance) = &snapshot.snapshot.balance {
        let ledger_value = cash
            .iter()
            .find(|money| money.currency == balance.currency)
            .map(|money| money.amount)
            .unwrap_or(Decimal::ZERO);
        compared = compared.saturating_add(1);
        if ledger_value != balance.amount {
            variances.push(Variance {
                subject: format!("cash {}", balance.currency),
                ledger: Some(ledger_value.to_string()),
                broker: Some(balance.amount.to_string()),
                note: "the cash totals differ; a cash operation may be missing from the export, \
                       or it may have been booked in a different account currency"
                    .to_string(),
            });
        }
    }

    variances.extend(internal_variances);
    let status = if compared == 0 {
        ReconciliationStatus::Unverified
    } else if variances.is_empty() {
        ReconciliationStatus::Matched
    } else {
        ReconciliationStatus::Variance
    };
    (
        Reconciliation {
            status,
            variances,
            compared,
        },
        snapshot.snapshot.as_of,
    )
}
