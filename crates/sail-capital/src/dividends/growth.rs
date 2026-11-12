//! Growth from a per-share payment history: annual sums, CAGR, volatility and the
//! payment schedule used by the projection.

use std::collections::BTreeMap;

use chrono::{Datelike, Days, NaiveDate};

use super::{Growth, MAX_GROWTH, Payment};

/// Complete calendar years used: this many, ending the year before `as_of`.
const YEARS_USED: i32 = 6;

/// Standard deviation used when there are too few yearly changes to estimate one.
const DEFAULT_SIGMA: f64 = 0.10;

/// Smallest volatility used, so the scenarios never collapse onto each other.
const MIN_SIGMA: f64 = 0.02;

/// Growth from a payment history, with the latest data at `as_of` (only complete
/// calendar years before `as_of`'s year are used).
pub fn growth(history: &[Payment], as_of: NaiveDate) -> Growth {
    let mut payments: Vec<Payment> = history
        .iter()
        .copied()
        .filter(|payment| payment.per_share.is_finite())
        .collect();
    payments.sort_by_key(|payment| payment.ex_date);

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
                (raw * changes as f64 / (changes as f64 + 3.0)).clamp(-MAX_GROWTH, MAX_GROWTH)
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
