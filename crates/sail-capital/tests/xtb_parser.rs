//! Comprehensive integration test suite for the XTB importer.
use chrono::{Datelike, TimeZone};
use rust_decimal::Decimal;
use sail_capital::Currency;
use sail_capital::contracts::{
    AccountId, Broker, EntryKind, FeeKind, ImportError, ImportOptions, InstrumentKind,
    MAX_CELL_CHARS, MAX_REPORT_BYTES, Severity, Side, TaxKind,
};
use sail_capital::import::xtb::parse;
use std::str::FromStr;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

#[test]
fn test_english_cash_xlsx_and_csv() {
    let xlsx_bytes = include_bytes!("fixtures/xtb/english_cash.xlsx");
    let csv_bytes = include_bytes!("fixtures/xtb/english_cash.csv");
    let options = ImportOptions::default();

    for (bytes, name) in [
        (xlsx_bytes.as_slice(), "english_cash.xlsx"),
        (csv_bytes.as_slice(), "english_cash.csv"),
    ] {
        let report = parse(bytes, name, &options).expect("should parse english cash report");

        assert_eq!(report.broker, Broker::Xtb);
        assert_eq!(report.account_label.as_deref(), Some("51234567"));
        let masked = AccountId::new(report.broker, report.account_label.as_deref().unwrap());
        assert_eq!(masked.as_str(), "xtb:…4567");
        assert_eq!(
            report.account_currency.as_ref().map(|c| c.as_str()),
            Some("EUR")
        );

        // Verify all 13 known cash types are present and correctly typed
        assert_eq!(report.entries.len(), 13);

        // 1. Deposit
        assert_eq!(report.entries[0].kind, EntryKind::Deposit);
        assert_eq!(
            report.entries[0].cash_effect.as_ref().unwrap().amount,
            d("5000.00")
        );

        // 2. Withdrawal
        assert_eq!(report.entries[1].kind, EntryKind::Withdrawal);
        assert_eq!(
            report.entries[1].cash_effect.as_ref().unwrap().amount,
            d("-500.00")
        );

        // 3. Sub account transfer
        assert_eq!(report.entries[2].kind, EntryKind::Transfer);
        assert_eq!(
            report.entries[2].cash_effect.as_ref().unwrap().amount,
            d("-100.00")
        );

        // 4. Trade Buy
        match &report.entries[3].kind {
            EntryKind::Trade {
                instrument,
                side,
                quantity,
                price,
                position_ref,
            } => {
                assert_eq!(instrument.symbol, "AAPL.US");
                assert_eq!(instrument.kind, InstrumentKind::Stock);
                assert_eq!(*side, Side::Buy);
                assert_eq!(*quantity, Some(d("10")));
                // The comment price is in USD but the row only states EUR cash, so it
                // is dropped rather than mislabelled; the quantity is still exact.
                assert_eq!(*price, None);
                assert!(position_ref.is_some());
            }
            other => panic!("expected Trade Buy, got {other:?}"),
        }

        // 5. Trade Sell
        match &report.entries[4].kind {
            EntryKind::Trade {
                instrument,
                side,
                quantity,
                price,
                ..
            } => {
                assert_eq!(instrument.symbol, "AAPL.US");
                assert_eq!(*side, Side::Sell);
                assert_eq!(*quantity, Some(d("5")));
                assert_eq!(*price, None);
            }
            other => panic!("expected Trade Sell, got {other:?}"),
        }

        // 6. Dividend
        match &report.entries[5].kind {
            EntryKind::Dividend { instrument } => {
                assert_eq!(instrument.as_ref().unwrap().symbol, "MSFT.US");
            }
            other => panic!("expected Dividend, got {other:?}"),
        }
        assert_eq!(
            report.entries[5].cash_effect.as_ref().unwrap().amount,
            d("25.50")
        );

        // 7. Withholding Tax
        match &report.entries[6].kind {
            EntryKind::Tax { kind, instrument } => {
                assert_eq!(*kind, TaxKind::Withholding);
                assert_eq!(instrument.as_ref().unwrap().symbol, "MSFT.US");
            }
            other => panic!("expected Withholding Tax, got {other:?}"),
        }
        assert_eq!(
            report.entries[6].cash_effect.as_ref().unwrap().amount,
            d("-3.83")
        );

        // 8. Interest
        assert_eq!(report.entries[7].kind, EntryKind::Interest);
        assert_eq!(
            report.entries[7].cash_effect.as_ref().unwrap().amount,
            d("4.20")
        );

        // 9. Interest Tax
        assert_eq!(
            report.entries[8].kind,
            EntryKind::Tax {
                kind: TaxKind::OnInterest,
                instrument: None
            }
        );
        assert_eq!(
            report.entries[8].cash_effect.as_ref().unwrap().amount,
            d("-1.18")
        );

        // 10. Regulatory fee (SEC)
        assert_eq!(
            report.entries[9].kind,
            EntryKind::Fee {
                kind: FeeKind::Regulatory,
                instrument: report.entries[9].kind.instrument().cloned(),
            }
        );

        // 11. Commission
        assert_eq!(
            report.entries[10].kind,
            EntryKind::Fee {
                kind: FeeKind::Commission,
                instrument: report.entries[10].kind.instrument().cloned(),
            }
        );

        // 12. Swap (CFD)
        assert_eq!(
            report.entries[11].kind,
            EntryKind::Fee {
                kind: FeeKind::Swap,
                instrument: report.entries[11].kind.instrument().cloned(),
            }
        );

        // 13. Derivative Result (close trade)
        match &report.entries[12].kind {
            EntryKind::DerivativeResult { instrument, .. } => {
                assert_eq!(instrument.symbol, "US500");
                assert_eq!(instrument.kind, InstrumentKind::Cfd);
            }
            other => panic!("expected DerivativeResult, got {other:?}"),
        }

        // Verify natural keys are prefixed with xtb:cash:
        for entry in &report.entries {
            assert!(entry.natural_key.starts_with("xtb:cash:"));
        }
    }
}

trait EntryKindExt {
    fn instrument(&self) -> Option<&sail_capital::contracts::Instrument>;
}

impl EntryKindExt for EntryKind {
    fn instrument(&self) -> Option<&sail_capital::contracts::Instrument> {
        match self {
            EntryKind::Trade { instrument, .. } => Some(instrument),
            EntryKind::Dividend { instrument } => instrument.as_ref(),
            EntryKind::Tax { instrument, .. } => instrument.as_ref(),
            EntryKind::Fee { instrument, .. } => instrument.as_ref(),
            EntryKind::DerivativeResult { instrument, .. } => Some(instrument),
            _ => None,
        }
    }
}

#[test]
fn test_portuguese_cash_xlsx_and_csv() {
    let xlsx_bytes = include_bytes!("fixtures/xtb/portuguese_cash.xlsx");
    let csv_bytes = include_bytes!("fixtures/xtb/portuguese_cash.csv");
    let options = ImportOptions::default();

    for (bytes, name) in [
        (xlsx_bytes.as_slice(), "portuguese_cash.xlsx"),
        (csv_bytes.as_slice(), "portuguese_cash.csv"),
    ] {
        let report = parse(bytes, name, &options).expect("should parse portuguese cash report");

        assert_eq!(report.account_label.as_deref(), Some("98765432"));
        assert_eq!(
            report.account_currency.as_ref().map(|c| c.as_str()),
            Some("EUR")
        );

        // Check entries
        assert_eq!(report.entries.len(), 13);
        assert_eq!(report.entries[0].kind, EntryKind::Deposit);
        assert_eq!(report.entries[1].kind, EntryKind::Withdrawal);

        match &report.entries[2].kind {
            EntryKind::Trade {
                side,
                quantity,
                price,
                instrument,
                ..
            } => {
                assert_eq!(*side, Side::Buy);
                assert_eq!(*quantity, Some(d("2")));
                assert_eq!(*price, None);
                assert_eq!(instrument.symbol, "VWCE.DE");
            }
            other => panic!("expected Buy, got {other:?}"),
        }

        match &report.entries[3].kind {
            EntryKind::Trade {
                side,
                quantity,
                price,
                instrument,
                ..
            } => {
                assert_eq!(*side, Side::Sell);
                assert_eq!(*quantity, Some(d("1")));
                assert_eq!(*price, None);
                assert_eq!(instrument.symbol, "VWCE.DE");
            }
            other => panic!("expected Sell, got {other:?}"),
        }

        assert!(matches!(report.entries[4].kind, EntryKind::Dividend { .. }));
        assert!(matches!(
            report.entries[5].kind,
            EntryKind::Tax {
                kind: TaxKind::Withholding,
                ..
            }
        ));
        assert_eq!(report.entries[6].kind, EntryKind::Interest);
        assert_eq!(
            report.entries[7].kind,
            EntryKind::Tax {
                kind: TaxKind::OnInterest,
                instrument: None
            }
        );
        assert_eq!(
            report.entries[8].kind,
            EntryKind::Fee {
                kind: FeeKind::Commission,
                instrument: report.entries[8].kind.instrument().cloned()
            }
        );
        assert_eq!(
            report.entries[9].kind,
            EntryKind::Fee {
                kind: FeeKind::Regulatory,
                instrument: report.entries[9].kind.instrument().cloned()
            }
        );
        assert_eq!(
            report.entries[10].kind,
            EntryKind::Fee {
                kind: FeeKind::Swap,
                instrument: report.entries[10].kind.instrument().cloned()
            }
        );
        assert!(matches!(
            report.entries[11].kind,
            EntryKind::DerivativeResult { .. }
        ));
        assert_eq!(report.entries[12].kind, EntryKind::Transfer);
    }
}

#[test]
fn test_open_positions_with_total_row() {
    let xlsx_bytes = include_bytes!("fixtures/xtb/open_positions.xlsx");
    let csv_bytes = include_bytes!("fixtures/xtb/open_positions.csv");
    let options = ImportOptions::default();

    for (bytes, name) in [
        (xlsx_bytes.as_slice(), "open_positions.xlsx"),
        (csv_bytes.as_slice(), "open_positions.csv"),
    ] {
        let report = parse(bytes, name, &options).expect("should parse open positions report");

        // Open positions
        assert_eq!(report.snapshot.open_positions.len(), 2);

        let p1 = &report.snapshot.open_positions[0];
        assert_eq!(p1.instrument.symbol, "AAPL.US");
        assert_eq!(p1.instrument.kind, InstrumentKind::Stock);
        assert_eq!(p1.quantity, d("10.0"));
        // Per-unit prices need the instrument's currency, which this export doesn't
        // state; the value at export is purchase value + gross P/L in account currency.
        assert_eq!(p1.open_price, None);
        assert_eq!(p1.market_price, None);
        assert_eq!(p1.market_value.as_ref().unwrap().amount, d("1800.0"));
        assert_eq!(p1.instrument.currency, None);

        let p2 = &report.snapshot.open_positions[1];
        assert_eq!(p2.instrument.symbol, "US500");
        assert_eq!(p2.instrument.kind, InstrumentKind::Cfd);
        assert_eq!(p2.quantity, d("1.5"));
        assert_eq!(p2.instrument.key(), "sym:US500#cfd");

        // Summary row parsed into balance & equity
        assert_eq!(
            report.snapshot.balance.as_ref().unwrap().amount,
            d("4500.00")
        );
        assert_eq!(
            report.snapshot.equity.as_ref().unwrap().amount,
            d("12450.00")
        );

        // As-of date from sheet name in xlsx
        if name.ends_with(".xlsx") {
            assert!(report.snapshot.as_of.is_some());
            let as_of = report.snapshot.as_of.unwrap();
            assert_eq!(as_of.year(), 2025);
            assert_eq!(as_of.month(), 3);
            assert_eq!(as_of.day(), 24);
        }
    }
}

#[test]
fn test_closed_positions() {
    let xlsx_bytes = include_bytes!("fixtures/xtb/closed_positions.xlsx");
    let csv_bytes = include_bytes!("fixtures/xtb/closed_positions.csv");

    // This export states no currency anywhere: without the owner saying which, every row
    // is rejected rather than assumed to be EUR.
    let unstated = parse(csv_bytes, "closed_positions.csv", &ImportOptions::default())
        .expect("the file itself is readable");
    assert!(unstated.snapshot.closed_positions.is_empty());
    assert!(
        unstated
            .issues
            .iter()
            .any(|issue| issue.code == "missing_currency")
    );

    let options = ImportOptions {
        default_currency: Currency::parse("EUR"),
        ..ImportOptions::default()
    };

    for (bytes, name) in [
        (xlsx_bytes.as_slice(), "closed_positions.xlsx"),
        (csv_bytes.as_slice(), "closed_positions.csv"),
    ] {
        let report = parse(bytes, name, &options).expect("should parse closed positions report");

        assert_eq!(report.snapshot.closed_positions.len(), 2);

        let cp1 = &report.snapshot.closed_positions[0];
        assert_eq!(cp1.instrument.symbol, "NVDA.US");
        assert_eq!(cp1.quantity, d("5.0"));
        assert_eq!(cp1.open_price, None);
        assert_eq!(cp1.close_price, None);
        assert_eq!(cp1.realized.as_ref().unwrap().amount, d("100.00"));

        let cp2 = &report.snapshot.closed_positions[1];
        assert_eq!(cp2.instrument.symbol, "OIL");
        assert_eq!(cp2.instrument.kind, InstrumentKind::Cfd);
        assert_eq!(cp2.quantity, d("2.0"));
    }
}

#[test]
fn test_multi_sheet_workbook() {
    let bytes = include_bytes!("fixtures/xtb/multi_sheet.xlsx");
    let options = ImportOptions::default();
    let report =
        parse(bytes, "multi_sheet.xlsx", &options).expect("should parse multi-sheet report");

    // All sheets parsed into the single report
    assert_eq!(report.entries.len(), 13);
    assert_eq!(report.snapshot.open_positions.len(), 2);
    assert_eq!(report.snapshot.closed_positions.len(), 2);
    assert_eq!(report.sheets.len(), 5);

    // Verify sheet kinds
    let kinds: Vec<_> = report.sheets.iter().map(|s| s.kind).collect();
    assert_eq!(
        kinds,
        vec![
            sail_capital::contracts::SheetKind::CashOperations,
            sail_capital::contracts::SheetKind::OpenPositions,
            sail_capital::contracts::SheetKind::ClosedPositions,
            sail_capital::contracts::SheetKind::Orders,
            sail_capital::contracts::SheetKind::Unknown,
        ]
    );

    // Unknown sheet generated an Info issue
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.severity == Severity::Info && i.code == "unknown_sheet")
    );
}

#[test]
fn test_edge_cases() {
    let bytes = include_bytes!("fixtures/xtb/edge_cases.csv");
    let options = ImportOptions::default();
    let report = parse(bytes, "edge_cases.csv", &options).expect("should parse edge cases report");

    // 1. Unknown type is kept as Unsupported with a Warning issue
    let unsupported = report
        .entries
        .iter()
        .find(|e| matches!(&e.kind, EntryKind::Unsupported { .. }));
    assert!(unsupported.is_some());
    assert_eq!(
        unsupported.unwrap().kind,
        EntryKind::Unsupported {
            raw_type: "Crypto Staking Reward".to_string()
        }
    );
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == "unknown_type" && i.severity == Severity::Warning)
    );

    // 2. Ambiguous number (1,234) and invalid number (not-a-number) caused row rejection with bad_number issue
    let bad_numbers: Vec<_> = report
        .issues
        .iter()
        .filter(|i| i.code == "bad_number")
        .collect();
    assert_eq!(bad_numbers.len(), 2);
    assert_eq!(report.sheets[0].rejected, 2);

    // 3. Over-long cell truncated to MAX_CELL_CHARS with Warning issue
    let long_entry = report.entries.iter().find(|e| {
        e.comment
            .as_deref()
            .map(|c| c.len() == MAX_CELL_CHARS)
            .unwrap_or(false)
    });
    assert!(long_entry.is_some());
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == "cell_text_truncated" && i.severity == Severity::Warning)
    );

    // 4. Duplicate rows with NO ID disambiguated to #2, #3
    let interest_entries: Vec<_> = report
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::Interest)
        .collect();
    assert_eq!(interest_entries.len(), 3);
    assert!(!interest_entries[0].natural_key.contains('#'));
    assert!(interest_entries[1].natural_key.ends_with("#2"));
    assert!(interest_entries[2].natural_key.ends_with("#3"));
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == "duplicate_row_disambiguated" && i.severity == Severity::Info)
    );
}

#[test]
fn test_dst_fall_back_hour() {
    // In Europe/Lisbon, at 02:00 WEST clocks fall back to 01:00 WET.
    // 01:30:00 on 2025-10-26 is therefore the ambiguous fall-back hour.
    let csv = "Account:;51234567;Currency:;EUR\nID;Type;Time;Comment;Symbol;Amount\n101;Deposit;26.10.2025 01:30:00;;;100.00\n";
    let options = ImportOptions {
        report_tz: chrono_tz::Europe::Lisbon,
        default_currency: None,
    };
    let report = parse(csv.as_bytes(), "dst.csv", &options).expect("should parse DST transition");
    assert_eq!(report.entries.len(), 1);

    // Resolved to earliest instant (UTC 00:30) with an Info issue
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == "ambiguous_dst_time" && i.severity == Severity::Info)
    );
    assert_eq!(
        report.entries[0].occurred.at,
        chrono::Utc
            .with_ymd_and_hms(2025, 10, 26, 0, 30, 0)
            .unwrap()
    );
}

#[test]
fn test_empty_file_rejected() {
    let options = ImportOptions::default();
    let res = parse(&[], "empty.csv", &options);
    assert!(matches!(res, Err(ImportError::Unreadable(_))));
}

#[test]
fn test_not_a_report_rejected() {
    let xlsx_bytes = include_bytes!("fixtures/xtb/not_a_report.xlsx");
    let csv_bytes = include_bytes!("fixtures/xtb/not_a_report.csv");
    let options = ImportOptions::default();

    assert!(matches!(
        parse(xlsx_bytes, "not_a_report.xlsx", &options),
        Err(ImportError::NoKnownSheet)
    ));
    assert!(matches!(
        parse(csv_bytes, "not_a_report.csv", &options),
        Err(ImportError::NoKnownSheet)
    ));
}

#[test]
fn test_file_over_size_cap_rejected() {
    let options = ImportOptions::default();
    // 32MB + 1 byte
    let large_file = vec![0u8; MAX_REPORT_BYTES + 1];
    let res = parse(&large_file, "large.csv", &options);
    assert_eq!(res, Err(ImportError::TooLarge(MAX_REPORT_BYTES + 1)));
}

#[test]
fn test_idempotency_and_natural_key_stability() {
    let bytes = include_bytes!("fixtures/xtb/english_cash.xlsx");
    let options = ImportOptions::default();

    let report1 = parse(bytes, "english_cash.xlsx", &options).unwrap();
    let report2 = parse(bytes, "english_cash.xlsx", &options).unwrap();

    // 1. Same bytes twice gives identical report
    assert_eq!(report1, report2);

    // 2. An operation keeps its natural key even when appearing at a different row in a longer export
    let short_csv = "Account:;51234567;Currency:;EUR\nID;Type;Time;Comment;Symbol;Amount\n;Free-funds Interest;14.01.2025 00:00:00;interest;;0.50\n";
    let long_csv = "Account:;51234567;Currency:;EUR\nID;Type;Time;Comment;Symbol;Amount\n101;Deposit;01.01.2025 10:00:00;;;100.00\n102;Deposit;02.01.2025 10:00:00;;;200.00\n;Free-funds Interest;14.01.2025 00:00:00;interest;;0.50\n";

    let rep_short = parse(short_csv.as_bytes(), "short.csv", &options).unwrap();
    let rep_long = parse(long_csv.as_bytes(), "long.csv", &options).unwrap();

    let key_short = &rep_short.entries[0].natural_key;
    let key_long = &rep_long.entries[2].natural_key;
    assert_eq!(key_short, key_long);
}
