//! Round 25 (B9, Antigravity's slice): dividends expected in the next days.
//!
//! **Schedule.** From each holding's per-share history (regular payments only, specials
//! excluded with [`super::classify`]):
//! - the typical gap between ex-dates over the last 2 years (median of consecutive gaps;
//!   monthly ≈ 30, quarterly ≈ 91, semi-annual ≈ 182, annual ≈ 365 days);
//! - the next ex-date estimate = the same payment one year earlier + 364 days (keeps the
//!   weekday), or, with under a year of history, the last ex-date + the typical gap.
//!   Roll forward until it is after `today`, and list every one inside `days`.
//! - amount per share = the latest regular payment (no growth inside 90 days).
//!
//! **Pay date.** Ex-date + the holding's median observed lag from ex-date to the
//! ledger's payment (each receipt matched to the latest ex-date in the 75 days before
//! it); with no receipts, ex-date + 21 days and `pay_date_estimated = true`.
//!
//! **Money.** `gross = per_share × units × fx`; `net = gross × (1 − withholding)`.
//!
//! **Confidence.**
//! - `High`: at least 8 regular payments and the gaps' spread (max − min of the last 4)
//!   under 10 days;
//! - `Medium`: at least 3 payments;
//! - `Low`: otherwise, or the last payment is older than 1.5 × the typical gap (the
//!   payer may have stopped).
//!
//! `basis` is one plain sentence, e.g. "Quarterly; last paid 0.24 on 2026-08-08.".
//!
//! **Purity.** No I/O and no clock.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::Holding;

/// The default window, in days.
pub const DEFAULT_DAYS: u32 = 90;

/// How sure the estimate is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// A long, regular schedule.
    High,
    /// Some history.
    Medium,
    /// Little history, or the payer may have stopped.
    Low,
}

/// One expected payment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpcomingPayment {
    /// Ledger key.
    pub key: String,
    /// Symbol.
    pub symbol: String,
    /// Estimated ex-date.
    pub ex_date: NaiveDate,
    /// Estimated pay date.
    pub pay_date: NaiveDate,
    /// No receipts to measure the lag from.
    pub pay_date_estimated: bool,
    /// Per share, trading currency.
    pub per_share: f64,
    /// Units held now.
    pub units: f64,
    /// Gross, account currency.
    pub gross: f64,
    /// Net of withholding, account currency.
    pub net: f64,
    /// How sure.
    pub confidence: Confidence,
    /// One plain sentence.
    pub basis: String,
}

/// Payments expected from `today` (exclusive) to `today + days` (inclusive) by ex-date,
/// sorted by pay date then symbol. `paid` maps ledger key → the ledger's payment dates.
/// M0 stub: none.
pub fn upcoming(
    holdings: &[Holding],
    paid: &BTreeMap<String, Vec<NaiveDate>>,
    today: NaiveDate,
    days: u32,
) -> Vec<UpcomingPayment> {
    let _ = (holdings, paid, today, days);
    Vec::new()
}
