use std::collections::BTreeMap;

use chrono::NaiveDate;
use sail_capital::dividends::calendar::{Confidence, DEFAULT_DAYS, upcoming};
use sail_capital::dividends::{Holding, Payment};

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).expect("valid date")
}

fn quarterly_holding(symbol: &str, last_year: i32) -> Holding {
    let mut history = Vec::new();
    for year in 2023..=last_year {
        for month in [2u32, 5, 8, 11] {
            history.push(Payment {
                ex_date: d(year, month, 10),
                per_share: 0.50,
            });
        }
    }
    Holding {
        key: symbol.to_string(),
        symbol: symbol.to_string(),
        units: 50.0,
        price: 100.0,
        fx: 1.0,
        history,
        withholding: 0.15,
        withholding_source: "observed".into(),
        pt_rate: 0.28,
        price_growth: None,
    }
}

#[test]
fn special_dividends_are_excluded_and_default_pay_lag_is_21_days() {
    let mut h = quarterly_holding("DIVA", 2025);
    h.history.push(Payment {
        ex_date: d(2026, 2, 10),
        per_share: 0.50,
    });
    h.history.push(Payment {
        ex_date: d(2026, 5, 10),
        per_share: 0.50,
    });
    // Special dividend (5× normal) must not set per_share or schedule.
    h.history.push(Payment {
        ex_date: d(2026, 6, 15),
        per_share: 2.50,
    });
    h.history.push(Payment {
        ex_date: d(2026, 8, 10),
        per_share: 0.50,
    });

    let today = d(2026, 9, 1);
    let list = upcoming(&[h.clone()], &BTreeMap::new(), today, DEFAULT_DAYS);
    assert_eq!(list, upcoming(&[h], &BTreeMap::new(), today, DEFAULT_DAYS));
    assert_eq!(list.len(), 1);
    assert!((list[0].per_share - 0.50).abs() < 1e-9);
    assert!(list[0].pay_date_estimated);
    assert_eq!((list[0].pay_date - list[0].ex_date).num_days(), 21);
    assert!(list[0].basis.starts_with("Quarterly;"));
}

#[test]
fn medium_confidence_for_short_history_and_sorted_by_pay_date_then_symbol() {
    let mut short_h = quarterly_holding("BBB", 2026);
    short_h
        .history
        .retain(|p| p.ex_date >= d(2025, 11, 10) && p.ex_date <= d(2026, 8, 10));

    let mut long_h = quarterly_holding("AAA", 2026);
    long_h.history.retain(|p| p.ex_date <= d(2026, 8, 10));

    let today = d(2026, 9, 1);
    let list = upcoming(&[short_h, long_h], &BTreeMap::new(), today, 90);
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].symbol, "AAA");
    assert_eq!(list[0].confidence, Confidence::High);
    assert_eq!(list[1].symbol, "BBB");
    assert_eq!(list[1].confidence, Confidence::Medium);
}
