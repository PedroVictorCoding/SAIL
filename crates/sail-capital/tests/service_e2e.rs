//! End to end through the service: import the official-format fixtures, read the
//! portfolio, re-import, import a longer export, and check point-in-time visibility and
//! privacy. Everything here is synthetic data.
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use sail_capital::AsOf;
use sail_capital::ledger::ReconciliationStatus;
use sail_capital::service::CapitalService;
use std::str::FromStr;

const CASH: &[u8] = include_bytes!("fixtures/xtb/english_cash.csv");
const OPEN: &[u8] = include_bytes!("fixtures/xtb/open_positions.csv");

fn t(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, day, 9, 0, 0).unwrap()
}

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn eur_cash(p: &sail_capital::ledger::Projection) -> Decimal {
    p.cash
        .iter()
        .find(|m| m.currency.as_str() == "EUR")
        .map(|m| m.amount)
        .unwrap()
}

#[test]
fn import_reimport_extend_and_replay() {
    let dir = tempfile::tempdir().unwrap();
    let svc = CapitalService::open(dir.path()).unwrap();

    // 1. First import.
    let first = svc
        .import_xtb(None, "cash.csv", CASH, None, None, t(10))
        .unwrap();
    assert_eq!(first.account.as_str(), "xtb:…4567");
    assert_eq!((first.added, first.already_imported), (13, false));
    let account = svc.resolve_account(None).unwrap();

    let now = AsOf {
        occurred_by: t(20),
        known_by: t(20),
    };
    let p = svc.portfolio(&account, now).unwrap();
    // 5000 - 500 - 100 - 1500 + 900 + 25.5 - 3.83 + 4.2 - 1.18 - 0.05 - 1.5 - 0.75 + 45
    assert_eq!(eur_cash(&p), d("3867.39"));
    assert_eq!(p.positions.len(), 1);
    assert_eq!(p.positions[0].quantity, d("5"));
    assert_eq!(p.positions[0].cost_basis.as_ref().unwrap().amount, d("750"));
    assert_eq!(p.dividends.len(), 1);
    assert_eq!(p.dividends[0].net.as_ref().unwrap().amount, d("21.67"));
    assert_eq!(p.realized[0].result.as_ref().unwrap().amount, d("150"));
    assert_eq!(p.reconciliation.status, ReconciliationStatus::Unverified);

    // 2. The same bytes again add nothing.
    let again = svc
        .import_xtb(None, "cash.csv", CASH, None, None, t(11))
        .unwrap();
    assert_eq!((again.added, again.already_imported), (0, true));

    // 3. A longer export: the same rows plus one new deposit.
    let mut longer = CASH.to_vec();
    if !longer.ends_with(b"\n") {
        longer.push(b'\n');
    }
    longer.extend_from_slice(b"114;Deposit;11.01.2025 09:00:00;;;250.0\n");
    let extended = svc
        .import_xtb(None, "cash-longer.csv", &longer, None, None, t(12))
        .unwrap();
    assert_eq!(
        (extended.added, extended.duplicates, extended.conflicts),
        (1, 13, 0)
    );
    let p = svc.portfolio(&account, now).unwrap();
    assert_eq!(eur_cash(&p), d("4117.39"));

    // 4. As known before the longer export, the new deposit is invisible.
    let before = AsOf {
        occurred_by: t(20),
        known_by: t(11),
    };
    assert_eq!(
        eur_cash(&svc.portfolio(&account, before).unwrap()),
        d("3867.39")
    );

    // 5. The broker's open-positions report is evidence to reconcile against.
    let snap = svc
        .import_xtb(None, "open.csv", OPEN, None, None, t(13))
        .unwrap();
    assert_eq!(snap.added, 0);
    let p = svc.portfolio(&account, now).unwrap();
    assert_eq!(p.reconciliation.status, ReconciliationStatus::Variance);
    assert_eq!(
        p.positions[0].broker_market_value.as_ref().unwrap().amount,
        d("1800")
    );
    assert!(
        p.reconciliation
            .variances
            .iter()
            .any(|v| v.subject == "quantity sym:AAPL.US")
    );
    assert_eq!(svc.batches(Some(&account)).unwrap().len(), 3);
}

#[test]
fn the_owner_name_and_full_account_number_are_never_stored() {
    let dir = tempfile::tempdir().unwrap();
    let svc = CapitalService::open(dir.path()).unwrap();
    svc.import_xtb(None, "cash.csv", CASH, None, None, t(10))
        .unwrap();
    let db = std::fs::read(dir.path().join("capital.sqlite3")).unwrap();
    let wal = std::fs::read(dir.path().join("capital.sqlite3-wal")).unwrap_or_default();
    for bytes in [&db, &wal] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("Confidential Owner Name"));
        assert!(!text.contains("51234567"));
    }
}

#[test]
fn bad_uploads_are_refused_with_owner_safe_errors() {
    let dir = tempfile::tempdir().unwrap();
    let svc = CapitalService::open(dir.path()).unwrap();
    assert!(
        svc.import_xtb(None, "cash.pdf", CASH, None, None, t(10))
            .is_err()
    );
    assert!(
        svc.import_xtb(None, "cash.csv", b"", None, None, t(10))
            .is_err()
    );
    assert!(
        svc.import_xtb(None, "cash.csv", CASH, Some("Mars/Base"), None, t(10))
            .is_err()
    );
    assert!(
        svc.import_xtb(None, "cash.csv", CASH, None, Some("EURO"), t(10))
            .is_err()
    );
    assert!(svc.resolve_account(None).is_err(), "nothing was imported");
}

#[test]
fn a_file_without_an_account_joins_the_selected_one_and_a_named_file_wins() {
    let dir = tempfile::tempdir().unwrap();
    let svc = CapitalService::open(dir.path()).unwrap();
    let first = svc
        .import_xtb(None, "cash.csv", CASH, None, None, t(10))
        .unwrap();
    // A closed-positions export with no preamble, uploaded while that account is selected.
    let closed = include_bytes!("fixtures/xtb/closed_positions.csv");
    let s = svc
        .import_xtb(
            Some(first.account.as_str()),
            "closed.csv",
            closed,
            None,
            Some("EUR"),
            t(11),
        )
        .unwrap();
    assert_eq!(s.account, first.account);
    // The closed-positions export says nothing about current holdings, so the earlier
    // open-positions snapshot stays the one reconciled against.
    svc.import_xtb(None, "open.csv", OPEN, None, None, t(10))
        .unwrap();
    let svc_closed_last = svc
        .import_xtb(
            Some(first.account.as_str()),
            "closed-again.csv",
            &[closed.as_slice(), b"\n"].concat(),
            None,
            Some("EUR"),
            t(13),
        )
        .unwrap();
    assert_eq!(svc_closed_last.account, first.account);
    let p = svc
        .portfolio(
            &first.account,
            AsOf {
                occurred_by: t(20),
                known_by: t(20),
            },
        )
        .unwrap();
    assert!(p.positions[0].broker_market_value.is_some());
    assert!(
        p.reconciliation
            .variances
            .iter()
            .any(|v| v.subject == "quantity sym:US500#cfd"),
        "still reconciled against the open-positions snapshot"
    );
    // A file naming its own account is never re-booked by the selection.
    let other = svc
        .import_xtb(Some("xtb:…9999"), "cash2.csv", OPEN, None, None, t(12))
        .unwrap();
    assert_eq!(other.account, first.account);
    assert_eq!(svc.accounts().unwrap().len(), 1);
}
