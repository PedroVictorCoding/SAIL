//! Dividend growth estimation (Kilo's slice, round 14): synthetic histories only.

use chrono::NaiveDate;
use sail_capital::dividends::{MAX_GROWTH, Payment, growth};

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
}

fn payment(ex_date: NaiveDate, per_share: f64) -> Payment {
    Payment { ex_date, per_share }
}

/// One payment a year in `month`, amounts growing by `rate` from the first year.
fn steady_history(first_year: i32, last_year: i32, rate: f64, month: u32) -> Vec<Payment> {
    (first_year..=last_year)
        .enumerate()
        .map(|(index, year)| payment(date(year, month, 15), (1.0 + rate).powi(index as i32)))
        .collect()
}

#[test]
fn steady_growth_gives_its_cagr_and_is_shrunk() {
    let history = steady_history(2020, 2025, 0.10, 6);
    let estimate = growth(&history, date(2026, 1, 15));

    assert_eq!(estimate.annual.len(), 6);
    assert_eq!(estimate.annual.first().map(|(year, _)| *year), Some(2020));
    let raw = estimate.raw.expect("six positive years give a raw growth");
    assert!((raw - 0.10).abs() < 1e-9);
    let expected = 0.10 * 5.0 / 8.0;
    assert!((estimate.g - expected).abs() < 1e-9);
    assert!((estimate.sigma - 0.02).abs() < 1e-12);
    assert_eq!(estimate.months, vec![6]);
    assert!((estimate.ttm_per_share - 1.1f64.powi(5)).abs() < 1e-9);
}

#[test]
fn a_year_without_a_payment_after_a_paying_year_is_a_cut() {
    let history = vec![
        payment(date(2023, 6, 15), 1.0),
        payment(date(2024, 6, 15), 1.0),
    ];
    let estimate = growth(&history, date(2026, 1, 1));

    assert_eq!(estimate.annual, vec![(2023, 1.0), (2024, 1.0), (2025, 0.0)]);
    assert!(estimate.raw.is_none());
    assert!((estimate.g + MAX_GROWTH).abs() < 1e-12);
}

#[test]
fn a_single_complete_year_gives_no_growth_and_the_default_sigma() {
    let history = vec![payment(date(2025, 6, 15), 1.0)];
    let estimate = growth(&history, date(2026, 1, 1));

    assert_eq!(estimate.annual, vec![(2025, 1.0)]);
    assert!(estimate.raw.is_none());
    assert!((estimate.g).abs() < 1e-12);
    assert!((estimate.sigma - 0.10).abs() < 1e-12);
}

#[test]
fn months_and_the_trailing_year_are_measured_from_the_history() {
    let history = vec![
        payment(date(2025, 3, 15), 0.5),
        payment(date(2025, 9, 15), 0.5),
        payment(date(2026, 3, 15), 0.6),
    ];
    let estimate = growth(&history, date(2026, 6, 1));

    assert_eq!(estimate.months, vec![3, 9]);
    assert!((estimate.ttm_per_share - 1.1).abs() < 1e-12);
    assert_eq!(estimate.annual, vec![(2025, 1.0)]);
}

#[test]
fn only_the_last_six_complete_years_are_used() {
    let history = steady_history(2018, 2025, 0.0, 6);
    let estimate = growth(&history, date(2026, 1, 1));

    assert_eq!(estimate.annual.len(), 6);
    assert_eq!(estimate.annual.first().map(|(year, _)| *year), Some(2020));
    assert_eq!(estimate.annual.last().map(|(year, _)| *year), Some(2025));
}
