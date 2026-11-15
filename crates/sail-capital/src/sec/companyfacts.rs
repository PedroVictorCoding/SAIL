//! Round 24: SEC XBRL company facts (Antigravity's slice, B3). **This module doc is the
//! spec.**
//!
//! `https://data.sec.gov/api/xbrl/companyfacts/CIK##########.json` lists every value a
//! company has filed, by taxonomy and concept. [`parse`] reads only these US-GAAP
//! concepts, in `USD` units:
//!
//! | Field | Concept (first one present wins) |
//! | --- | --- |
//! | `dividends_paid` | `PaymentsOfDividendsCommonStock`, then `PaymentsOfDividends` |
//! | `operating_cash_flow` | `NetCashProvidedByUsedInOperatingActivities` |
//! | `capex` | `PaymentsToAcquirePropertyPlantAndEquipment` |
//! | `net_income` | `NetIncomeLoss` |
//! | `long_term_debt` | `LongTermDebt`, then `LongTermDebtNoncurrent` |
//! | `cash` | `CashAndCashEquivalentsAtCarryingValue` |
//!
//! Only annual values count: `form` is `10-K` (or `10-K/A`), `fp` is `FY`, and for flow
//! concepts (all but debt and cash) the period from `start` to `end` is 350–380 days. One
//! [`FiscalYear`] per `end` date: for each concept, the value from the latest `filed`
//! date wins (restatements replace), and `filed` of the year is the latest filing that
//! contributed. Years come oldest first.
//!
//! Point in time: a caller asking "as of" a date keeps only years whose `filed` is on or
//! before it ([`CompanyFacts::known_by`]).
//!
//! Hostile input: bodies over [`crate::evidence::MAX_SEC_RESPONSE_BYTES`] →
//! [`SourceError::TooLarge`]; not JSON or no `facts` object → [`SourceError::Malformed`];
//! missing concepts, units or fields are skipped, never a panic; non-finite numbers are
//! dropped.
//!
//! Pure: no I/O, no clock.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::evidence::SourceError;

/// One fiscal year's facts, in USD. `None` when not filed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FiscalYear {
    /// Period end.
    pub end: NaiveDate,
    /// Latest filing date that contributed a value.
    pub filed: NaiveDate,
    /// Cash paid as dividends (positive).
    pub dividends_paid: Option<f64>,
    /// Net cash from operations.
    pub operating_cash_flow: Option<f64>,
    /// Capital expenditure (positive).
    pub capex: Option<f64>,
    /// Net income.
    pub net_income: Option<f64>,
    /// Long-term debt at period end.
    pub long_term_debt: Option<f64>,
    /// Cash and equivalents at period end.
    pub cash: Option<f64>,
}

/// A company's annual facts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompanyFacts {
    /// SEC CIK, as in the body's `cik`.
    pub cik: u64,
    /// Fiscal years, oldest first.
    pub years: Vec<FiscalYear>,
}

impl CompanyFacts {
    /// The facts as they were known on `as_of`: years filed on or before it.
    pub fn known_by(&self, as_of: NaiveDate) -> CompanyFacts {
        CompanyFacts {
            cik: self.cik,
            years: self
                .years
                .iter()
                .filter(|y| y.filed <= as_of)
                .cloned()
                .collect(),
        }
    }
}

/// Antigravity, round 24, *pure*: reads a company-facts body (module doc). M0 stub: an
/// empty result for any body within the size cap.
pub fn parse(body: &[u8]) -> Result<CompanyFacts, SourceError> {
    if body.len() > crate::evidence::MAX_SEC_RESPONSE_BYTES {
        return Err(SourceError::TooLarge);
    }
    Ok(CompanyFacts::default())
}
