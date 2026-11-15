//! Round 25 B8: the projection record's grading, its unit adjustment and its store.

use chrono::{NaiveDate, TimeZone, Utc};
use sail_capital::contracts::AccountId;
use sail_capital::dividends::record::{IncomeSnapshot, Receipt, SnapshotHolding, grade, snapshot};
use sail_capital::store::CapitalStore;

fn d(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("a date")
}

fn made() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2025, 1, 2, 9, 0, 0).unwrap()
}

fn one_holding(projected: f64) -> Vec<SnapshotHolding> {
    vec![SnapshotHolding {
        key: "A".into(),
        symbol: "A.US".into(),
        units: 10.0,
        projected_net: projected,
    }]
}

fn first_snapshot() -> IncomeSnapshot {
    snapshot(
        d(2025, 1, 1),
        made(),
        "EUR",
        one_holding(100.0),
        80.0,
        100.0,
        120.0,
        Some((85.0, 115.0)),
    )
}

#[test]
fn a_snapshot_is_pending_until_its_twelve_months_end() {
    let snap = first_snapshot();
    let early = grade(std::slice::from_ref(&snap), &[], d(2025, 12, 31));
    assert_eq!(early.pending, 1);
    assert!(early.graded.is_empty());
    assert!(early.bias.is_none());
    let on_time = grade(std::slice::from_ref(&snap), &[], d(2026, 1, 1));
    assert_eq!(on_time.pending, 0);
    assert_eq!(on_time.graded.len(), 1);
}

#[test]
fn receipts_are_unit_adjusted_and_windowed() {
    let snap = first_snapshot();
    let receipts = vec![
        Receipt {
            key: "A".into(),
            paid: d(2025, 3, 10),
            net: 30.0,
            units_then: 10.0,
        },
        Receipt {
            key: "A".into(),
            paid: d(2025, 6, 10),
            net: 30.0,
            units_then: 10.0,
        },
        // Twice the units when paid: only half counts.
        Receipt {
            key: "A".into(),
            paid: d(2025, 9, 10),
            net: 60.0,
            units_then: 20.0,
        },
        Receipt {
            key: "A".into(),
            paid: d(2025, 12, 10),
            net: 30.0,
            units_then: 10.0,
        },
        // After the window.
        Receipt {
            key: "A".into(),
            paid: d(2026, 1, 5),
            net: 30.0,
            units_then: 10.0,
        },
        // Unknown holding.
        Receipt {
            key: "B".into(),
            paid: d(2025, 5, 1),
            net: 999.0,
            units_then: 1.0,
        },
        // No units: skipped.
        Receipt {
            key: "A".into(),
            paid: d(2025, 5, 2),
            net: 10.0,
            units_then: 0.0,
        },
        // Non-finite: skipped.
        Receipt {
            key: "A".into(),
            paid: d(2025, 5, 3),
            net: f64::NAN,
            units_then: 10.0,
        },
    ];
    let summary = grade(&[snap], &receipts, d(2026, 2, 1));
    assert_eq!(summary.pending, 0);
    assert_eq!(summary.graded.len(), 1);
    let row = &summary.graded[0];
    assert!((row.received - 120.0).abs() < 1e-9);
    assert!((row.error.expect("error") - 0.2).abs() < 1e-9);
    assert!(row.in_scenarios);
    assert_eq!(row.in_band, Some(false));
    assert!((row.holdings[0].1 - 100.0).abs() < 1e-9);
    assert!((row.holdings[0].2 - 120.0).abs() < 1e-9);
    assert!((row.holdings[0].3.expect("holding error") - 0.2).abs() < 1e-9);
    assert!((summary.bias.expect("bias") - 0.2).abs() < 1e-9);
    assert!((summary.mean_abs_error.expect("mae") - 0.2).abs() < 1e-9);
    assert_eq!(summary.scenario_coverage, Some(1.0));
    assert_eq!(summary.band_coverage, Some(0.0));
    assert_eq!(
        summary,
        grade(&[first_snapshot()], &receipts, d(2026, 2, 1))
    );
}

#[test]
fn coverage_is_the_share_of_snapshots_inside_each_band() {
    let first = first_snapshot();
    let second = snapshot(
        d(2025, 2, 1),
        made(),
        "EUR",
        vec![SnapshotHolding {
            key: "B".into(),
            symbol: "B.US".into(),
            units: 10.0,
            projected_net: 100.0,
        }],
        80.0,
        100.0,
        120.0,
        Some((85.0, 115.0)),
    );
    let receipts = vec![
        Receipt {
            key: "A".into(),
            paid: d(2025, 9, 1),
            net: 120.0,
            units_then: 10.0,
        },
        Receipt {
            key: "B".into(),
            paid: d(2025, 10, 1),
            net: 100.0,
            units_then: 10.0,
        },
    ];
    let summary = grade(&[first, second], &receipts, d(2026, 3, 1));
    assert_eq!(summary.graded.len(), 2);
    assert_eq!(summary.scenario_coverage, Some(1.0));
    assert_eq!(summary.band_coverage, Some(0.5));
    assert!((summary.bias.expect("bias") - 0.1).abs() < 1e-9);
    assert!((summary.mean_abs_error.expect("mae") - 0.1).abs() < 1e-9);
}

#[test]
fn a_later_receipt_changes_no_earlier_grade() {
    let snap = first_snapshot();
    let early = grade(
        std::slice::from_ref(&snap),
        &[Receipt {
            key: "A".into(),
            paid: d(2025, 3, 1),
            net: 50.0,
            units_then: 10.0,
        }],
        d(2026, 2, 1),
    );
    let later = grade(
        std::slice::from_ref(&snap),
        &[
            Receipt {
                key: "A".into(),
                paid: d(2025, 3, 1),
                net: 50.0,
                units_then: 10.0,
            },
            Receipt {
                key: "A".into(),
                paid: d(2027, 3, 1),
                net: 500.0,
                units_then: 10.0,
            },
        ],
        d(2026, 2, 1),
    );
    assert_eq!(early, later);
}

#[test]
fn the_store_keeps_one_snapshot_per_month() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = CapitalStore::open(dir.path()).expect("a store");
    let account = AccountId::from_stored("xtb:test");
    let snap = first_snapshot();
    assert!(
        store
            .record_income_snapshot(&account, &snap)
            .expect("record")
    );
    assert!(
        !store
            .record_income_snapshot(&account, &snap)
            .expect("record again")
    );
    assert!(
        store
            .has_income_snapshot(&account, d(2025, 1, 1))
            .expect("has")
    );
    assert!(
        !store
            .has_income_snapshot(&account, d(2025, 2, 1))
            .expect("missing")
    );
    let all = store
        .income_snapshots(&account, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
        .expect("list");
    assert_eq!(all, vec![snap]);
}
