//! First-in, first-out lot matching for trades, splits and grants.
//!
//! FIFO is the method Portuguese capital-gains rules use for securities, and it is the only
//! ordering the reports support: an export states a quantity but not which lot a sale
//! consumed. A sale larger than the open lots is a data gap, so it is capped with a warning
//! and a reconciliation variance instead of becoming a short position.
//!
//! Corporate actions and grants are replayed in the same time order as trades:
//! - a split scales every open lot by `new_units / old_units`, keeping each lot's total
//!   cost, and turns a cash-in-lieu row into a realized fractional sale from the newest
//!   lots (which is the order XTB itself used for LCID);
//! - a grant opens a lot that is marked [`LotOrigin::Granted`], so a zero cost basis is
//!   never mistaken for a purchase whose price went missing.
use super::{Lot, LotOrigin, Position, Realized, Variance};
use crate::contracts::{
    CorporateAction, Currency, EntryKind, Instrument, LedgerRecord, Money, Side,
};
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

    /// Units in the open lots.
    pub(super) fn open_units(&self) -> Decimal {
        self.lots.iter().map(|lot| lot.quantity).sum()
    }

    /// Units still open that were received free with no cost basis. Used for the owner
    /// warning; a granted lot with a stated cost doesn't count.
    pub(super) fn granted_zero_cost_units(&self) -> Decimal {
        self.lots
            .iter()
            .filter(|lot| lot.origin == LotOrigin::Granted)
            .filter(|lot| {
                lot.unit_cost
                    .as_ref()
                    .is_none_or(|unit| unit.amount.is_zero())
            })
            .map(|lot| lot.quantity)
            .sum()
    }

    /// The instrument symbol, for owner-facing warnings.
    pub(super) fn symbol(&self) -> &str {
        &self.instrument.symbol
    }
}

/// Replay every lot-changing row in order: trades, splits and grants. Returns the open
/// collections by instrument key and the realized sales, oldest first.
pub(super) fn build_positions(
    records: &[&LedgerRecord],
    account_currency: Option<&Currency>,
    warnings: &mut Vec<String>,
    variances: &mut Vec<Variance>,
) -> (BTreeMap<String, LotBook>, Vec<Realized>) {
    let mut build = LedgerBuild {
        books: BTreeMap::new(),
        realized: Vec::new(),
        warnings,
        variances,
        missing_quantity_warned: BTreeSet::new(),
        bad_quantity_warned: BTreeSet::new(),
        account_currency: account_currency.cloned(),
    };

    for record in records {
        build.apply(record);
    }

    (build.books, build.realized)
}

/// Units held by instrument key just after each moment in `moments` (ascending), replaying
/// the rows once. A row at exactly a moment counts, as in `project`'s `occurred_by`.
pub(super) fn units_at_moments(
    records: &[&LedgerRecord],
    moments: &[chrono::DateTime<chrono::Utc>],
) -> Vec<BTreeMap<String, Decimal>> {
    let mut warnings = Vec::new();
    let mut variances = Vec::new();
    let mut build = LedgerBuild {
        books: BTreeMap::new(),
        realized: Vec::new(),
        warnings: &mut warnings,
        variances: &mut variances,
        missing_quantity_warned: BTreeSet::new(),
        bad_quantity_warned: BTreeSet::new(),
        account_currency: None,
    };
    let mut out = Vec::with_capacity(moments.len());
    let mut rows = records.iter().peekable();
    for moment in moments {
        while let Some(record) = rows.next_if(|r| r.entry.occurred.at <= *moment) {
            build.apply(record);
        }
        out.push(
            build
                .books
                .iter()
                .map(|(key, book)| (key.clone(), book.open_units()))
                .filter(|(_, units)| *units > Decimal::ZERO)
                .collect(),
        );
    }
    out
}

impl LedgerBuild<'_> {
    /// Apply one row if it changes lots.
    fn apply(&mut self, record: &LedgerRecord) {
        let build = self;
        match &record.entry.kind {
            EntryKind::Trade {
                instrument,
                side,
                quantity,
                price,
                ..
            } => build.trade(record, instrument, *side, *quantity, price.as_ref()),
            EntryKind::CorporateAction {
                instrument,
                action:
                    CorporateAction::Split {
                        new_units,
                        old_units,
                    },
            } => build.split(record, instrument, *new_units, *old_units),
            EntryKind::Grant {
                instrument,
                quantity,
                cost,
            } => build.grant(record, instrument, *quantity, cost.as_ref()),
            _ => {}
        }
    }
}

/// Mutable state while replaying rows, so every helper can warn and realize without a long
/// parameter list.
struct LedgerBuild<'a> {
    books: BTreeMap<String, LotBook>,
    realized: Vec<Realized>,
    warnings: &'a mut Vec<String>,
    variances: &'a mut Vec<Variance>,
    missing_quantity_warned: BTreeSet<String>,
    bad_quantity_warned: BTreeSet<String>,
    account_currency: Option<Currency>,
}

impl LedgerBuild<'_> {
    /// Apply one trade row. A quantity is required to open or consume lots; without one the
    /// row only changes cash and the warning says so once per instrument.
    fn trade(
        &mut self,
        record: &LedgerRecord,
        instrument: &Instrument,
        side: Side,
        quantity: Option<Decimal>,
        price: Option<&Money>,
    ) {
        let key = instrument.key();
        let quantity = match quantity {
            Some(value) if value > Decimal::ZERO => Some(value),
            Some(_) => {
                if self.bad_quantity_warned.insert(key.clone()) {
                    self.warnings.push(format!(
                        "a trade in {key} states a non-positive quantity; it is treated as cash only"
                    ));
                }
                None
            }
            None => None,
        };
        let Some(traded) = quantity else {
            if self.missing_quantity_warned.insert(key.clone()) {
                self.warnings.push(format!(
                    "trades in {key} have no stated quantity; the position can't be derived from the ledger"
                ));
            }
            return;
        };

        match side {
            Side::Buy => {
                let unit_cost =
                    match price {
                        Some(stated) => Some(stated.clone()),
                        // No stated price: the cash the row moved is what was actually paid,
                        // in the account's currency (a EUR account buying a USD share is
                        // debited in EUR). That is the cost basis the owner's tax return uses,
                        // so it is labelled with the cash currency, not the instrument's.
                        None => record.entry.cash_effect.as_ref().map(|cash| {
                            Money::new(cash.amount.abs() / traded, cash.currency.clone())
                        }),
                    };
                self.book(instrument).lots.push_back(Lot {
                    opened: record.entry.occurred.clone(),
                    quantity: traded,
                    unit_cost,
                    origin: LotOrigin::Bought,
                    record_id: record.id.clone(),
                });
            }
            Side::Sell => {
                let matched = match self.books.get_mut(&key) {
                    Some(book) => consume(book, traded),
                    None => (Decimal::ZERO, None),
                };
                let (consumed, cost) = matched;
                if consumed < traded {
                    self.warnings.push(format!(
                        "a sell of {} {} in {key} found only {} units in the ledger; nothing beyond that was consumed",
                        traded, key, consumed
                    ));
                    self.variances.push(Variance {
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
                    return;
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
                self.realized.push(Realized {
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

    /// Apply one split. Every open lot is scaled and keeps its total cost. A stated cash
    /// effect is cash in lieu of the fractional remainder, which is realized against the
    /// newest lots first, matching what the broker did in the LCID case.
    fn split(
        &mut self,
        record: &LedgerRecord,
        instrument: &Instrument,
        new_units: Decimal,
        old_units: Decimal,
    ) {
        if new_units <= Decimal::ZERO || old_units <= Decimal::ZERO {
            self.warnings.push(format!(
                "a split of {} has a non-positive ratio; it was ignored",
                instrument.symbol
            ));
            return;
        }
        let key = instrument.key();
        let Some(book) = self.books.get_mut(&key) else {
            self.warnings.push(format!(
                "a split of {} happened with no open lots; nothing was adjusted",
                instrument.symbol
            ));
            return;
        };

        let mut scaled_total = Decimal::ZERO;
        for lot in book.lots.iter_mut() {
            let before = lot.quantity;
            let after = before * new_units / old_units;
            if let Some(unit) = &mut lot.unit_cost {
                let total = unit.amount * before;
                if !after.is_zero() {
                    unit.amount = total / after;
                }
            }
            lot.quantity = after;
            scaled_total += after;
        }

        let fraction = scaled_total - scaled_total.trunc();
        let Some(cash) = record.entry.cash_effect.as_ref() else {
            // No cash in lieu: the fraction stays as ledger units.
            return;
        };
        if fraction <= Decimal::ZERO {
            return;
        }
        let (removed, cost) = remove_newest(book, fraction);
        if removed.is_zero() {
            return;
        }
        let proceeds = Money::new(cash.amount, cash.currency.clone());
        let result = match &cost {
            Some(cost) if cost.currency == proceeds.currency => Some(Money::new(
                proceeds.amount - cost.amount,
                proceeds.currency.clone(),
            )),
            _ => None,
        };
        self.realized.push(Realized {
            instrument: instrument.clone(),
            sold: record.entry.occurred.clone(),
            quantity: removed,
            proceeds: Some(proceeds),
            cost,
            result,
            record_id: record.id.clone(),
        });
    }

    /// Open a granted lot. Without a stated cost the basis is zero in the account currency,
    /// or unlabelled when the projection has no single cash currency; the lot is always
    /// marked [`LotOrigin::Granted`] so the warning path can find it.
    fn grant(
        &mut self,
        record: &LedgerRecord,
        instrument: &Instrument,
        quantity: Decimal,
        cost: Option<&Money>,
    ) {
        if quantity <= Decimal::ZERO {
            self.warnings.push(format!(
                "a grant of {} states a non-positive quantity; it was ignored",
                instrument.symbol
            ));
            return;
        }
        let unit_cost = match cost {
            Some(cost) => Some(Money::new(cost.amount / quantity, cost.currency.clone())),
            None => self
                .account_currency
                .clone()
                .map(|currency| Money::new(Decimal::ZERO, currency)),
        };
        self.book(instrument).lots.push_back(Lot {
            opened: record.entry.occurred.clone(),
            quantity,
            unit_cost,
            origin: LotOrigin::Granted,
            record_id: record.id.clone(),
        });
    }

    /// The book for an instrument, created on first use.
    fn book(&mut self, instrument: &Instrument) -> &mut LotBook {
        self.books
            .entry(instrument.key())
            .or_insert_with(|| LotBook {
                instrument: instrument.clone(),
                lots: VecDeque::new(),
            })
    }
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

/// Remove units from the newest lots first, returning how many were removed and their cost.
/// This is the cash-in-lieu path of a split, where the broker cashed out the fraction of
/// the most recently acquired shares.
fn remove_newest(book: &mut LotBook, quantity: Decimal) -> (Decimal, Option<Money>) {
    let mut remaining = quantity;
    let mut removed = Decimal::ZERO;
    let mut cost: Option<Money> = None;
    let mut cost_broken = false;

    while remaining > Decimal::ZERO {
        let Some(lot) = book.lots.back_mut() else {
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
            book.lots.pop_back();
        }
        removed += take;
        remaining -= take;
    }

    if cost_broken {
        cost = None;
    }
    (removed, cost)
}
