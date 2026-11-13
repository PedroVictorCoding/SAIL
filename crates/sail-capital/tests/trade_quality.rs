//! Round 19: one-day jumps that look like unadjusted splits keep bars out of the evidence.

use chrono::{Days, NaiveDate};
use sail_capital::trade::Bar;
use sail_capital::trade::quality::{gaps, usable};

fn bars(closes: &[f64]) -> Vec<Bar> {
    let start = NaiveDate::from_ymd_opt(2026, 1, 5).expect("a date");
    closes
        .iter()
        .enumerate()
        .map(|(index, close)| Bar {
            session: start + Days::new(index as u64),
            open: *close,
            high: close * 1.01,
            low: close * 0.99,
            close: *close,
        })
        .collect()
}

#[test]
fn ordinary_moves_are_not_flagged() {
    let series = bars(&[100.0, 103.0, 98.0, 120.0, 90.0]);
    // 120/98 = 1.22 and 120/90 = 1.33 both stay under 1.4.
    assert!(gaps(&series).is_empty());
    assert!(usable(&series));
}

#[test]
fn an_unadjusted_four_for_one_split_is_named_and_unusable() {
    let series = bars(&[400.0, 404.0, 101.5, 102.0]);
    let found = gaps(&series);
    assert_eq!(found.len(), 1);
    let gap = &found[0];
    assert_eq!(gap.session, series[2].session);
    assert!(gap.down);
    // 404 / 101.5 = 3.980, within 3% of 4.
    assert!((gap.ratio - 404.0 / 101.5).abs() < 1e-12);
    assert_eq!(gap.likely_split, Some((4, 1)));
    assert!(!usable(&series));
}

#[test]
fn a_reverse_split_and_a_three_for_two_are_named() {
    let reverse = gaps(&bars(&[2.0, 2.02, 20.1]));
    // 2.02 / 20.1 = 0.1005: a 1-for-10 reverse split.
    assert_eq!(reverse[0].likely_split, Some((1, 10)));
    assert!(!reverse[0].down);
    let three_for_two = gaps(&bars(&[150.0, 99.5]));
    // 150 / 99.5 = 1.5075, within 3% of 1.5.
    assert_eq!(three_for_two[0].likely_split, Some((3, 2)));
}

#[test]
fn a_crash_that_is_not_a_split_ratio_is_flagged_but_usable() {
    // 100 -> 38 is a ratio of 2.63: far from 2 and 3.
    let series = bars(&[100.0, 38.0, 39.0]);
    let found = gaps(&series);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].likely_split, None);
    assert!(usable(&series));
}

#[test]
fn degenerate_bars_never_panic() {
    assert!(gaps(&[]).is_empty());
    assert!(gaps(&bars(&[100.0])).is_empty());
    let mut odd = bars(&[100.0, 0.0, f64::NAN, 50.0, f64::INFINITY]);
    odd[1].open = -1.0;
    let _ = gaps(&odd);
    assert!(usable(&bars(&[])));
}
