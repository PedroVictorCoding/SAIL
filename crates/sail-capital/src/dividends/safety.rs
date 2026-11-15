//! Round 24: dividend safety grades (Antigravity's slice, B3). **This module doc is the
//! spec.**
//!
//! From a company's annual facts known by `as_of` ([`CompanyFacts::known_by`]) and its
//! per-share payment history:
//! - **FCF cover** of the latest fiscal year = `(operating_cash_flow − capex) /
//!   dividends_paid`; `None` when either flow is missing or nothing was paid.
//! - **Earnings cover** = `net_income / dividends_paid`.
//! - **Streak** = complete calendar years before `as_of`, counting back from the last
//!   one, in which the per-share total was paid and not below the year before (the first
//!   paying year counts).
//! - **Cut** in the last N years = a complete calendar year whose per-share total is
//!   below 0.9× the year before (specials are not separated here: the caller passes the
//!   regular payments).
//!
//! Grade (fixed a priori, first match wins):
//! 1. no facts year, or FCF cover `None` → [`SafetyGrade::Unknown`];
//! 2. FCF cover < 0.7 in each of the latest two years → [`SafetyGrade::E`];
//! 3. FCF cover < 1.0, or a cut in the last 3 years → [`SafetyGrade::D`];
//! 4. FCF cover ≥ 1.5, streak ≥ 10 and no cut in the last 5 years → [`SafetyGrade::A`];
//! 5. FCF cover ≥ 1.2 → [`SafetyGrade::B`];
//! 6. otherwise → [`SafetyGrade::C`].
//!
//! `reasons` has one plain sentence per input that decided the grade, with its number,
//! e.g. "Free cash flow covered the dividend 1.8× in fiscal 2025." and "Raised or held
//! the dividend for 12 years in a row." Unknown says why ("No annual cash-flow filings
//! were found.").
//!
//! Pure: no I/O, no clock.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

use super::Payment;
use crate::sec::companyfacts::{CompanyFacts, FiscalYear};

/// FCF cover floor for Grade A (fixed a priori).
const GRADE_A_FCF_COVER: f64 = 1.5;
/// Minimum dividend streak in years for Grade A (fixed a priori).
const GRADE_A_STREAK_YEARS: u32 = 10;
/// Lookback window in years for no-cut requirement on Grade A (fixed a priori).
const GRADE_A_NO_CUT_YEARS: i32 = 5;
/// FCF cover floor for Grade B (fixed a priori).
const GRADE_B_FCF_COVER: f64 = 1.2;
/// FCF cover floor below which a payer is Grade D (fixed a priori).
const GRADE_D_FCF_COVER: f64 = 1.0;
/// Lookback window in years for a recent cut triggering Grade D (fixed a priori).
const GRADE_D_CUT_YEARS: i32 = 3;
/// FCF cover ceiling for two consecutive years triggering Grade E (fixed a priori).
const GRADE_E_FCF_COVER: f64 = 0.7;
/// Year-over-year DPS ratio below which a year counts as a cut (fixed a priori).
const CUT_THRESHOLD_RATIO: f64 = 0.9;

/// A safety grade. JSON: `"a"`, `"b"`, `"c"`, `"d"`, `"e"` or `"unknown"`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SafetyGrade {
    /// Well covered, long record.
    A,
    /// Covered with room.
    B,
    /// Just covered.
    C,
    /// Not covered, or cut recently.
    D,
    /// Badly uncovered two years running.
    E,
    /// Not enough facts (funds, non-US listings).
    #[default]
    Unknown,
}

impl SafetyGrade {
    /// Whether this grade is at least `floor` (A is best; Unknown meets no floor).
    pub fn at_least(self, floor: SafetyGrade) -> bool {
        self != SafetyGrade::Unknown && self <= floor
    }
}

/// One holding's dividend safety.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Safety {
    /// The grade.
    pub grade: SafetyGrade,
    /// Free cash flow over dividends paid, latest fiscal year.
    pub fcf_cover: Option<f64>,
    /// Net income over dividends paid, latest fiscal year.
    pub earnings_cover: Option<f64>,
    /// Years in a row the dividend was paid and not lowered.
    pub streak_years: u32,
    /// Plain sentences behind the grade.
    pub reasons: Vec<String>,
}

/// Antigravity, round 24, *pure*: the grade (module doc).
pub fn grade(facts: Option<&CompanyFacts>, history: &[Payment], as_of: NaiveDate) -> Safety {
    let by_year = annual_totals(history, as_of);
    let last_complete = as_of.year().saturating_sub(1);
    let streak_years = compute_streak(&by_year, last_complete);
    let cut_3y = has_cut(&by_year, last_complete, GRADE_D_CUT_YEARS);
    let cut_5y = has_cut(&by_year, last_complete, GRADE_A_NO_CUT_YEARS);

    let known = facts.map(|f| f.known_by(as_of));
    let years = known.as_ref().map(|k| k.years.as_slice()).unwrap_or(&[]);
    let Some(latest) = years.last() else {
        return Safety {
            grade: SafetyGrade::Unknown,
            fcf_cover: None,
            earnings_cover: None,
            streak_years,
            reasons: vec!["No annual cash-flow filings were found.".to_string()],
        };
    };

    let fcf_cover = year_fcf_cover(latest);
    let earnings_cover = year_earnings_cover(latest);
    let Some(latest_fcf) = fcf_cover else {
        return Safety {
            grade: SafetyGrade::Unknown,
            fcf_cover: None,
            earnings_cover,
            streak_years,
            reasons: vec![
                "Latest annual filing is missing free cash flow or dividend data.".to_string(),
            ],
        };
    };

    let prev_year = if years.len() >= 2 {
        Some(&years[years.len() - 2])
    } else {
        None
    };
    let prev_fcf = prev_year.and_then(year_fcf_cover);

    let mut reasons = vec![format!(
        "Free cash flow covered the dividend {latest_fcf:.1}× in fiscal {}.",
        latest.end.year()
    )];

    let grade = if latest_fcf < GRADE_E_FCF_COVER && prev_fcf.is_some_and(|p| p < GRADE_E_FCF_COVER)
    {
        if let (Some(py), Some(pf)) = (prev_year, prev_fcf) {
            reasons.push(format!(
                "Free cash flow covered the dividend {pf:.1}× in fiscal {} (below 0.7× for two years).",
                py.end.year()
            ));
        }
        SafetyGrade::E
    } else if latest_fcf < GRADE_D_FCF_COVER || cut_3y {
        if cut_3y {
            reasons.push("The annual dividend was cut within the last 3 years.".to_string());
        }
        SafetyGrade::D
    } else if latest_fcf >= GRADE_A_FCF_COVER && streak_years >= GRADE_A_STREAK_YEARS && !cut_5y {
        reasons.push(format!(
            "Raised or held the dividend for {streak_years} years in a row."
        ));
        SafetyGrade::A
    } else if latest_fcf >= GRADE_B_FCF_COVER {
        if streak_years >= 2 {
            reasons.push(format!(
                "Raised or held the dividend for {streak_years} years in a row."
            ));
        }
        SafetyGrade::B
    } else {
        SafetyGrade::C
    };

    Safety {
        grade,
        fcf_cover: Some(latest_fcf),
        earnings_cover,
        streak_years,
        reasons,
    }
}

fn year_fcf_cover(year: &FiscalYear) -> Option<f64> {
    match (year.operating_cash_flow, year.capex, year.dividends_paid) {
        (Some(ocf), Some(capex), Some(div))
            if ocf.is_finite() && capex.is_finite() && div.is_finite() && div > 0.0 =>
        {
            Some((ocf - capex) / div)
        }
        _ => None,
    }
}

fn year_earnings_cover(year: &FiscalYear) -> Option<f64> {
    match (year.net_income, year.dividends_paid) {
        (Some(ni), Some(div)) if ni.is_finite() && div.is_finite() && div > 0.0 => Some(ni / div),
        _ => None,
    }
}

fn annual_totals(history: &[Payment], as_of: NaiveDate) -> BTreeMap<i32, f64> {
    let mut totals = BTreeMap::new();
    for p in history {
        if p.ex_date <= as_of && p.per_share.is_finite() && p.per_share > 0.0 {
            *totals.entry(p.ex_date.year()).or_insert(0.0) += p.per_share;
        }
    }
    totals
}

fn compute_streak(by_year: &BTreeMap<i32, f64>, last_complete: i32) -> u32 {
    let mut streak = 0u32;
    let mut year = last_complete;
    loop {
        let curr = by_year.get(&year).copied().unwrap_or(0.0);
        if curr <= 0.0 {
            break;
        }
        let prev = by_year.get(&(year - 1)).copied().unwrap_or(0.0);
        if prev <= 0.0 {
            streak += 1;
            break;
        }
        if curr + 1e-12 >= prev {
            streak += 1;
            year -= 1;
        } else {
            break;
        }
    }
    streak
}

fn has_cut(by_year: &BTreeMap<i32, f64>, last_complete: i32, lookback_years: i32) -> bool {
    let start = last_complete - (lookback_years - 1);
    (start..=last_complete).any(|year| {
        let prev = by_year.get(&(year - 1)).copied().unwrap_or(0.0);
        let curr = by_year.get(&year).copied().unwrap_or(0.0);
        prev > 0.0 && curr < CUT_THRESHOLD_RATIO * prev
    })
}
