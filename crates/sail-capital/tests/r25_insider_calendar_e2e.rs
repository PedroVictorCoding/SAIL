//! Round 25 end to end for Antigravity's slice: the Form 4 parser, the insider-cluster
//! signal and the upcoming-payments calendar. Red until that slice lands.

use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate, TimeZone, Utc};
use sail_capital::dividends::calendar::{self, Confidence};
use sail_capital::dividends::{Holding, Payment};
use sail_capital::evidence::{Cik, Filing, PublishedBasis};
use sail_capital::sec::form4::{self, Form4, InsiderTransaction, ReportingOwner};
use sail_capital::signals::insider::{self, CLUSTER_BONUS};

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn filing(accession: &str, form: &str, published: NaiveDate) -> Filing {
    Filing {
        accession: accession.into(),
        cik: Cik(900001),
        form: form.into(),
        filing_date: published,
        published_at: Utc.from_utc_datetime(&published.and_hms_opt(21, 30, 0).unwrap()),
        published_basis: PublishedBasis::Acceptance,
        report_date: None,
        items: Vec::new(),
        primary_document: Some("xslF345X05/form4.xml".into()),
        description: None,
        url: "https://www.sec.gov/".into(),
    }
}

#[test]
fn form4_reads_owners_and_open_market_purchases() {
    let body = include_bytes!("fixtures/r25/form4_director_buy.xml");
    let parsed =
        form4::parse(body, &filing("0000900001-26-000010", "4", d(2026, 3, 3))).expect("parses");
    assert_eq!(parsed.issuer_cik, Cik(900001));
    assert_eq!(parsed.issuer_ticker.as_deref(), Some("XMPL"));
    assert_eq!(parsed.owners.len(), 1);
    assert!(parsed.owners[0].is_director && !parsed.owners[0].is_officer);
    // Two non-derivative rows; the option grant is skipped.
    assert_eq!(parsed.transactions.len(), 2);
    let buy = &parsed.transactions[0];
    assert!(buy.is_open_market_buy());
    assert_eq!(buy.shares, 5000.0);
    assert_eq!(buy.price, Some(41.25));
    assert_eq!(parsed.transactions[1].code, "F");
    assert_eq!(parsed.transactions[1].price, None);
    assert!(!parsed.amendment);

    // Hostile input: a DTD with entities is refused, and so is a document that isn't one.
    let evil = br#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY a "aaaa">]><ownershipDocument>&a;</ownershipDocument>"#;
    assert!(form4::parse(evil, &filing("1", "4", d(2026, 3, 3))).is_err());
    assert!(form4::parse(b"<html></html>", &filing("2", "4", d(2026, 3, 3))).is_err());
}

fn buy(owner: &str, accession: &str, day: NaiveDate, published: NaiveDate) -> Form4 {
    Form4 {
        accession: accession.into(),
        issuer_cik: Cik(900001),
        issuer_ticker: Some("XMPL".into()),
        published_at: Utc.from_utc_datetime(&published.and_hms_opt(21, 0, 0).unwrap()),
        owners: vec![ReportingOwner {
            cik: owner.into(),
            name: owner.into(),
            is_director: true,
            is_officer: false,
            is_ten_pct_owner: false,
            officer_title: None,
        }],
        transactions: vec![InsiderTransaction {
            date: day,
            code: "P".into(),
            shares: 1000.0,
            price: Some(40.0),
            acquired: true,
        }],
        amendment: false,
    }
}

#[test]
fn three_insiders_buying_make_a_cluster_known_only_once_published() {
    let sessions: Vec<NaiveDate> = (0..40)
        .map(|i| d(2026, 3, 2) + Duration::days(i))
        .filter(|day| !matches!(day.format("%a").to_string().as_str(), "Sat" | "Sun"))
        .collect();
    let mut amended = buy("o3", "a3a", d(2026, 3, 4), d(2026, 3, 9));
    amended.amendment = true;
    let filings = BTreeMap::from([(
        "XMPL".to_string(),
        vec![
            buy("o1", "a1", d(2026, 3, 2), d(2026, 3, 3)),
            buy("o2", "a2", d(2026, 3, 3), d(2026, 3, 4)),
            buy("o3", "a3", d(2026, 3, 4), d(2026, 3, 6)),
            // An amendment of o3's filing: never counted twice.
            amended,
        ],
    )]);
    let by_key = BTreeMap::from([("XMPL".to_string(), sessions.clone())]);
    let values = insider::cluster_scores(&filings, &by_key);
    let at = |day: NaiveDate| {
        values
            .iter()
            .find(|(k, s, _)| k == "XMPL" && *s == day)
            .map(|(_, _, v)| *v)
    };
    // 2026-03-05: o3's filing isn't published yet (2026-03-06) → two buyers.
    assert_eq!(at(d(2026, 3, 5)), Some(2.0));
    // 2026-03-06: three buyers → a cluster.
    assert_eq!(at(d(2026, 3, 6)), Some(3.0 + CLUSTER_BONUS));
    assert_eq!(at(d(2026, 3, 10)), Some(3.0 + CLUSTER_BONUS));
    // Well after the 10-session window: zero, not missing.
    assert_eq!(at(*sessions.last().unwrap()), Some(0.0));
    assert_eq!(
        insider::current_clusters(
            &filings,
            &BTreeMap::from([("XMPL".to_string(), sessions[..8].to_vec())])
        ),
        vec![("XMPL".to_string(), 3)]
    );
}

#[test]
fn the_calendar_lists_the_next_quarterly_payments() {
    // Paid every quarter since 2023 around the 10th of Feb/May/Aug/Nov.
    let mut history = Vec::new();
    for year in 2023..=2026 {
        for month in [2u32, 5, 8, 11] {
            let ex = d(year, month, 10);
            if ex <= d(2026, 8, 10) {
                history.push(Payment {
                    ex_date: ex,
                    per_share: 0.25,
                });
            }
        }
    }
    let holding = Holding {
        key: "XMPL".into(),
        symbol: "XMPL.US".into(),
        units: 100.0,
        price: 40.0,
        fx: 0.9,
        history,
        withholding: 0.15,
        withholding_source: "observed".into(),
        pt_rate: 0.28,
        price_growth: None,
    };
    let paid = BTreeMap::from([(
        "XMPL".to_string(),
        vec![d(2026, 2, 24), d(2026, 5, 24), d(2026, 8, 24)],
    )]);
    let today = d(2026, 9, 1);
    let upcoming = calendar::upcoming(std::slice::from_ref(&holding), &paid, today, 90);
    assert_eq!(upcoming.len(), 1, "{upcoming:?}");
    let next = &upcoming[0];
    assert!((next.ex_date - d(2026, 11, 10)).num_days().abs() <= 4);
    assert!(!next.pay_date_estimated);
    assert!((next.pay_date - next.ex_date).num_days() == 14);
    assert!((next.gross - 0.25 * 100.0 * 0.9).abs() < 1e-9);
    assert!((next.net - next.gross * 0.85).abs() < 1e-9);
    assert_eq!(next.confidence, Confidence::High);
    assert!(!next.basis.is_empty());
    // Nothing paid for 14 months: low confidence.
    let mut stale = holding.clone();
    stale.history.retain(|p| p.ex_date < d(2025, 6, 1));
    let stale_list = calendar::upcoming(&[stale], &BTreeMap::new(), today, 90);
    assert!(stale_list.iter().all(|p| p.confidence == Confidence::Low));
}
