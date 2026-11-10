//! Dividend and withholding matching.
//!
//! A broker reports a dividend credit and, separately, the tax withheld on it. Neither row
//! points at the other, so they are matched by instrument, currency and a small date window.
//! Matching is global nearest-first: every candidate pair is ordered by date distance, so
//! the result does not depend on the row order inside the report. An unmatched withholding
//! is kept in the flows and warned about, never dropped.
use super::DividendView;
use crate::contracts::{Currency, EntryKind, EventTime, Instrument, LedgerRecord, Money, TaxKind};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use std::collections::BTreeSet;

/// Largest gap, in local days, between a dividend credit and its withholding row.
const WITHHOLDING_WINDOW_DAYS: i64 = 3;

/// A dividend credit seen while scanning the ledger.
struct DividendDraft {
    /// Ledger record id.
    record_id: String,
    /// The paying security, if the row names it.
    instrument: Option<Instrument>,
    /// When it was credited.
    paid: EventTime,
    /// Gross amount credited, positive.
    gross: Money,
    /// Local calendar date of the credit, used for matching only.
    date: NaiveDate,
    /// Index into the withholding list, once matched.
    matched: Option<usize>,
}

/// A withholding row seen while scanning the ledger.
struct WithholdingRow {
    /// Ledger record id.
    record_id: String,
    /// Instrument key, if the row names a security.
    key: Option<String>,
    /// Currency of the withheld amount.
    currency: Currency,
    /// Withheld amount, positive.
    amount: Decimal,
    /// Local calendar date of the withholding.
    date: NaiveDate,
}

/// Build every [`DividendView`] in ledger order and match the withholding rows to them.
pub(super) fn build_dividends(
    records: &[&LedgerRecord],
    warnings: &mut Vec<String>,
) -> Vec<DividendView> {
    let mut drafts: Vec<DividendDraft> = Vec::new();
    let mut taxes: Vec<WithholdingRow> = Vec::new();

    for record in records {
        match &record.entry.kind {
            EntryKind::Dividend { instrument } => {
                let Some(cash) = &record.entry.cash_effect else {
                    warnings.push(format!(
                        "dividend {} has no cash amount; it can't be shown as a dividend",
                        record.id
                    ));
                    continue;
                };
                drafts.push(DividendDraft {
                    record_id: record.id.clone(),
                    instrument: instrument.clone(),
                    paid: record.entry.occurred.clone(),
                    gross: Money::new(cash.amount.abs(), cash.currency.clone()),
                    date: record.entry.occurred.at.date_naive(),
                    matched: None,
                });
            }
            EntryKind::Tax {
                kind: TaxKind::Withholding,
                instrument,
            } => {
                let Some(cash) = &record.entry.cash_effect else {
                    warnings.push(format!(
                        "withholding {} has no cash amount; it can't be matched to a dividend",
                        record.id
                    ));
                    continue;
                };
                taxes.push(WithholdingRow {
                    record_id: record.id.clone(),
                    key: instrument.as_ref().map(|item| item.key()),
                    currency: cash.currency.clone(),
                    amount: cash.amount.abs(),
                    date: record.entry.occurred.at.date_naive(),
                });
            }
            _ => {}
        }
    }

    let mut pairs: Vec<(i64, usize, usize)> = Vec::new();
    for (dividend_index, dividend) in drafts.iter().enumerate() {
        let Some(dividend_key) = dividend.instrument.as_ref().map(|item| item.key()) else {
            continue;
        };
        for (tax_index, tax) in taxes.iter().enumerate() {
            if tax.key.as_deref() != Some(dividend_key.as_str()) {
                continue;
            }
            if tax.currency != dividend.gross.currency {
                continue;
            }
            let distance = (dividend.date - tax.date).num_days().abs();
            if distance <= WITHHOLDING_WINDOW_DAYS {
                pairs.push((distance, dividend_index, tax_index));
            }
        }
    }
    pairs.sort();

    let mut used_dividends: BTreeSet<usize> = BTreeSet::new();
    let mut used_taxes: BTreeSet<usize> = BTreeSet::new();
    for (_, dividend_index, tax_index) in pairs {
        if used_dividends.contains(&dividend_index) || used_taxes.contains(&tax_index) {
            continue;
        }
        used_dividends.insert(dividend_index);
        used_taxes.insert(tax_index);
        drafts[dividend_index].matched = Some(tax_index);
    }

    for (tax_index, tax) in taxes.iter().enumerate() {
        if used_taxes.contains(&tax_index) {
            continue;
        }
        warnings.push(format!(
            "withholding of {} {} on {} (record {}) has no matching dividend within {} days; it \
             stays in the flows",
            tax.amount, tax.currency, tax.date, tax.record_id, WITHHOLDING_WINDOW_DAYS
        ));
    }

    let mut views = Vec::with_capacity(drafts.len());
    for draft in drafts {
        let mut record_ids = vec![draft.record_id.clone()];
        let withheld = match draft.matched {
            Some(tax_index) => {
                record_ids.push(taxes[tax_index].record_id.clone());
                Some(Money::new(
                    taxes[tax_index].amount,
                    taxes[tax_index].currency.clone(),
                ))
            }
            None => None,
        };
        let net = match &withheld {
            Some(withheld) if withheld.currency == draft.gross.currency => Some(Money::new(
                draft.gross.amount - withheld.amount,
                draft.gross.currency.clone(),
            )),
            Some(_) => None,
            None => Some(draft.gross.clone()),
        };
        views.push(DividendView {
            instrument: draft.instrument,
            paid: draft.paid,
            gross: draft.gross,
            withheld,
            net,
            record_ids,
        });
    }
    views
}
