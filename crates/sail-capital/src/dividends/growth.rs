//! Growth from a per-share payment history: annual sums, CAGR, volatility and the
//! payment schedule used by the projection.
//!
//! Round 24: special payments are separated first ([`classify`]) and left out of the
//! trailing twelve months, the annual totals and the schedule, and the shrinkage of the
//! raw growth toward zero is a parameter ([`growth_with`]) so the calibration backtest
//! can compare values. [`growth`] keeps the round 14 value, `n / (n + 3)`.

use std::collections::BTreeMap;

use chrono::{Datelike, Days, NaiveDate};

use serde::{Deserialize, Serialize};

use super::{Growth, MAX_GROWTH, Payment};

/// The shrinkage constant `k` in `n / (n + k)` used since round 14.
pub const DEFAULT_SHRINK_K: f64 = 3.0;

/// A payment above this multiple of the recent regular median is a special (fixed a
/// priori: a raise of more than 150% in one payment is far rarer than a one-off).
const SPECIAL_MULTIPLE: f64 = 2.5;

/// Days before a payment whose regular payments set its comparison median.
const SPECIAL_LOOKBACK_DAYS: u64 = 400;

/// Regular payments needed in the lookback before a payment can be called special.
const SPECIAL_MIN_PRIOR: usize = 2;

/// A later payment of at least this share of a candidate special, within the lookback
/// after it, means the higher level is the new normal (a frequency change, e.g.
/// quarterly to annual, or a rebased payout), so the candidate is regular.
const NEW_LEVEL_SHARE: f64 = 0.6;

/// Whether a payment is part of the regular schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentKind {
    /// Part of the schedule.
    Regular,
    /// A one-off: more than 2.5× the median regular payment of the 400 days before it.
    Special,
}

/// Round 24, *pure*: each payment with its kind, oldest first. A payment is special when
/// it is above [`SPECIAL_MULTIPLE`] × the median regular payment of the
/// [`SPECIAL_LOOKBACK_DAYS`] before it (with at least [`SPECIAL_MIN_PRIOR`] of them), unless
/// a payment in the same span after it is at least [`NEW_LEVEL_SHARE`] of it. Only
/// payments in `history` are used, so a caller that cuts the history at a date gets the
/// classification as it was knowable then.
pub fn classify(history: &[Payment]) -> Vec<(Payment, PaymentKind)> {
    let mut payments: Vec<Payment> = history
        .iter()
        .copied()
        .filter(|p| p.per_share.is_finite() && p.per_share > 0.0)
        .collect();
    payments.sort_by_key(|p| p.ex_date);
    let mut out: Vec<(Payment, PaymentKind)> = Vec::with_capacity(payments.len());
    for (index, payment) in payments.iter().enumerate() {
        let from = payment
            .ex_date
            .checked_sub_days(Days::new(SPECIAL_LOOKBACK_DAYS))
            .unwrap_or(NaiveDate::MIN);
        let mut prior: Vec<f64> = out
            .iter()
            .filter(|(p, kind)| *kind == PaymentKind::Regular && p.ex_date >= from)
            .map(|(p, _)| p.per_share)
            .collect();
        let kind = if prior.len() >= SPECIAL_MIN_PRIOR {
            prior.sort_by(f64::total_cmp);
            let median = median_sorted(&prior);
            let until = payment
                .ex_date
                .checked_add_days(Days::new(SPECIAL_LOOKBACK_DAYS))
                .unwrap_or(NaiveDate::MAX);
            let new_level = payments[index + 1..]
                .iter()
                .take_while(|p| p.ex_date <= until)
                .any(|p| p.per_share >= NEW_LEVEL_SHARE * payment.per_share);
            if payment.per_share > SPECIAL_MULTIPLE * median && !new_level {
                PaymentKind::Special
            } else {
                PaymentKind::Regular
            }
        } else {
            PaymentKind::Regular
        };
        out.push((*payment, kind));
    }
    out
}

/// The special payments in `history` on or before `as_of`, oldest first.
pub fn specials(history: &[Payment], as_of: NaiveDate) -> Vec<Payment> {
    let known: Vec<Payment> = history
        .iter()
        .copied()
        .filter(|p| p.ex_date <= as_of)
        .collect();
    classify(&known)
        .into_iter()
        .filter(|(_, kind)| *kind == PaymentKind::Special)
        .map(|(p, _)| p)
        .collect()
}

fn median_sorted(values: &[f64]) -> f64 {
    let n = values.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Complete calendar years used: this many, ending the year before `as_of`.
const YEARS_USED: i32 = 6;

/// Standard deviation used when there are too few yearly changes to estimate one.
const DEFAULT_SIGMA: f64 = 0.10;

/// Smallest volatility used, so the scenarios never collapse onto each other.
const MIN_SIGMA: f64 = 0.02;

/// Growth from a payment history, with the latest data at `as_of` (only complete
/// calendar years before `as_of`'s year are used), shrunk by `n / (n + 3)`.
pub fn growth(history: &[Payment], as_of: NaiveDate) -> Growth {
    growth_with(history, as_of, DEFAULT_SHRINK_K)
}

/// Round 24, *pure*: [`growth`] with the raw growth shrunk by `n / (n + k)` (`k ≥ 0`;
/// `n` is the number of yearly changes). Payments after `as_of` and specials are ignored.
pub fn growth_with(history: &[Payment], as_of: NaiveDate, k: f64) -> Growth {
    let k = if k.is_finite() {
        k.max(0.0)
    } else {
        DEFAULT_SHRINK_K
    };
    let known: Vec<Payment> = history
        .iter()
        .copied()
        .filter(|payment| payment.per_share.is_finite() && payment.ex_date <= as_of)
        .collect();
    let payments: Vec<Payment> = classify(&known)
        .into_iter()
        .filter(|(_, kind)| *kind == PaymentKind::Regular)
        .map(|(payment, _)| payment)
        .collect();

    let ttm_start = as_of.checked_sub_days(Days::new(365));
    let ttm_per_share: f64 = payments
        .iter()
        .filter(|payment| {
            payment.ex_date <= as_of && ttm_start.is_none_or(|start| payment.ex_date > start)
        })
        .map(|payment| payment.per_share)
        .sum();

    let last_complete_year = as_of.year().saturating_sub(1);
    let first_window_year = as_of.year().saturating_sub(YEARS_USED);

    let mut by_year: BTreeMap<i32, f64> = BTreeMap::new();
    for payment in &payments {
        let year = payment.ex_date.year();
        if year >= first_window_year && year <= last_complete_year {
            *by_year.entry(year).or_insert(0.0) += payment.per_share;
        }
    }

    let first_paying_year = by_year
        .iter()
        .find(|(_, amount)| **amount > 0.0)
        .map(|(year, _)| *year);
    let annual: Vec<(i32, f64)> = match first_paying_year {
        Some(first) => (first..=last_complete_year)
            .map(|year| (year, by_year.get(&year).copied().unwrap_or(0.0)))
            .collect(),
        None => Vec::new(),
    };

    let months = months_in_last_complete_year(&payments, last_complete_year);

    let changes = annual.len().saturating_sub(1);
    let cut = annual.iter().skip(1).any(|(_, amount)| *amount <= 0.0);

    let raw = if cut || changes == 0 {
        None
    } else {
        let first = annual[0].1;
        let last = annual[annual.len() - 1].1;
        (first > 0.0 && last > 0.0).then(|| (last / first).powf(1.0 / changes as f64) - 1.0)
    };

    let g = if cut {
        -MAX_GROWTH
    } else {
        match raw {
            Some(raw) => {
                (raw * changes as f64 / (changes as f64 + k)).clamp(-MAX_GROWTH, MAX_GROWTH)
            }
            None => 0.0,
        }
    };

    // A cut makes the log change undefined, so only consecutive positive years enter the
    // volatility. With fewer than two such changes the default stands.
    let log_changes: Vec<f64> = annual
        .windows(2)
        .filter(|pair| pair[0].1 > 0.0 && pair[1].1 > 0.0)
        .map(|pair| (pair[1].1 / pair[0].1).ln())
        .collect();
    let sigma = if log_changes.len() < 2 {
        DEFAULT_SIGMA
    } else {
        sample_std(&log_changes).max(MIN_SIGMA)
    };

    Growth {
        annual,
        raw,
        g,
        sigma,
        months,
        ttm_per_share,
    }
}

/// Payment months seen in the last complete calendar year, ascending and deduplicated.
fn months_in_last_complete_year(payments: &[Payment], year: i32) -> Vec<u32> {
    let mut months: Vec<u32> = payments
        .iter()
        .filter(|payment| payment.ex_date.year() == year)
        .map(|payment| payment.ex_date.month())
        .collect();
    months.sort_unstable();
    months.dedup();
    months
}

/// Sample standard deviation with denominator `n - 1`; 0 for fewer than two values.
fn sample_std(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values
        .iter()
        .map(|value| (value - mean) * (value - mean))
        .sum::<f64>()
        / (values.len() - 1) as f64;
    variance.max(0.0).sqrt()
}
