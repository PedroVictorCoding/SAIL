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

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::evidence::SourceError;

/// Minimum days for an annual flow period (`start` to `end`), fixed a priori.
const MIN_ANNUAL_DAYS: i64 = 350;

/// Maximum days for an annual flow period (`start` to `end`), fixed a priori.
const MAX_ANNUAL_DAYS: i64 = 380;

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

/// A single parsed fact observation before merging into [`FiscalYear`].
#[derive(Clone, Copy)]
struct FactValue {
    filed: NaiveDate,
    val: f64,
}

/// Antigravity, round 24, *pure*: reads a company-facts body (module doc).
pub fn parse(body: &[u8]) -> Result<CompanyFacts, SourceError> {
    if body.len() > crate::evidence::MAX_SEC_RESPONSE_BYTES {
        return Err(SourceError::TooLarge);
    }
    let root: Value = serde_json::from_slice(body)
        .map_err(|e| SourceError::Malformed(format!("companyfacts is not JSON: {e}")))?;
    let obj = root
        .as_object()
        .ok_or_else(|| SourceError::Malformed("companyfacts root is not an object".to_string()))?;
    let facts = obj
        .get("facts")
        .and_then(Value::as_object)
        .ok_or_else(|| SourceError::Malformed("companyfacts has no facts object".to_string()))?;

    let cik = obj
        .get("cik")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0);

    let Some(us_gaap) = facts.get("us-gaap").and_then(Value::as_object) else {
        return Ok(CompanyFacts {
            cik,
            years: Vec::new(),
        });
    };

    let dividends_paid = read_concept_chain(
        us_gaap,
        &["PaymentsOfDividendsCommonStock", "PaymentsOfDividends"],
        true,
    );
    let operating_cash_flow = read_concept_chain(
        us_gaap,
        &["NetCashProvidedByUsedInOperatingActivities"],
        true,
    );
    let capex = read_concept_chain(
        us_gaap,
        &["PaymentsToAcquirePropertyPlantAndEquipment"],
        true,
    );
    let net_income = read_concept_chain(us_gaap, &["NetIncomeLoss"], true);
    let long_term_debt =
        read_concept_chain(us_gaap, &["LongTermDebt", "LongTermDebtNoncurrent"], false);
    let cash = read_concept_chain(us_gaap, &["CashAndCashEquivalentsAtCarryingValue"], false);

    let mut by_end: BTreeMap<NaiveDate, FiscalYear> = BTreeMap::new();
    merge_field(&mut by_end, &dividends_paid, |y, v| {
        y.dividends_paid = Some(v)
    });
    merge_field(&mut by_end, &operating_cash_flow, |y, v| {
        y.operating_cash_flow = Some(v)
    });
    merge_field(&mut by_end, &capex, |y, v| y.capex = Some(v));
    merge_field(&mut by_end, &net_income, |y, v| y.net_income = Some(v));
    merge_field(&mut by_end, &long_term_debt, |y, v| {
        y.long_term_debt = Some(v)
    });
    merge_field(&mut by_end, &cash, |y, v| y.cash = Some(v));

    Ok(CompanyFacts {
        cik,
        years: by_end.into_values().collect(),
    })
}

/// Reads a prioritized list of concepts, keeping the first concept that supplies a value
/// for each `end` date.
fn read_concept_chain(
    us_gaap: &Map<String, Value>,
    concepts: &[&str],
    is_flow: bool,
) -> BTreeMap<NaiveDate, FactValue> {
    let mut merged: BTreeMap<NaiveDate, FactValue> = BTreeMap::new();
    for concept in concepts {
        for (end, obs) in read_concept(us_gaap, concept, is_flow) {
            merged.entry(end).or_insert(obs);
        }
    }
    merged
}

/// Reads annual USD observations for one concept; latest `filed` wins per `end` date.
fn read_concept(
    us_gaap: &Map<String, Value>,
    concept: &str,
    is_flow: bool,
) -> BTreeMap<NaiveDate, FactValue> {
    let mut out: BTreeMap<NaiveDate, FactValue> = BTreeMap::new();
    let Some(items) = us_gaap
        .get(concept)
        .and_then(|c| c.get("units"))
        .and_then(|u| u.get("USD"))
        .and_then(Value::as_array)
    else {
        return out;
    };

    for item in items {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let Some(form) = obj.get("form").and_then(Value::as_str) else {
            continue;
        };
        if form != "10-K" && form != "10-K/A" {
            continue;
        }
        if obj.get("fp").and_then(Value::as_str) != Some("FY") {
            continue;
        }
        let Some(end) = parse_date(obj.get("end")) else {
            continue;
        };
        let Some(filed) = parse_date(obj.get("filed")) else {
            continue;
        };
        if is_flow {
            let Some(start) = parse_date(obj.get("start")) else {
                continue;
            };
            let days = (end - start).num_days();
            if !(MIN_ANNUAL_DAYS..=MAX_ANNUAL_DAYS).contains(&days) {
                continue;
            }
        }
        let Some(val) = obj
            .get("val")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
        else {
            continue;
        };

        out.entry(end)
            .and_modify(|existing| {
                if filed >= existing.filed {
                    *existing = FactValue { filed, val };
                }
            })
            .or_insert(FactValue { filed, val });
    }
    out
}

fn parse_date(val: Option<&Value>) -> Option<NaiveDate> {
    val.and_then(Value::as_str)
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}

fn merge_field<F>(
    by_end: &mut BTreeMap<NaiveDate, FiscalYear>,
    values: &BTreeMap<NaiveDate, FactValue>,
    assign: F,
) where
    F: Fn(&mut FiscalYear, f64),
{
    for (&end, &obs) in values {
        let year = by_end.entry(end).or_insert_with(|| FiscalYear {
            end,
            filed: obs.filed,
            ..FiscalYear::default()
        });
        if obs.filed > year.filed {
            year.filed = obs.filed;
        }
        assign(year, obs.val);
    }
}
