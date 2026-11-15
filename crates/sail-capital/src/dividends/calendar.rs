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

use chrono::{Duration, NaiveDate};
use serde::{Deserialize, Serialize};

use super::growth::{PaymentKind, classify};
use super::{Holding, Payment};

/// The default window, in days (fixed a priori).
pub const DEFAULT_DAYS: u32 = 90;

/// Default pay-date lag in days when no ledger receipts exist (fixed a priori).
const DEFAULT_PAY_LAG_DAYS: i64 = 21;

/// Maximum lookback in days to match a ledger receipt to an ex-date (fixed a priori).
const RECEIPT_MATCH_WINDOW_DAYS: i64 = 75;

/// Week-aligned year step in days (keeps weekday, fixed a priori).
const ANNUAL_STEP_DAYS: i64 = 364;

/// Lookback window in days for typical-gap estimation (fixed a priori).
const GAP_LOOKBACK_DAYS: i64 = 730;

/// Minimum regular payments for High confidence (fixed a priori).
const HIGH_CONF_MIN_PAYMENTS: usize = 8;

/// Maximum spread (days) of last 4 gaps for High confidence (fixed a priori).
const HIGH_CONF_MAX_SPREAD_DAYS: i64 = 10;

/// Minimum regular payments for Medium confidence (fixed a priori).
const MEDIUM_CONF_MIN_PAYMENTS: usize = 3;

/// Staleness multiplier on typical gap that forces Low confidence (fixed a priori).
const STALE_GAP_MULTIPLIER: f64 = 1.5;

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
pub fn upcoming(
    holdings: &[Holding],
    paid: &BTreeMap<String, Vec<NaiveDate>>,
    today: NaiveDate,
    days: u32,
) -> Vec<UpcomingPayment> {
    if days == 0 {
        return Vec::new();
    }
    let window_end = today + Duration::days(i64::from(days));
    let mut out = Vec::new();

    for holding in holdings {
        if !holding.units.is_finite()
            || holding.units <= 0.0
            || !holding.fx.is_finite()
            || holding.fx <= 0.0
        {
            continue;
        }
        let regular: Vec<Payment> = classify(&holding.history)
            .into_iter()
            .filter_map(|(p, kind)| {
                (kind == PaymentKind::Regular
                    && p.ex_date <= today
                    && p.per_share.is_finite()
                    && p.per_share > 0.0)
                    .then_some(p)
            })
            .collect();

        if regular.len() < 2 {
            continue;
        }
        let latest = regular[regular.len() - 1];
        let Some(typical_gap) = estimate_typical_gap(&regular, today) else {
            continue;
        };

        let ex_dates = project_ex_dates(&regular, typical_gap, today, window_end);
        if ex_dates.is_empty() {
            continue;
        }

        let (pay_lag, pay_date_estimated) =
            estimate_pay_lag(&regular, paid.get(&holding.key).map(Vec::as_slice));
        let confidence = assess_confidence(&regular, typical_gap, today, latest.ex_date);
        let basis = format!(
            "{}; last paid {:.2} on {}.",
            cadence_label(typical_gap),
            latest.per_share,
            latest.ex_date
        );

        let gross = latest.per_share * holding.units * holding.fx;
        let withholding = holding.withholding.clamp(0.0, 1.0);
        let net = gross * (1.0 - withholding);

        for ex_date in ex_dates {
            out.push(UpcomingPayment {
                key: holding.key.clone(),
                symbol: holding.symbol.clone(),
                ex_date,
                pay_date: ex_date + Duration::days(pay_lag),
                pay_date_estimated,
                per_share: latest.per_share,
                units: holding.units,
                gross,
                net,
                confidence,
                basis: basis.clone(),
            });
        }
    }

    out.sort_by(|a, b| a.pay_date.cmp(&b.pay_date).then(a.symbol.cmp(&b.symbol)));
    out
}

fn estimate_typical_gap(regular: &[Payment], today: NaiveDate) -> Option<i64> {
    let cutoff = today - Duration::days(GAP_LOOKBACK_DAYS);
    let recent: Vec<&Payment> = regular.iter().filter(|p| p.ex_date >= cutoff).collect();
    let sample: Vec<NaiveDate> = if recent.len() >= 2 {
        recent.into_iter().map(|p| p.ex_date).collect()
    } else {
        regular.iter().map(|p| p.ex_date).collect()
    };
    let mut gaps: Vec<i64> = sample
        .windows(2)
        .map(|w| (w[1] - w[0]).num_days())
        .filter(|&g| g > 0)
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_unstable();
    Some(median_sorted(&gaps))
}

fn project_ex_dates(
    regular: &[Payment],
    typical_gap: i64,
    today: NaiveDate,
    window_end: NaiveDate,
) -> Vec<NaiveDate> {
    let first = regular[0].ex_date;
    let latest = regular[regular.len() - 1].ex_date;
    let mut dates = Vec::new();

    if (latest - first).num_days() >= 330 {
        let cycle_start = latest - Duration::days(365);
        for p in regular.iter().filter(|p| p.ex_date > cycle_start) {
            let mut candidate = p.ex_date + Duration::days(ANNUAL_STEP_DAYS);
            while candidate <= today {
                candidate += Duration::days(ANNUAL_STEP_DAYS);
            }
            while candidate <= window_end {
                dates.push(candidate);
                candidate += Duration::days(ANNUAL_STEP_DAYS);
            }
        }
    } else {
        let mut candidate = latest + Duration::days(typical_gap);
        while candidate <= today {
            candidate += Duration::days(typical_gap);
        }
        while candidate <= window_end {
            dates.push(candidate);
            candidate += Duration::days(typical_gap);
        }
    }

    dates.sort_unstable();
    dates.dedup();
    dates
}

fn estimate_pay_lag(regular: &[Payment], receipts: Option<&[NaiveDate]>) -> (i64, bool) {
    let Some(receipt_dates) = receipts else {
        return (DEFAULT_PAY_LAG_DAYS, true);
    };
    let mut lags = Vec::new();
    for &paid_date in receipt_dates {
        let matched = regular
            .iter()
            .rev()
            .find(|p| {
                let diff = (paid_date - p.ex_date).num_days();
                (0..=RECEIPT_MATCH_WINDOW_DAYS).contains(&diff)
            })
            .map(|p| (paid_date - p.ex_date).num_days());
        if let Some(lag) = matched {
            lags.push(lag);
        }
    }
    if lags.is_empty() {
        (DEFAULT_PAY_LAG_DAYS, true)
    } else {
        lags.sort_unstable();
        (median_sorted(&lags), false)
    }
}

fn assess_confidence(
    regular: &[Payment],
    typical_gap: i64,
    today: NaiveDate,
    latest_ex: NaiveDate,
) -> Confidence {
    let age_days = (today - latest_ex).num_days() as f64;
    if age_days > STALE_GAP_MULTIPLIER * typical_gap as f64 {
        return Confidence::Low;
    }
    if regular.len() >= HIGH_CONF_MIN_PAYMENTS {
        let all_gaps: Vec<i64> = regular
            .windows(2)
            .map(|w| (w[1].ex_date - w[0].ex_date).num_days())
            .filter(|&g| g > 0)
            .collect();
        let last_four = &all_gaps[all_gaps.len().saturating_sub(4)..];
        if let (Some(&min_g), Some(&max_g)) = (last_four.iter().min(), last_four.iter().max())
            && (max_g - min_g) < HIGH_CONF_MAX_SPREAD_DAYS
        {
            return Confidence::High;
        }
    }
    if regular.len() >= MEDIUM_CONF_MIN_PAYMENTS {
        Confidence::Medium
    } else {
        Confidence::Low
    }
}

fn cadence_label(gap_days: i64) -> &'static str {
    if gap_days <= 40 {
        "Monthly"
    } else if gap_days <= 120 {
        "Quarterly"
    } else if gap_days <= 220 {
        "Semi-annual"
    } else {
        "Annual"
    }
}

fn median_sorted(sorted: &[i64]) -> i64 {
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid] + 1) / 2
    }
}
