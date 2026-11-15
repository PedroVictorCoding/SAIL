use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate, TimeZone, Utc};
use sail_capital::evidence::{Cik, Filing, PublishedBasis};
use sail_capital::sec::form4::{self, Form4, InsiderTransaction, ReportingOwner, parse};
use sail_capital::signals::insider::{CLUSTER_BONUS, cluster_scores, current_clusters};

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).expect("valid date")
}

fn weekday_sessions(start: NaiveDate, count: i64) -> Vec<NaiveDate> {
    (0..count)
        .map(|i| start + Duration::days(i))
        .filter(|day| !matches!(day.format("%a").to_string().as_str(), "Sat" | "Sun"))
        .collect()
}

fn filing(accession: &str, form: &str, published: NaiveDate, hour: u32) -> Filing {
    Filing {
        accession: accession.into(),
        cik: Cik(900001),
        form: form.into(),
        filing_date: published,
        published_at: Utc.from_utc_datetime(&published.and_hms_opt(hour, 0, 0).unwrap()),
        published_basis: PublishedBasis::Acceptance,
        report_date: None,
        items: Vec::new(),
        primary_document: None,
        description: None,
        url: "https://www.sec.gov/".into(),
    }
}

fn buy(owner: &str, accession: &str, tx_day: NaiveDate, pub_day: NaiveDate, hour: u32) -> Form4 {
    Form4 {
        accession: accession.into(),
        issuer_cik: Cik(900001),
        issuer_ticker: Some("XMPL".into()),
        published_at: Utc.from_utc_datetime(&pub_day.and_hms_opt(hour, 0, 0).unwrap()),
        owners: vec![ReportingOwner {
            cik: owner.into(),
            name: owner.into(),
            is_director: true,
            is_officer: false,
            is_ten_pct_owner: false,
            officer_title: None,
        }],
        transactions: vec![InsiderTransaction {
            date: tx_day,
            code: "P".into(),
            shares: 500.0,
            price: Some(25.0),
            acquired: true,
        }],
        amendment: false,
    }
}

#[test]
fn ten_pct_owner_only_is_ignored_and_sellers_offset_half() {
    let sessions = weekday_sessions(d(2026, 3, 2), 20);
    let tenp_xml = include_bytes!("fixtures/r25/form4_ten_pct_only.xml");
    let tenp_parsed = parse(tenp_xml, &filing("tp1", "4", d(2026, 3, 2), 18)).expect("parses");

    let sale_xml = include_bytes!("fixtures/r25/form4_sale.xml");
    let sale_parsed = parse(sale_xml, &filing("s1", "4", d(2026, 3, 4), 18)).expect("parses");

    let filings = BTreeMap::from([
        ("TENP".to_string(), vec![tenp_parsed]),
        (
            "XMPL".to_string(),
            vec![
                buy("o1", "b1", d(2026, 3, 2), d(2026, 3, 2), 18),
                sale_parsed,
            ],
        ),
    ]);
    let sess_map = BTreeMap::from([
        ("TENP".to_string(), sessions.clone()),
        ("XMPL".to_string(), sessions.clone()),
    ]);

    let scores = cluster_scores(&filings, &sess_map);
    assert_eq!(scores, cluster_scores(&filings, &sess_map));

    let at = |key: &str, day: NaiveDate| {
        scores
            .iter()
            .find(|(k, s, _)| k == key && *s == day)
            .map(|(_, _, v)| *v)
    };
    // 10%-owner-only buy contributes 0.0, not 1.0.
    assert_eq!(at("TENP", d(2026, 3, 2)), Some(0.0));
    // XMPL: 1 buyer - 0.5 * 1 seller = 0.5 on 2026-03-04.
    assert_eq!(at("XMPL", d(2026, 3, 4)), Some(0.5));
}

#[test]
fn no_lookahead_when_future_filing_changes() {
    let sessions = weekday_sessions(d(2026, 3, 2), 25);
    let base_filings = BTreeMap::from([(
        "XMPL".to_string(),
        vec![
            buy("o1", "a1", d(2026, 3, 2), d(2026, 3, 3), 18),
            buy("o2", "a2", d(2026, 3, 3), d(2026, 3, 4), 18),
        ],
    )]);
    let mut with_future = base_filings.clone();
    // Accepted at 22:30 UTC (after 16:00 NY close on 2026-03-06 -> counts 2026-03-09).
    with_future
        .get_mut("XMPL")
        .unwrap()
        .push(buy("o3", "a3", d(2026, 3, 5), d(2026, 3, 6), 22));

    let sess_map = BTreeMap::from([("XMPL".to_string(), sessions.clone())]);
    let s1 = cluster_scores(&base_filings, &sess_map);
    let s2 = cluster_scores(&with_future, &sess_map);

    let earlier_s1: Vec<_> = s1
        .iter()
        .filter(|(_, day, _)| *day <= d(2026, 3, 6))
        .collect();
    let earlier_s2: Vec<_> = s2
        .iter()
        .filter(|(_, day, _)| *day <= d(2026, 3, 6))
        .collect();
    assert_eq!(earlier_s1, earlier_s2);

    // On 2026-03-09, the after-hours filing becomes known -> cluster!
    let mar9 = s2
        .iter()
        .find(|(_, day, _)| *day == d(2026, 3, 9))
        .map(|(_, _, v)| *v);
    assert_eq!(mar9, Some(3.0 + CLUSTER_BONUS));
}

#[test]
fn joint_filing_counts_each_distinct_owner_once() {
    let sessions = weekday_sessions(d(2026, 4, 1), 15);
    let joint_xml = include_bytes!("fixtures/r25/form4_joint_owners.xml");
    let joint = form4::parse(joint_xml, &filing("j1", "4", d(2026, 4, 2), 18)).expect("parses");

    let filings = BTreeMap::from([(
        "JNT".to_string(),
        vec![
            joint,
            buy("0001700013", "b3", d(2026, 4, 2), d(2026, 4, 2), 18),
        ],
    )]);
    let sess_map = BTreeMap::from([("JNT".to_string(), sessions[..5].to_vec())]);
    assert_eq!(
        current_clusters(&filings, &sess_map),
        vec![("JNT".to_string(), 3)]
    );
}
