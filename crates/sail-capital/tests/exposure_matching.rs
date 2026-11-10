//! Tests for matching instruments to SEC issuers.

use chrono::Utc;
use sail_capital::contracts::{Currency, Instrument, InstrumentKind};
use sail_capital::evidence::{Cik, IssuerMatch, TickerEntry, TickerIndex};
use sail_capital::exposure::match_issuer;

fn test_index() -> TickerIndex {
    TickerIndex {
        fetched_at: Utc::now(),
        payload_sha256: "testsha".into(),
        entries: vec![
            TickerEntry {
                cik: Cik::new(320193).expect("valid cik"),
                ticker: "AAPL".into(),
                title: "Apple Inc.".into(),
            },
            TickerEntry {
                cik: Cik::new(1067983).expect("valid cik"),
                ticker: "BRK-B".into(),
                title: "BERKSHIRE HATHAWAY INC".into(),
            },
            TickerEntry {
                cik: Cik::new(789019).expect("valid cik"),
                ticker: "MSFT".into(),
                title: "MICROSOFT CORP".into(),
            },
            TickerEntry {
                cik: Cik::new(111111).expect("valid cik"),
                ticker: "DUP".into(),
                title: "Duplicate Corp A".into(),
            },
            TickerEntry {
                cik: Cik::new(222222).expect("valid cik"),
                ticker: "DUP".into(),
                title: "Duplicate Corp B".into(),
            },
        ],
    }
}

#[test]
fn matches_us_listed_stock() {
    let index = test_index();
    let inst = Instrument {
        symbol: "AAPL.US".into(),
        isin: None,
        name: Some("Apple Inc.".into()),
        currency: Currency::parse("USD"),
        kind: InstrumentKind::Stock,
    };

    let result = match_issuer(&inst, &index);
    match result {
        IssuerMatch::Matched {
            cik,
            ticker,
            title,
            map_fetched_at,
        } => {
            assert_eq!(cik.0, 320193);
            assert_eq!(ticker, "AAPL");
            assert_eq!(title, "Apple Inc.");
            assert_eq!(map_fetched_at, index.fetched_at);
        }
        other => panic!("expected Matched, got {other:?}"),
    }
}

#[test]
fn rejects_non_us_listed() {
    let index = test_index();
    let inst = Instrument {
        symbol: "SAP.DE".into(),
        isin: None,
        name: Some("SAP SE".into()),
        currency: Some(Currency::eur()),
        kind: InstrumentKind::Stock,
    };

    assert_eq!(match_issuer(&inst, &index), IssuerMatch::NotUsListed);
}

#[test]
fn rejects_cfd_even_if_us_symbol() {
    let index = test_index();
    let inst = Instrument {
        symbol: "AAPL.US".into(),
        isin: None,
        name: Some("Apple CFD".into()),
        currency: Currency::parse("USD"),
        kind: InstrumentKind::Cfd,
    };

    let result = match_issuer(&inst, &index);
    match result {
        IssuerMatch::Unresolved { reason } => {
            assert_eq!(reason, "CFDs are contracts, not shares of the issuer");
        }
        other => panic!("expected Unresolved for CFD, got {other:?}"),
    }
}

#[test]
fn unresolved_when_ticker_missing_from_map() {
    let index = test_index();
    let inst = Instrument {
        symbol: "GOOG.US".into(),
        isin: None,
        name: Some("Alphabet Inc.".into()),
        currency: Currency::parse("USD"),
        kind: InstrumentKind::Stock,
    };

    let result = match_issuer(&inst, &index);
    match result {
        IssuerMatch::Unresolved { reason } => {
            assert!(
                reason.contains("not found in SEC ticker map"),
                "reason was: {reason}"
            );
        }
        other => panic!("expected Unresolved for missing ticker, got {other:?}"),
    }
}

#[test]
fn unresolved_when_ticker_is_duplicate_in_map() {
    let index = test_index();
    let inst = Instrument {
        symbol: "DUP.US".into(),
        isin: None,
        name: Some("Duplicate Corp".into()),
        currency: Currency::parse("USD"),
        kind: InstrumentKind::Stock,
    };

    let result = match_issuer(&inst, &index);
    match result {
        IssuerMatch::Unresolved { reason } => {
            assert!(
                reason.contains("is ambiguous") && reason.contains("matched 2 entries"),
                "reason was: {reason}"
            );
        }
        other => panic!("expected Unresolved for duplicate ticker, got {other:?}"),
    }
}

#[test]
fn matches_dot_to_dash_convention_for_share_class() {
    let index = test_index();
    // Symbol BRK.B.US has ticker BRK.B. Not directly in index, but BRK-B is!
    let inst = Instrument {
        symbol: "BRK.B.US".into(),
        isin: None,
        name: Some("Berkshire Hathaway Class B".into()),
        currency: Currency::parse("USD"),
        kind: InstrumentKind::Stock,
    };

    let result = match_issuer(&inst, &index);
    match result {
        IssuerMatch::Matched {
            cik,
            ticker,
            title,
            map_fetched_at,
        } => {
            assert_eq!(cik.0, 1067983);
            assert_eq!(ticker, "BRK-B");
            assert_eq!(title, "BERKSHIRE HATHAWAY INC");
            assert_eq!(map_fetched_at, index.fetched_at);
        }
        other => panic!("expected Matched via dot-to-dash fallback, got {other:?}"),
    }
}
