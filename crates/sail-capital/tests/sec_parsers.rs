//! Parser tests for SEC EDGAR's ticker map and submissions JSON. Every fixture is
//! synthetic; no request leaves the process.
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use sail_capital::evidence::*;
use sail_capital::sec::{parse_submissions, parse_ticker_index};
use serde_json::json;

const TICKER_MAP: &[u8] = include_bytes!("fixtures/sec/ticker_map.json");
const SUBMISSIONS: &[u8] = include_bytes!("fixtures/sec/submissions_acme.json");
const UNEVEN: &[u8] = include_bytes!("fixtures/sec/submissions_uneven.json");

fn fetched_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2024, 12, 1, 0, 0, 0)
        .single()
        .expect("test time")
}

fn cik(value: u64) -> Cik {
    Cik::new(value).expect("test cik")
}

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("test date")
}

fn body(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("test json")
}

#[test]
fn ticker_map_is_trimmed_uppercased_and_sorted() {
    let index = parse_ticker_index(TICKER_MAP, fetched_at()).expect("parse ticker map");

    assert_eq!(index.fetched_at, fetched_at());
    assert_eq!(index.payload_sha256, sail_capital::sha256_hex(TICKER_MAP));
    let rows: Vec<(&str, u64, &str)> = index
        .entries
        .iter()
        .map(|entry| (entry.ticker.as_str(), entry.cik.0, entry.title.as_str()))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("ACME", 1234567, "Acme Widgets Inc."),
            ("ACME", 1234567, "Acme Widgets Inc."),
            ("BRK-B", 1111111, "Berkshire Hathaway"),
            ("ZZZ", 7654321, "Zeta Zippers"),
        ]
    );
}

#[test]
fn ticker_map_accepts_a_numeric_string_cik() {
    let bytes = body(&json!({
        "0": {"cik_str": "7654321", "ticker": "abc", "title": "String Cik"}
    }));
    let index = parse_ticker_index(&bytes, fetched_at()).expect("parse");

    assert_eq!(index.entries.len(), 1);
    assert_eq!(index.entries[0].cik, cik(7654321));
    assert_eq!(index.entries[0].ticker, "ABC");
}

#[test]
fn ticker_map_titles_are_capped() {
    let long_title = "x".repeat(MAX_SEC_TEXT_CHARS + 50);
    let bytes = body(&json!({
        "0": {"cik_str": 1234567, "ticker": "ACME", "title": long_title}
    }));
    let index = parse_ticker_index(&bytes, fetched_at()).expect("parse");

    assert_eq!(index.entries[0].title.chars().count(), MAX_SEC_TEXT_CHARS);
}

#[test]
fn ticker_map_rejects_a_body_that_is_not_this_shape() {
    for bytes in [
        b"not json".as_slice(),
        b"[]".as_slice(),
        b"{\"0\": []}".as_slice(),
        b"{} trailing".as_slice(),
    ] {
        assert!(
            matches!(
                parse_ticker_index(bytes, fetched_at()),
                Err(SourceError::Malformed(_))
            ),
            "body {bytes:?} should be malformed"
        );
    }
}

#[test]
fn submissions_happy_path_reads_every_field() {
    let filings = parse_submissions(SUBMISSIONS, cik(1234567)).expect("parse submissions");

    assert_eq!(filings.cik, cik(1234567));
    assert_eq!(filings.name, "Acme Widgets Inc.");
    assert_eq!(filings.tickers, vec!["ACME".to_string()]);
    assert_eq!(
        filings.payload_sha256,
        sail_capital::sha256_hex(SUBMISSIONS)
    );
    assert_eq!(filings.filings.len(), 3);

    let annual = &filings.filings[0];
    assert_eq!(annual.accession, "0001234567-24-000001");
    assert_eq!(annual.cik, cik(1234567));
    assert_eq!(annual.form, "10-K");
    assert_eq!(annual.filing_date, date(2024, 11, 1));
    assert_eq!(annual.report_date, Some(date(2024, 9, 28)));
    assert_eq!(annual.published_basis, PublishedBasis::Acceptance);
    assert_eq!(
        annual.published_at,
        Utc.with_ymd_and_hms(2024, 11, 1, 10, 1, 36)
            .single()
            .expect("test time")
    );
    assert!(annual.items.is_empty());
    assert_eq!(
        annual.primary_document.as_deref(),
        Some("acme-20240928.htm")
    );
    assert_eq!(annual.description.as_deref(), Some("10-K"));
    assert_eq!(
        annual.url,
        "https://www.sec.gov/Archives/edgar/data/0001234567/000123456724000001/acme-20240928.htm"
    );

    let current = &filings.filings[1];
    assert_eq!(current.accession, "0001234567-24-000002");
    assert_eq!(current.form, "8-K");
    assert_eq!(current.items, vec!["2.02".to_string(), "9.01".to_string()]);
    assert_eq!(current.report_date, None);
    assert_eq!(current.published_basis, PublishedBasis::FilingDateEndOfDay);
    assert_eq!(
        current.published_at,
        Utc.with_ymd_and_hms(2024, 8, 3, 3, 59, 59)
            .single()
            .expect("test time")
    );
    assert_eq!(current.primary_document, None);
    assert_eq!(current.description, None);
    assert_eq!(
        current.url,
        "https://www.sec.gov/Archives/edgar/data/0001234567/000123456724000002/"
    );

    let quarterly = &filings.filings[2];
    assert_eq!(quarterly.accession, "0001234567-24-000003");
    assert_eq!(
        quarterly.published_at,
        Utc.with_ymd_and_hms(2024, 5, 3, 20, 5, 0)
            .single()
            .expect("test time")
    );
}

#[test]
fn unequal_parallel_arrays_read_the_common_prefix() {
    let filings = parse_submissions(UNEVEN, cik(1234567)).expect("parse uneven");

    assert_eq!(filings.filings.len(), 2);
    assert_eq!(filings.filings[0].accession, "0001234567-24-000011");
    assert_eq!(filings.filings[1].accession, "0001234567-24-000012");
}

#[test]
fn a_bad_accession_is_skipped() {
    let bytes = body(&json!({
        "cik": "1234567",
        "name": "Acme Widgets Inc.",
        "tickers": [],
        "filings": {"recent": {
            "accessionNumber": ["123", "0001234567-24-000001"],
            "filingDate": ["2024-01-02", "2024-01-03"],
            "form": ["10-K", "10-K"],
            "acceptanceDateTime": ["2024-01-02T12:00:00.000Z", "not-a-time"]
        }}
    }));
    let filings = parse_submissions(&bytes, cik(1234567)).expect("parse");

    assert_eq!(filings.filings.len(), 1);
    let filing = &filings.filings[0];
    assert_eq!(filing.accession, "0001234567-24-000001");
    assert_eq!(filing.published_basis, PublishedBasis::FilingDateEndOfDay);
    assert_eq!(
        filing.published_at,
        Utc.with_ymd_and_hms(2024, 1, 4, 4, 59, 59)
            .single()
            .expect("test time")
    );
}

#[test]
fn a_hostile_primary_document_falls_back_to_the_index_url() {
    for hostile in ["../x", "javascript:alert(1)", "a/b.htm", ""] {
        let bytes = body(&json!({
            "cik": "1234567",
            "name": "Acme Widgets Inc.",
            "tickers": [],
            "filings": {"recent": {
                "accessionNumber": ["0001234567-24-000001"],
                "filingDate": ["2024-01-02"],
                "form": ["10-K"],
                "acceptanceDateTime": ["2024-01-02T12:00:00.000Z"],
                "primaryDocument": [hostile]
            }}
        }));
        let filings = parse_submissions(&bytes, cik(1234567)).expect("parse");

        assert_eq!(filings.filings.len(), 1);
        assert_eq!(filings.filings[0].primary_document, None, "{hostile:?}");
        assert_eq!(
            filings.filings[0].url,
            "https://www.sec.gov/Archives/edgar/data/0001234567/000123456724000001/"
        );
    }
}

#[test]
fn submissions_for_another_cik_are_malformed() {
    assert!(matches!(
        parse_submissions(SUBMISSIONS, cik(999)),
        Err(SourceError::Malformed(_))
    ));
}

#[test]
fn submissions_reject_a_body_that_is_not_this_shape() {
    for bytes in [
        b"not json".as_slice(),
        b"[]".as_slice(),
        b"{\"cik\":\"1234567\"}".as_slice(),
        b"{\"cik\":\"1234567\",\"filings\":{}}".as_slice(),
    ] {
        assert!(
            matches!(
                parse_submissions(bytes, cik(1234567)),
                Err(SourceError::Malformed(_))
            ),
            "body {bytes:?} should be malformed"
        );
    }
}

#[test]
fn keeps_the_newest_filings_up_to_the_cap() {
    let count = MAX_FILINGS_PER_ISSUER + 5;
    let accessions: Vec<String> = (1..=count)
        .map(|number| format!("0001234567-24-{number:06}"))
        .collect();
    let bytes = body(&json!({
        "cik": "1234567",
        "name": "Acme Widgets Inc.",
        "tickers": ["ACME"],
        "filings": {"recent": {
            "accessionNumber": accessions,
            "filingDate": vec!["2024-01-02"; count],
            "form": vec!["10-K"; count],
            "acceptanceDateTime": vec!["2024-01-02T12:00:00.000Z"; count]
        }}
    }));
    let filings = parse_submissions(&bytes, cik(1234567)).expect("parse");

    assert_eq!(filings.filings.len(), MAX_FILINGS_PER_ISSUER);
    assert_eq!(filings.filings[0].accession, "0001234567-24-000001");
    assert_eq!(
        filings.filings[MAX_FILINGS_PER_ISSUER - 1].accession,
        "0001234567-24-000200"
    );
}
