//! Tests for Stooq and owner price file parsers.
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sail_capital::contracts::Currency;
use sail_capital::market::{
    Adjustment, MAX_PRICE_FILE_BYTES, PriceSourceError, parse_price_file, parse_stooq_csv,
    stooq_symbol,
};
use std::str::FromStr;

const STOOQ_AAPL: &[u8] = include_bytes!("fixtures/market/stooq_aapl.csv");
const STOOQ_DUPS: &[u8] = include_bytes!("fixtures/market/stooq_duplicates_and_unsorted.csv");
const STOOQ_BAD: &[u8] = include_bytes!("fixtures/market/stooq_bad_rows.csv");
const FILE_SEMICOLON_PT: &[u8] = include_bytes!("fixtures/market/price_file_semicolon_pt.csv");
const FILE_ADJ_CLOSE: &[u8] = include_bytes!("fixtures/market/price_file_adj_and_close.csv");

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).expect("valid decimal")
}

#[test]
fn test_stooq_symbol_mapping() {
    // Suffix mappings
    assert_eq!(stooq_symbol("AAPL.US"), Some("aapl.us".to_string()));
    assert_eq!(stooq_symbol("SAP.DE"), Some("sap.de".to_string()));
    assert_eq!(stooq_symbol("VOD.UK"), Some("vod.uk".to_string()));
    assert_eq!(stooq_symbol("AIR.FR"), Some("air.fr".to_string()));
    assert_eq!(stooq_symbol("ASML.NL"), Some("asml.nl".to_string()));
    assert_eq!(stooq_symbol("EDP.PT"), Some("edp.pt".to_string()));
    assert_eq!(stooq_symbol("IBE.ES"), Some("ibe.es".to_string()));
    assert_eq!(stooq_symbol("ENI.IT"), Some("eni.it".to_string()));
    assert_eq!(stooq_symbol("PKN.PL"), Some("pkn.pl".to_string()));

    // Lowercase conversion of symbol
    assert_eq!(stooq_symbol("Brk.b.us"), Some("brk.b.us".to_string()));

    // CFDs or unmapped suffixes
    assert_eq!(stooq_symbol("US500"), None);
    assert_eq!(stooq_symbol("GOLD"), None);
    assert_eq!(stooq_symbol("OIL.WTI"), None);
    assert_eq!(stooq_symbol("TEST.XX"), None);
    assert_eq!(stooq_symbol(""), None);
    assert_eq!(stooq_symbol(".US"), None);
}

#[test]
fn test_stooq_csv_happy_path() {
    let series = parse_stooq_csv(STOOQ_AAPL, "AAPL.US", "aapl.us").expect("valid series");
    assert_eq!(series.source_id, "stooq");
    assert_eq!(series.symbol, "AAPL.US");
    assert_eq!(series.source_symbol, "aapl.us");
    assert_eq!(series.adjustment, Adjustment::Unknown);
    assert_eq!(series.bars.len(), 3);

    // Oldest first
    assert_eq!(
        series.bars[0].session,
        NaiveDate::from_ymd_opt(2024, 5, 1).unwrap()
    );
    assert_eq!(series.bars[0].close, d("171.00"));
    assert_eq!(series.bars[0].open, Some(d("170.00")));
    assert_eq!(series.bars[0].volume, Some(d("1000000")));

    assert_eq!(
        series.bars[2].session,
        NaiveDate::from_ymd_opt(2024, 5, 3).unwrap()
    );
    assert_eq!(series.bars[2].close, d("175.50"));
}

#[test]
fn test_stooq_csv_no_data_response() {
    let err = parse_stooq_csv(b"No data\n", "XYZ.US", "xyz.us").unwrap_err();
    assert_eq!(err, PriceSourceError::UnknownSymbol("XYZ.US".to_string()));

    let err2 = parse_stooq_csv(b"no data", "XYZ.US", "xyz.us").unwrap_err();
    assert_eq!(err2, PriceSourceError::UnknownSymbol("XYZ.US".to_string()));
}

#[test]
fn test_stooq_csv_unsorted_and_duplicate_sessions() {
    let series = parse_stooq_csv(STOOQ_DUPS, "AAPL.US", "aapl.us").expect("valid series");
    assert_eq!(series.bars.len(), 3);

    // Sorted oldest first
    assert_eq!(
        series.bars[0].session,
        NaiveDate::from_ymd_opt(2024, 5, 1).unwrap()
    );
    // Duplicate 2024-05-01 kept the last occurrence in file (close: 171.50)
    assert_eq!(series.bars[0].close, d("171.50"));

    assert_eq!(
        series.bars[1].session,
        NaiveDate::from_ymd_opt(2024, 5, 2).unwrap()
    );
    assert_eq!(series.bars[1].close, d("173.00"));

    assert_eq!(
        series.bars[2].session,
        NaiveDate::from_ymd_opt(2024, 5, 3).unwrap()
    );
    assert_eq!(series.bars[2].close, d("175.50"));
}

#[test]
fn test_stooq_csv_skips_bad_and_non_positive_close() {
    let series = parse_stooq_csv(STOOQ_BAD, "AAPL.US", "aapl.us").expect("valid series");
    // Row 2 (not-a-date), row 3 (close 0.00), row 4 (close -5.00) skipped
    assert_eq!(series.bars.len(), 2);
    assert_eq!(
        series.bars[0].session,
        NaiveDate::from_ymd_opt(2024, 5, 1).unwrap()
    );
    assert_eq!(series.bars[0].close, d("171.00"));
    assert_eq!(
        series.bars[1].session,
        NaiveDate::from_ymd_opt(2024, 5, 4).unwrap()
    );
    assert_eq!(series.bars[1].close, d("175.50"));
}

#[test]
fn test_price_file_semicolon_and_portuguese() {
    let series = parse_price_file(
        FILE_SEMICOLON_PT,
        "EDP.PT",
        Some(Currency::eur()),
        Adjustment::SplitAdjusted,
    )
    .expect("valid series");

    assert_eq!(series.source_id, "owner-file");
    assert_eq!(series.symbol, "EDP.PT");
    assert_eq!(series.source_symbol, "EDP.PT");
    assert_eq!(series.currency, Some(Currency::eur()));
    assert_eq!(series.adjustment, Adjustment::SplitAdjusted);
    assert_eq!(series.bars.len(), 3);

    // Oldest first
    assert_eq!(
        series.bars[0].session,
        NaiveDate::from_ymd_opt(2024, 5, 1).unwrap()
    );
    assert_eq!(series.bars[0].close, d("15.70"));
    assert_eq!(series.bars[0].open, Some(d("15.50")));

    // Slashes in date: 03/05/2024
    assert_eq!(
        series.bars[2].session,
        NaiveDate::from_ymd_opt(2024, 5, 3).unwrap()
    );
    assert_eq!(series.bars[2].close, d("16.10"));
}

#[test]
fn test_price_file_prefers_close_over_adj_close() {
    let series = parse_price_file(FILE_ADJ_CLOSE, "TEST.US", None, Adjustment::Unadjusted)
        .expect("valid series");

    assert_eq!(series.bars.len(), 2);
    // Uses "Close" (10.20) rather than "Adj Close" (9.90)
    assert_eq!(series.bars[0].close, d("10.20"));
    assert_eq!(series.bars[1].close, d("10.40"));
}

#[test]
fn test_price_file_with_bom() {
    let mut bom_file = vec![0xEF, 0xBB, 0xBF];
    bom_file.extend_from_slice(b"Date,Close\n2024-05-01,100.50\n");

    let series =
        parse_price_file(&bom_file, "BOM.US", None, Adjustment::Unknown).expect("valid series");

    assert_eq!(series.bars.len(), 1);
    assert_eq!(
        series.bars[0].session,
        NaiveDate::from_ymd_opt(2024, 5, 1).unwrap()
    );
    assert_eq!(series.bars[0].close, d("100.50"));
}

#[test]
fn test_price_file_size_cap() {
    let oversized = vec![b'a'; MAX_PRICE_FILE_BYTES + 1];
    let err = parse_price_file(&oversized, "BIG.US", None, Adjustment::Unknown).unwrap_err();
    assert_eq!(err, PriceSourceError::TooLarge);
}

#[test]
fn stooq_bot_check_page_is_blocked_not_malformed() {
    let page = b"<!DOCTYPE html><html><head></head><body><noscript>This site requires JavaScript to verify your browser.</noscript></body></html>";
    let err = sail_capital::market::parse_stooq_csv(page, "AAPL.US", "aapl.us").unwrap_err();
    assert_eq!(err, sail_capital::market::PriceSourceError::Blocked);
}

#[test]
fn comma_price_files_read_three_decimals_as_a_fraction() {
    let csv = b"Date,Open,High,Low,Close,Adj Close,Volume\n2026-11-10,54.123,54.5,53.125,54.250,54.250,1200\n2026-11-11,54.3,54.9,54.1,54.875,54.875,900\n";
    let series = sail_capital::market::parse_price_file(
        csv,
        "SAP.DE",
        None,
        sail_capital::market::Adjustment::SplitAdjusted,
    )
    .unwrap();
    assert_eq!(series.bars.len(), 2);
    assert_eq!(series.bars[0].close.to_string(), "54.250");
    assert_eq!(
        series.bars[0].open.map(|d| d.to_string()).as_deref(),
        Some("54.123")
    );
    assert_eq!(series.bars[1].close.to_string(), "54.875");
}
