//! Round 24 end to end for dividend safety: a recorded-shape SEC company-facts body →
//! `companyfacts::parse` → `safety::grade` → the maximizer's safety floor. Red until
//! Antigravity's slice (G1) lands.

use chrono::NaiveDate;
use sail_capital::dividends::Payment;
use sail_capital::dividends::safety::{SafetyGrade, grade};
use sail_capital::sec::companyfacts::parse;

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("a date")
}

/// Twelve years of quarterly payments, raised every year.
fn raised_every_year() -> Vec<Payment> {
    (2013..=2025)
        .flat_map(|year| {
            [2, 5, 8, 11].map(move |month| Payment {
                ex_date: date(year, month, 10),
                per_share: 0.5 * 1.05f64.powi(year - 2013),
            })
        })
        .collect()
}

#[test]
fn a_well_covered_payer_grades_a_and_only_annual_values_count() {
    let body = include_bytes!("fixtures/r24/companyfacts_steady_payer.json");
    let facts = parse(body).expect("the fixture parses");
    assert_eq!(facts.cik, 123_456);
    assert_eq!(facts.years.len(), 2, "the Q4 value is not a fiscal year");
    let latest = facts.years.last().expect("a year");
    assert_eq!(latest.dividends_paid, Some(1_050_000_000.0));
    assert_eq!(latest.filed, date(2025, 2, 18));

    let safety = grade(Some(&facts), &raised_every_year(), date(2026, 1, 15));
    // FCF cover = (2.5 − 0.6) / 1.05 ≈ 1.81, streak ≥ 10, no cut.
    assert_eq!(safety.grade, SafetyGrade::A, "{:?}", safety.reasons);
    let cover = safety.fcf_cover.expect("a cover");
    assert!((cover - 1.9 / 1.05).abs() < 1e-9);
    assert!(safety.streak_years >= 10);
    assert!(!safety.reasons.is_empty());
}

#[test]
fn facts_filed_later_are_not_known_earlier() {
    let body = include_bytes!("fixtures/r24/companyfacts_steady_payer.json");
    let facts = parse(body).expect("the fixture parses");
    let before = facts.known_by(date(2025, 1, 1));
    assert_eq!(before.years.len(), 1);
    let safety = grade(Some(&before), &raised_every_year(), date(2025, 1, 1));
    // 2023: (2.4 − 0.6) / 1.0 = 1.8.
    assert!((safety.fcf_cover.expect("a cover") - 1.8).abs() < 1e-9);
}

#[test]
fn garbage_is_refused_without_a_panic() {
    assert!(parse(b"not json").is_err());
    assert!(parse(b"{}").is_err());
    let safety = grade(None, &[], date(2026, 1, 1));
    assert_eq!(safety.grade, SafetyGrade::Unknown);
}
