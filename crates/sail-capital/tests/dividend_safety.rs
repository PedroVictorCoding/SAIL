use chrono::NaiveDate;
use sail_capital::dividends::Payment;
use sail_capital::dividends::safety::{SafetyGrade, grade};
use sail_capital::evidence::{MAX_SEC_RESPONSE_BYTES, SourceError};
use sail_capital::sec::companyfacts::parse;

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
}

fn steady_payments(start_year: i32, end_year: i32, base: f64) -> Vec<Payment> {
    (start_year..=end_year)
        .flat_map(|year| {
            [3, 6, 9, 12].map(move |month| Payment {
                ex_date: date(year, month, 15),
                per_share: base * 1.04f64.powi(year - start_year),
            })
        })
        .collect()
}

#[test]
fn a_grade_payer_and_restatement_handling() {
    let body = include_bytes!("fixtures/r24/companyfacts_a_payer.json");
    let facts = parse(body).expect("fixture parses");
    assert_eq!(facts, parse(body).expect("pure"));
    assert_eq!(facts.cik, 320_193);
    assert_eq!(facts.years.len(), 2);

    let y2024 = &facts.years[1];
    assert_eq!(y2024.end, date(2024, 12, 31));
    assert_eq!(y2024.filed, date(2025, 3, 1));
    assert_eq!(y2024.dividends_paid, Some(1_050_000_000.0));
    assert_eq!(y2024.long_term_debt, Some(3_000_000_000.0));
    assert_eq!(y2024.cash, Some(1_200_000_000.0));

    let history = steady_payments(2014, 2025, 0.25);
    let safety = grade(Some(&facts), &history, date(2026, 2, 1));
    assert_eq!(safety, grade(Some(&facts), &history, date(2026, 2, 1)));
    assert_eq!(safety.grade, SafetyGrade::A, "{:?}", safety.reasons);
    assert!((safety.fcf_cover.expect("fcf") - 2.0).abs() < 1e-9);
    assert!((safety.earnings_cover.expect("eps") - 2.0).abs() < 1e-9);
    assert_eq!(safety.streak_years, 12);
    assert!(safety.reasons.iter().any(|r| r.contains("2.0×")));
    assert!(safety.reasons.iter().any(|r| r.contains("12 years")));
}

#[test]
fn two_years_badly_uncovered_grades_e() {
    let body = include_bytes!("fixtures/r24/companyfacts_cutter.json");
    let facts = parse(body).expect("fixture parses");
    assert_eq!(facts.years.len(), 2);

    let history = steady_payments(2020, 2025, 0.50);
    let safety = grade(Some(&facts), &history, date(2026, 1, 15));
    assert_eq!(safety.grade, SafetyGrade::E, "{:?}", safety.reasons);
    assert!((safety.fcf_cover.expect("fcf") - 0.55).abs() < 1e-9);
    assert!(safety.reasons.iter().any(|r| r.contains("below 0.7×")));
}

#[test]
fn recent_cut_grades_d_even_with_high_cover() {
    let body = include_bytes!("fixtures/r24/companyfacts_a_payer.json");
    let facts = parse(body).expect("fixture parses");

    let mut history = steady_payments(2015, 2023, 1.0);
    // Cut 2024 dividend by 50%.
    history.push(Payment {
        ex_date: date(2024, 6, 15),
        per_share: 1.5,
    });
    history.push(Payment {
        ex_date: date(2025, 6, 15),
        per_share: 1.5,
    });

    let safety = grade(Some(&facts), &history, date(2026, 1, 15));
    assert_eq!(safety.grade, SafetyGrade::D, "{:?}", safety.reasons);
    assert!(safety.reasons.iter().any(|r| r.contains("cut")));
}

#[test]
fn missing_cash_flow_grades_unknown() {
    let body = include_bytes!("fixtures/r24/companyfacts_missing_cf.json");
    let facts = parse(body).expect("fixture parses");
    let history = steady_payments(2020, 2025, 0.25);
    let safety = grade(Some(&facts), &history, date(2026, 1, 15));
    assert_eq!(safety.grade, SafetyGrade::Unknown);
    assert!(safety.fcf_cover.is_none());
    assert!(!safety.reasons.is_empty());
}

#[test]
fn b_and_c_grades_and_point_in_time() {
    let body = include_bytes!("fixtures/r24/companyfacts_a_payer.json");
    let mut facts = parse(body).expect("fixture parses");

    // Known before 2025-02-14 -> only 2023 fiscal year (FCF cover = (2.5 - 0.5) / 1.0 = 2.0).
    let before = facts.known_by(date(2025, 1, 1));
    assert_eq!(before.years.len(), 1);
    assert_eq!(before.years[0].end, date(2023, 12, 31));

    // Short streak (< 10) with FCF cover 2.0 -> Grade B.
    let short_history = steady_payments(2021, 2025, 0.25);
    let safety_b = grade(Some(&facts), &short_history, date(2026, 1, 15));
    assert_eq!(safety_b.grade, SafetyGrade::B);

    // Adjust 2024 operating cash flow so FCF cover = 1.1 -> Grade C.
    facts.years[1].operating_cash_flow = Some(1_855_000_000.0); // (1.855 - 0.7) / 1.05 = 1.1
    let safety_c = grade(Some(&facts), &short_history, date(2026, 1, 15));
    assert_eq!(safety_c.grade, SafetyGrade::C);
}

#[test]
fn hostile_inputs_are_rejected_without_panicking() {
    let oversized = vec![b' '; MAX_SEC_RESPONSE_BYTES + 1];
    assert!(matches!(parse(&oversized), Err(SourceError::TooLarge)));
    assert!(matches!(parse(b"not json"), Err(SourceError::Malformed(_))));
    assert!(matches!(parse(b"{}"), Err(SourceError::Malformed(_))));
    assert!(matches!(parse(b"[]"), Err(SourceError::Malformed(_))));

    // Missing us-gaap returns empty years, not an error.
    let empty_facts = parse(br#"{"cik": 1, "facts": {}}"#).expect("empty facts ok");
    assert_eq!(empty_facts.cik, 1);
    assert!(empty_facts.years.is_empty());
}
