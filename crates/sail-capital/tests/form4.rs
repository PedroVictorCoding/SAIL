use chrono::{NaiveDate, TimeZone, Utc};
use sail_capital::evidence::{
    Cik, Filing, MAX_SEC_RESPONSE_BYTES, MAX_SEC_TEXT_CHARS, PublishedBasis, SourceError,
};
use sail_capital::sec::form4::{MAX_TRANSACTIONS, parse};

fn filing(accession: &str, form: &str, published: NaiveDate) -> Filing {
    Filing {
        accession: accession.into(),
        cik: Cik(900001),
        form: form.into(),
        filing_date: published,
        published_at: Utc.from_utc_datetime(&published.and_hms_opt(20, 0, 0).unwrap()),
        published_basis: PublishedBasis::Acceptance,
        report_date: None,
        items: Vec::new(),
        primary_document: Some("xslF345X05/form4.xml".into()),
        description: None,
        url: "https://www.sec.gov/".into(),
    }
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).expect("valid date")
}

#[test]
fn parses_joint_filing_and_amendment_sale() {
    let joint = include_bytes!("fixtures/r25/form4_joint_owners.xml");
    let f = filing("0000900002-26-000001", "4", d(2026, 4, 2));
    let parsed = parse(joint, &f).expect("joint parses");
    assert_eq!(parsed, parse(joint, &f).expect("pure"));
    assert_eq!(parsed.owners.len(), 2);
    assert!(parsed.owners[0].is_director);
    assert!(parsed.owners[1].is_officer);
    assert_eq!(
        parsed.owners[1].officer_title.as_deref(),
        Some("Chief Financial Officer")
    );
    assert!(!parsed.amendment);
    assert_eq!(parsed.transactions.len(), 1);
    assert!(parsed.transactions[0].is_open_market_buy());

    let sale = include_bytes!("fixtures/r25/form4_sale.xml");
    let f_amend = filing("0000900001-26-000012", "4/A", d(2026, 3, 5));
    let parsed_sale = parse(sale, &f_amend).expect("sale parses");
    assert!(parsed_sale.amendment);
    assert_eq!(parsed_sale.transactions.len(), 1);
    assert!(parsed_sale.transactions[0].is_open_market_sell());
}

#[test]
fn hostile_and_oversized_inputs_are_safely_rejected_or_capped() {
    let f = filing("1", "4", d(2026, 3, 3));
    let oversized = vec![b' '; MAX_SEC_RESPONSE_BYTES + 1];
    assert!(matches!(parse(&oversized, &f), Err(SourceError::TooLarge)));
    assert!(matches!(parse(b"", &f), Err(SourceError::Malformed(_))));
    assert!(matches!(
        parse(b"\xff\xfe\xfd", &f),
        Err(SourceError::Malformed(_))
    ));
    assert!(matches!(
        parse(b"<notForm4/>", &f),
        Err(SourceError::Malformed(_))
    ));

    // Negative/non-finite shares dropped; text capped; transactions capped at MAX_TRANSACTIONS.
    let long_name = "A".repeat(MAX_SEC_TEXT_CHARS + 50);
    let mut txs = String::new();
    for _ in 0..(MAX_TRANSACTIONS + 10) {
        txs.push_str(
            "<nonDerivativeTransaction>\
            <transactionDate><value>2026-03-01</value></transactionDate>\
            <transactionCoding><transactionCode>P</transactionCode></transactionCoding>\
            <transactionAmounts>\
            <transactionShares><value>10</value></transactionShares>\
            <transactionPricePerShare><value>5.0</value></transactionPricePerShare>\
            <transactionAcquiredDisposedCode><value>A</value></transactionAcquiredDisposedCode>\
            </transactionAmounts></nonDerivativeTransaction>",
        );
    }
    txs.push_str(
        "<nonDerivativeTransaction>\
        <transactionDate><value>2026-03-01</value></transactionDate>\
        <transactionCoding><transactionCode>P</transactionCode></transactionCoding>\
        <transactionAmounts>\
        <transactionShares><value>-5</value></transactionShares>\
        <transactionAcquiredDisposedCode><value>A</value></transactionAcquiredDisposedCode>\
        </transactionAmounts></nonDerivativeTransaction>",
    );
    let xml = format!(
        "<ownershipDocument><issuer><issuerCik>900001</issuerCik></issuer>\
        <reportingOwner><reportingOwnerId><rptOwnerCik>1</rptOwnerCik><rptOwnerName>{long_name}</rptOwnerName></reportingOwnerId></reportingOwner>\
        <nonDerivativeTable>{txs}</nonDerivativeTable></ownershipDocument>"
    );
    let parsed = parse(xml.as_bytes(), &f).expect("capped parses");
    assert_eq!(parsed.owners[0].name.chars().count(), MAX_SEC_TEXT_CHARS);
    assert_eq!(parsed.transactions.len(), MAX_TRANSACTIONS);
}
