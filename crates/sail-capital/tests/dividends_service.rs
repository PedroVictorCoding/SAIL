//! Round 14 (Claude): dividend projection inputs and validation. Engine tests are Kilo's.
use chrono::{NaiveDate, Utc};
use sail_capital::dividends::{Allocation, Payment};
use sail_capital::service::{CapitalService, ServiceError};

const FILE: &[u8] = include_bytes!("fixtures/xtb/xtb_2026_layout.xlsx");

#[test]
fn out_of_range_settings_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let svc = CapitalService::open(dir.path()).unwrap();
    let account = svc
        .import_xtb(None, "export.xlsx", FILE, None, None, Utc::now())
        .unwrap()
        .account;
    for (years, pct) in [
        (0, 50.0),
        (31, 50.0),
        (10, -1.0),
        (10, 101.0),
        (10, f64::NAN),
    ] {
        assert!(
            matches!(
                svc.dividend_projection(&account, years, pct, Allocation::Proportional, false),
                Err(ServiceError::BadRequest(_))
            ),
            "{years} {pct}"
        );
    }
    for top_n in [0, 11] {
        assert!(
            matches!(
                svc.dividend_projection_with(
                    &account,
                    5,
                    100.0,
                    Allocation::TopValue,
                    top_n,
                    false,
                ),
                Err(ServiceError::BadRequest(_))
            ),
            "top_n {top_n}"
        );
    }
    let top = svc
        .dividend_projection_with(&account, 5, 100.0, Allocation::TopValue, 3, false)
        .unwrap();
    assert_eq!(top.settings.top_n, 3);

    // Without stored prices the holdings are left out, and the notes say so.
    let outlook = svc
        .dividend_projection(&account, 5, 100.0, Allocation::SameHolding, true)
        .unwrap();
    assert!(outlook.notes.iter().any(|n| n.contains("no stored price")));
    assert_eq!(outlook.settings.years, 5);
}

#[test]
fn histories_are_stored_append_only_and_the_latest_wins() {
    let dir = tempfile::tempdir().unwrap();
    let store = sail_capital::store::CapitalStore::open(dir.path()).unwrap();
    let p = |d: u32, v: f64| Payment {
        ex_date: NaiveDate::from_ymd_opt(2026, d, 15).unwrap(),
        per_share: v,
    };
    store
        .record_dividend_history("KO", Some("USD"), &[p(3, 0.5)], Utc::now())
        .unwrap();
    store
        .record_dividend_history("KO", Some("USD"), &[p(3, 0.5), p(6, 0.51)], Utc::now())
        .unwrap();
    let (_, currency, history) = store.dividend_history("KO").unwrap().unwrap();
    assert_eq!(currency.as_deref(), Some("USD"));
    assert_eq!(history.len(), 2);
    assert!(store.dividend_history("NONE").unwrap().is_none());
}
