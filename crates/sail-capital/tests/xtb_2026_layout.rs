//! The layout of XTB's web-platform export as of November 2026, checked against a real
//! export (fixture values are invented; see `fixtures/xtb/make_2026_layout.py`):
//! - an account block above each table, with the account number as a number;
//! - "(UTC)" time headers, and times stored as plain spreadsheet serial numbers;
//! - "Instrument" is the security's name and "Ticker" its symbol;
//! - a "Category" column (STOCK / ETF / ETC / CFD);
//! - totals rows ("Total", "Profit/loss");
//! - open positions grouped: one instrument row, then its individual positions.
use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;
use sail_capital::import::xtb;
use sail_capital::service::CapitalService;
use sail_capital::*;
use std::str::FromStr;

const FILE: &[u8] = include_bytes!("fixtures/xtb/xtb_2026_layout.xlsx");

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn parse() -> ParsedReport {
    // The default report zone is Lisbon; the file's own "(UTC)" labels must win.
    xtb::parse(FILE, "export.xlsx", &ImportOptions::default()).unwrap()
}

#[test]
fn every_row_is_read_and_totals_rows_are_not_operations() {
    let report = parse();
    assert_eq!(report.account_label.as_deref(), Some("51234567"));
    assert_eq!(report.account_currency, Some(Currency::eur()));
    for sheet in &report.sheets {
        assert_eq!(sheet.rejected, 0, "{sheet:?}");
    }
    let rows: Vec<(SheetKind, u32)> = report.sheets.iter().map(|s| (s.kind, s.rows)).collect();
    assert_eq!(
        rows,
        vec![
            (SheetKind::ClosedPositions, 2),
            (SheetKind::CashOperations, 14),
            (SheetKind::OpenPositions, 2),
        ]
    );
    let total: Decimal = report
        .entries
        .iter()
        .filter_map(|e| e.cash_effect.as_ref())
        .map(|m| m.amount)
        .sum();
    assert_eq!(total, d("1184.7"), "the Total row itself isn't counted");
    // Only the spin-off is unclassified.
    let unknown: Vec<&ImportIssue> = report
        .issues
        .iter()
        .filter(|i| i.code == "unknown_type")
        .collect();
    assert_eq!(unknown.len(), 1);
}

#[test]
fn times_are_utc_as_labelled_and_read_from_serial_numbers() {
    let report = parse();
    let buy = &report.entries[1];
    assert_eq!(
        buy.occurred.at,
        Utc.with_ymd_and_hms(2025, 7, 1, 14, 30, 0).unwrap()
    );
    let closed = &report.snapshot.closed_positions[0];
    assert_eq!(
        closed.closed.at,
        Utc.with_ymd_and_hms(2025, 7, 2, 16, 0, 0).unwrap()
    );
    assert_eq!(
        report.snapshot.as_of,
        Some(Utc.with_ymd_and_hms(2025, 12, 31, 18, 0, 0).unwrap())
    );
}

#[test]
fn ticker_is_the_symbol_and_category_the_kind() {
    let report = parse();
    match &report.entries[1].kind {
        EntryKind::Trade {
            instrument,
            side,
            quantity,
            position_ref,
            ..
        } => {
            assert_eq!(instrument.symbol, "AAPL.US");
            assert_eq!(instrument.name.as_deref(), Some("Example Fruit Computers"));
            assert_eq!(instrument.kind, InstrumentKind::Stock);
            assert_eq!(*side, Side::Buy);
            assert_eq!(*quantity, Some(d("10")));
            assert_eq!(position_ref.as_deref(), Some("1000000001"));
        }
        other => panic!("expected a buy, got {other:?}"),
    }
    // "OPEN BUY TTE.FR 4 @ 55.00" repeats the symbol before the quantity.
    match &report.entries[2].kind {
        EntryKind::Trade { quantity, .. } => assert_eq!(*quantity, Some(d("4"))),
        other => panic!("expected a buy, got {other:?}"),
    }
    // "CLOSE BUY 5/10" sells 5 of a 10-unit position.
    match &report.entries[3].kind {
        EntryKind::Trade { side, quantity, .. } => {
            assert_eq!(*side, Side::Sell);
            assert_eq!(*quantity, Some(d("5")));
        }
        other => panic!("expected a sell, got {other:?}"),
    }
    // CFD rows stay CFDs, with the position from the comment.
    match &report.entries[6].kind {
        EntryKind::DerivativeResult {
            instrument,
            position_ref,
        } => {
            assert_eq!(instrument.kind, InstrumentKind::Cfd);
            assert_eq!(position_ref.as_deref(), Some("1000000002"));
        }
        other => panic!("expected a CFD result, got {other:?}"),
    }
    match &report.entries[8].kind {
        EntryKind::Dividend {
            instrument: Some(i),
        } => assert_eq!(i.kind, InstrumentKind::Cfd),
        other => panic!("expected a CFD dividend equivalent, got {other:?}"),
    }
    assert!(matches!(
        report.entries[9].kind,
        EntryKind::Fee {
            kind: FeeKind::Other,
            ..
        }
    ));
    assert!(matches!(report.entries[10].kind, EntryKind::Interest));
    assert!(matches!(
        report.entries[11].kind,
        EntryKind::Tax {
            kind: TaxKind::OnInterest,
            ..
        }
    ));
    assert!(matches!(report.entries[12].kind, EntryKind::Transfer));
    assert_eq!(
        report.snapshot.closed_positions[1].instrument.kind,
        InstrumentKind::Cfd
    );
}

#[test]
fn grouped_open_positions_are_counted_once_with_the_stated_value() {
    let report = parse();
    let open = &report.snapshot.open_positions;
    assert_eq!(open.len(), 2, "instrument rows only, positions folded in");
    let tte = open
        .iter()
        .find(|p| p.instrument.symbol == "TTE.FR")
        .unwrap();
    assert_eq!(tte.quantity, d("4"));
    assert_eq!(tte.instrument.name.as_deref(), Some("TotalEnergies"));
    assert_eq!(
        tte.market_value,
        Some(Money::new(d("216.0"), Currency::eur()))
    );
    assert!(report.issues.iter().any(|i| i.code == "positions_folded"));
}

#[test]
fn the_ledger_reconciles_with_the_broker() {
    let dir = tempfile::tempdir().unwrap();
    let svc = CapitalService::open(dir.path()).unwrap();
    let batch = svc
        .import_xtb(None, "export.xlsx", FILE, None, None, Utc::now())
        .unwrap();
    let p = svc.portfolio(&batch.account, AsOf::now()).unwrap();
    let held: Vec<(String, Decimal)> = p
        .positions
        .iter()
        .map(|x| (x.instrument.symbol.clone(), x.quantity))
        .collect();
    assert_eq!(
        held,
        vec![
            ("AAPL.US".to_string(), d("5")),
            ("TTE.FR".to_string(), d("4"))
        ]
    );
    assert!(
        p.reconciliation.variances.is_empty(),
        "{:?}",
        p.reconciliation.variances
    );
    assert_eq!(p.cash, vec![Money::new(d("1184.7"), Currency::eur())]);
}
