//! Round 25: the dividend projection's record (monthly snapshots graded against the
//! ledger) and the upcoming-payments calendar.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;

use super::{CapitalService, ServiceError};
use crate::contracts::AccountId;
use crate::dividends::record::{IncomeSnapshot, Receipt, RecordSummary, SnapshotHolding};
use crate::dividends::{Allocation, Scenario};

/// Months of ledger receipts read for grading: 12 for the newest graded snapshot plus
/// the record's span.
const RECEIPT_MONTHS: i64 = 60;

/// The most days ahead the calendar accepts.
pub const MAX_CALENDAR_DAYS: u32 = 366;

impl CapitalService {
    /// Stores this month's projection snapshot for `account` unless one exists. Uses a
    /// one-year projection with nothing reinvested and no allowance, so the number is a
    /// forecast of what the current holdings pay, not of a plan. Returns whether a
    /// snapshot was made.
    pub fn record_income_snapshot_if_due(
        &self,
        account: &AccountId,
        now: DateTime<Utc>,
    ) -> Result<bool, ServiceError> {
        let today = now.date_naive();
        let month = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
        if self.store.has_income_snapshot(account, month)? {
            return Ok(false);
        }
        let outlook =
            self.dividend_projection(account, 1, 0.0, Allocation::Proportional, 0.0, false)?;
        let (inputs, _, _) = self.cached_dividend_inputs(account, now)?;
        let holdings: Vec<SnapshotHolding> = outlook
            .holdings
            .iter()
            .filter_map(|row| {
                let input = inputs.iter().find(|h| h.symbol == row.symbol)?;
                Some(SnapshotHolding {
                    key: input.key.clone(),
                    symbol: row.symbol.clone(),
                    units: input.units,
                    projected_net: row.first_year_net,
                })
            })
            .collect();
        if holdings.is_empty() {
            return Ok(false);
        }
        let year1 = |scenario: Scenario| {
            outlook
                .scenarios
                .iter()
                .find(|p| p.scenario == scenario)
                .and_then(|p| p.years.first())
                .map_or(0.0, |y| y.net)
        };
        let currency = self.account_currency(account)?;
        let snapshot = crate::dividends::record::snapshot(
            month,
            now,
            &currency,
            holdings,
            year1(Scenario::Low),
            year1(Scenario::Base),
            year1(Scenario::High),
            None,
        );
        Ok(self.store.record_income_snapshot(account, &snapshot)?)
    }

    /// The projection's record for `account`.
    pub fn dividend_record(&self, account: &AccountId) -> Result<RecordSummary, ServiceError> {
        let now = Utc::now();
        let snapshots: Vec<IncomeSnapshot> = self.store.income_snapshots(account, now)?;
        let receipts = self.net_receipts(account, now)?;
        Ok(crate::dividends::record::grade(
            &snapshots,
            &receipts,
            now.date_naive(),
        ))
    }

    /// Dividends expected in the next `days` days (1..=[`MAX_CALENDAR_DAYS`]).
    pub fn dividend_calendar(
        &self,
        account: &AccountId,
        days: u32,
    ) -> Result<Vec<crate::dividends::calendar::UpcomingPayment>, ServiceError> {
        if !(1..=MAX_CALENDAR_DAYS).contains(&days) {
            return Err(ServiceError::BadRequest(format!(
                "Days must be between 1 and {MAX_CALENDAR_DAYS}."
            )));
        }
        let now = Utc::now();
        let (holdings, _, _) = self.cached_dividend_inputs(account, now)?;
        let mut paid: BTreeMap<String, Vec<NaiveDate>> = BTreeMap::new();
        for receipt in self.net_receipts(account, now)? {
            paid.entry(receipt.key).or_default().push(receipt.paid);
        }
        Ok(crate::dividends::calendar::upcoming(
            &holdings,
            &paid,
            now.date_naive(),
            days,
        ))
    }

    /// Net dividends the ledger received in the last [`RECEIPT_MONTHS`] months, with the
    /// units held on each payment date.
    fn net_receipts(
        &self,
        account: &AccountId,
        now: DateTime<Utc>,
    ) -> Result<Vec<Receipt>, ServiceError> {
        let received = self.dividends(
            account,
            Some(now - Duration::days(RECEIPT_MONTHS * 31)),
            None,
        )?;
        let paid_at: Vec<DateTime<Utc>> = received.iter().map(|d| d.paid.at).collect();
        let units_then =
            crate::ledger::units_at(account, &self.ledger_records(account, now)?, now, &paid_at);
        Ok(received
            .iter()
            .zip(&units_then)
            .filter_map(|(d, units)| {
                let key = d.instrument.as_ref()?.key();
                let gross = d.gross.amount.to_f64()?;
                let net = match &d.net {
                    Some(net) => net.amount.to_f64()?,
                    None => {
                        gross
                            - d.withheld
                                .as_ref()
                                .and_then(|w| w.amount.to_f64())
                                .map_or(0.0, f64::abs)
                    }
                };
                Some(Receipt {
                    units_then: units.get(&key).and_then(|u| u.to_f64()).unwrap_or(0.0),
                    key,
                    paid: d.paid.at.date_naive(),
                    net,
                })
            })
            .collect())
    }
}
