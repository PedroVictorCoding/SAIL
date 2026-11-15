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

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::Payment;
use crate::sec::companyfacts::CompanyFacts;

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

/// Antigravity, round 24, *pure*: the grade (module doc). M0 stub: Unknown.
pub fn grade(facts: Option<&CompanyFacts>, history: &[Payment], as_of: NaiveDate) -> Safety {
    let _ = (facts, history, as_of);
    Safety {
        reasons: vec!["No annual cash-flow filings were found.".to_string()],
        ..Safety::default()
    }
}
