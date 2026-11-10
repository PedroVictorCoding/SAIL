//! First-in, first-out lot matching for trade rows.
//!
//! FIFO is the method Portuguese capital-gains rules use for securities, and it is the only
//! ordering the reports support: an export states a quantity but not which lot a sale
//! consumed. A sale larger than the open lots is a data gap, so it is capped with a warning
//! and a reconciliation variance instead of becoming a short position.
use super::{Lot, Position, Realized, Variance};
use crate::contracts::{EntryKind, Instrument, LedgerRecord, Money, Side};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The open lots of one instrument, oldest first.
pub(super) struct LotBook {
    /// The instrument as first seen for this key. Later rows with the same key only refine
    /// it; the key itself decides identity.
    instrument: Instrument,
    /// Open lots, oldest first.
    lots: VecDeque<Lot>,
}

impl LotBook {
    /// Turn the open lots into a [`Position`], or `None` when nothing is left. The cost
    /// basis is only stated when every lot has a cost in one currency; a mixed or missing
    /// cost stays `None` rather than being approximated.
    pub(super) fn into_position(self, market_values: &BTreeMap<String, Money>) -> Option<Position> {
        if self.lots.is_empty() {
            return None;
        }
        let mut quantity = Decimal::ZERO;
        let mut basis: Option<Money> = None;
        let mut basis_complete = true;
        for lot in &self.lots {
            quantity += lot.quantity;
            if !basis_complete {
                continue;
            }
            match &lot.unit_cost {
                None => basis_complete = false,
                Some(unit) => {
                    let line = unit.amount * lot.quantity;
                    match &mut basis {
                        None => basis = Some(Money::new(line, unit.currency.clone())),
                        Some(total) if total.currency == unit.currency => total.amount += line,
                        Some(_) => basis_complete = false,
                    }
                }
            }
        }
        let key = self.instrument.key();
        Some(Position {
            broker_market_value: market_values.get(&key).cloned(),
            instrument: self.instrument,
            quantity,
            lots: self.lots.into_iter().collect(),
            cost_basis: if basis_complete { basis } else { None },
        })
    }
}

/// Replay every trade row in order. Returns the open collections by instrument key and the
/// FIFO-matched sales, oldest first.
pub(super) fn build_positions(
    records: &[&LedgerRecord],
    warnings: &mut Vec<String>,
    variances: &mut Vec<Variance>,
) -> (BTreeMap<String, LotBook>, Vec<Realized>) {
    let mut books: BTreeMap<String, LotBook> = BTreeMap::new();
    let mut realized: Vec<Realized> = Vec::new();
    let mut missing_quantity_warned: BTreeSet<String> = BTreeSet::new();
    let mut bad_quantity_warned: BTreeSet<String> = BTreeSet::new();

    for record in records {
        let EntryKind::Trade {
            instrument,
            side,
            quantity,
            price,
            ..
        } = &record.entry.kind
        else {
            continue;
        };
        let key = instrument.key();
        let quantity = match quantity {
            Some(value) if *value > Decimal::ZERO => Some(*value),
            Some(_) => {
                if bad_quantity_warned.insert(key.clone()) {
                    warnings.push(format!(
                        "a trade in {key} states a non-positive quantity; it is treated as cash only"
                    ));
                }
                None
            }
            None => None,
        };
        let Some(traded) = quantity else {
            if missing_quantity_warned.insert(key.clone()) {
                warnings.push(format!(
                    "trades in {key} have no stated quantity; the position can't be derived from the ledger"
                ));
            }
            continue;
        };

        match side {
            Side::Buy => {
                let unit_cost = match price {
                    Some(stated) => Some(stated.clone()),
                    None => match &record.entry.cash_effect {
                        Some(cash) if instrument.currency.as_ref() == Some(&cash.currency) => Some(
                            Money::new(cash.amount.abs() / traded, cash.currency.clone()),
                        ),
                        _ => None,
                    },
                };
                let book = books.entry(key).or_insert_with(|| LotBook {
                    instrument: instrument.clone(),
                    lots: VecDeque::new(),
                });
                book.lots.push_back(Lot {
                    opened: record.entry.occurred.clone(),
                    quantity: traded,
                    unit_cost,
                    record_id: record.id.clone(),
                });
            }
            Side::Sell => {
                let matched = match books.get_mut(&key) {
                    Some(book) => consume(book, traded),
                    None => (Decimal::ZERO, None),
                };
                let (consumed, cost) = matched;
                if consumed < traded {
                    warnings.push(format!(
                        "a sell of {} {} in {key} found only {} units in the ledger; nothing beyond that was consumed",
                        traded, key, consumed
                    ));
                    variances.push(Variance {
                        subject: format!("fifo {key}"),
                        ledger: Some(consumed.to_string()),
                        broker: Some(traded.to_string()),
                        note: "the ledger has fewer units than the report says were sold; the \
                               export usually starts after the first buy. The position is never \
                               allowed to go negative, so the excess is left as a variance."
                            .to_string(),
                    });
                }
                if consumed.is_zero() {
                    continue;
                }
                let proceeds = match price {
                    Some(stated) => Some(Money::new(
                        stated.amount * consumed,
                        stated.currency.clone(),
                    )),
                    // The row's cash amount covers the whole stated sale. When the ledger
                    // could only match part of it, attributing the full amount to the
                    // matched units would overstate the result, so it stays unknown.
                    None if consumed == traded => record
                        .entry
                        .cash_effect
                        .as_ref()
                        .map(|cash| Money::new(cash.amount.abs(), cash.currency.clone())),
                    None => None,
                };
                let result = match (&proceeds, &cost) {
                    (Some(proceeds), Some(cost)) if proceeds.currency == cost.currency => Some(
                        Money::new(proceeds.amount - cost.amount, proceeds.currency.clone()),
                    ),
                    _ => None,
                };
                realized.push(Realized {
                    instrument: instrument.clone(),
                    sold: record.entry.occurred.clone(),
                    quantity: consumed,
                    proceeds,
                    cost,
                    result,
                    record_id: record.id.clone(),
                });
            }
        }
    }

    (books, realized)
}

/// Consume lots oldest first, returning how many units were matched and their FIFO cost.
/// The cost is `None` when any matched lot's cost is unknown or the lots mix currencies.
fn consume(book: &mut LotBook, quantity: Decimal) -> (Decimal, Option<Money>) {
    let mut remaining = quantity;
    let mut matched = Decimal::ZERO;
    let mut cost: Option<Money> = None;
    let mut cost_broken = false;

    while remaining > Decimal::ZERO {
        let Some(lot) = book.lots.front_mut() else {
            break;
        };
        let take = lot.quantity.min(remaining);
        if !cost_broken {
            match &lot.unit_cost {
                None => cost_broken = true,
                Some(unit) => {
                    let line = unit.amount * take;
                    match &mut cost {
                        None => cost = Some(Money::new(line, unit.currency.clone())),
                        Some(total) if total.currency == unit.currency => total.amount += line,
                        Some(_) => cost_broken = true,
                    }
                }
            }
        }
        lot.quantity -= take;
        if lot.quantity.is_zero() {
            book.lots.pop_front();
        }
        matched += take;
        remaining -= take;
    }

    if cost_broken {
        cost = None;
    }
    (matched, cost)
}
