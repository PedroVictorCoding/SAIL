//! Tests for `watch_items`: point-in-time visibility, revision handling, share of owned value, and determinism.

use std::str::FromStr;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;
use sail_capital::AsOf;
use sail_capital::contracts::{AccountId, Currency, Instrument, InstrumentKind, Money};
use sail_capital::evidence::{
    Cik, EvidenceRecord, Filing, IssuerMatch, PublishedBasis, ResearchAction,
};
use sail_capital::exposure::watch_items;
use sail_capital::ledger::{Position, Projection, Reconciliation, ReconciliationStatus};

fn d(text: &str) -> Decimal {
    Decimal::from_str(text).expect("valid decimal")
}

fn usd() -> Currency {
    Currency::parse("USD").expect("valid USD")
}

fn make_projection(positions: Vec<Position>) -> Projection {
    Projection {
        account: AccountId::from_stored("xtb:test-account"),
        as_of: AsOf::now(),
        positions,
        cfd_positions: Vec::new(),
        cash: Vec::new(),
        dividends: Vec::new(),
        realized: Vec::new(),
        flows: Vec::new(),
        snapshot_as_of: None,
        reconciliation: Reconciliation {
            status: ReconciliationStatus::Unverified,
            variances: Vec::new(),
            compared: 0,
        },
        warnings: Vec::new(),
        records_used: 0,
    }
}

fn make_position(symbol: &str, quantity: Decimal, market_value: Option<Money>) -> Position {
    Position {
        instrument: Instrument {
            symbol: symbol.into(),
            isin: None,
            name: Some(symbol.into()),
            currency: market_value.as_ref().map(|m| m.currency.clone()),
            kind: InstrumentKind::Stock,
        },
        quantity,
        lots: Vec::new(),
        cost_basis: market_value.clone(),
        broker_market_value: market_value,
    }
}

#[allow(clippy::too_many_arguments)]
fn make_evidence(
    id: &str,
    accession: &str,
    cik: u64,
    form: &str,
    items: Vec<&str>,
    published_at: DateTime<Utc>,
    first_observed_at: DateTime<Utc>,
    ingested_at: DateTime<Utc>,
) -> EvidenceRecord {
    EvidenceRecord {
        id: id.into(),
        natural_key: format!("sec:{accession}"),
        source: "sec-edgar".into(),
        period_end: Some(NaiveDate::from_ymd_opt(2024, 12, 31).expect("valid date")),
        published_at,
        first_observed_at,
        ingested_at,
        payload_sha256: format!("sha_{id}"),
        issuer_name: format!("Issuer {cik}"),
        filing: Filing {
            accession: accession.into(),
            cik: Cik::new(cik).expect("valid cik"),
            form: form.into(),
            filing_date: published_at.date_naive(),
            published_at,
            published_basis: PublishedBasis::Acceptance,
            report_date: Some(NaiveDate::from_ymd_opt(2024, 12, 31).expect("valid date")),
            items: items.into_iter().map(String::from).collect(),
            primary_document: Some("doc.htm".into()),
            description: Some(format!("Description for {id}")),
            url: format!("https://www.sec.gov/Archives/edgar/data/{cik}/{accession}/doc.htm"),
        },
    }
}

#[test]
fn replay_visibility_test() {
    let t_pub = Utc.with_ymd_and_hms(2025, 2, 1, 10, 0, 0).unwrap();
    let t_obs = Utc.with_ymd_and_hms(2025, 2, 1, 11, 0, 0).unwrap();
    let t_ing = Utc.with_ymd_and_hms(2025, 2, 1, 11, 5, 0).unwrap();

    let pos = make_position(
        "AAPL.US",
        d("10"),
        Some(Money {
            amount: d("1500"),
            currency: usd(),
        }),
    );
    let projection = make_projection(vec![pos.clone()]);
    let matches = vec![(
        pos.instrument.key(),
        IssuerMatch::Matched {
            cik: Cik::new(320193).unwrap(),
            ticker: "AAPL".into(),
            title: "Apple Inc.".into(),
            map_fetched_at: t_obs,
        },
    )];

    let evidence = vec![make_evidence(
        "ev1",
        "0000320193-25-000001",
        320193,
        "10-K",
        vec![],
        t_pub,
        t_obs,
        t_ing,
    )];

    // 1. known_by before first_observed_at: invisible
    let as_of_before_obs = AsOf {
        occurred_by: t_pub + chrono::Duration::hours(5),
        known_by: t_obs - chrono::Duration::seconds(1),
    };
    let items = watch_items(&projection, &matches, &evidence, as_of_before_obs);
    assert!(
        items.is_empty(),
        "must be invisible when known_by < first_observed_at"
    );

    // 2. occurred_by before published_at: invisible
    let as_of_before_pub = AsOf {
        occurred_by: t_pub - chrono::Duration::seconds(1),
        known_by: t_ing + chrono::Duration::hours(1),
    };
    let items = watch_items(&projection, &matches, &evidence, as_of_before_pub);
    assert!(
        items.is_empty(),
        "must be invisible when occurred_by < published_at"
    );

    // 3. known_by before ingested_at: invisible
    let as_of_before_ing = AsOf {
        occurred_by: t_pub + chrono::Duration::hours(5),
        known_by: t_ing - chrono::Duration::seconds(1),
    };
    let items = watch_items(&projection, &matches, &evidence, as_of_before_ing);
    assert!(
        items.is_empty(),
        "must be invisible when known_by < ingested_at"
    );

    // 4. visible when bounds cover published_at and known_by covers first_observed_at & ingested_at
    let as_of_visible = AsOf {
        occurred_by: t_pub,
        known_by: t_ing,
    };
    let items = watch_items(&projection, &matches, &evidence, as_of_visible);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].evidence_id, "ev1");
    assert_eq!(items[0].action, ResearchAction::ResearchProposal);
    assert_eq!(items[0].reason, "Annual report (10-K)");
}

#[test]
fn revision_resolution_later_revision_wins_once_visible() {
    let t_pub = Utc.with_ymd_and_hms(2025, 2, 1, 10, 0, 0).unwrap();
    let t_obs = Utc.with_ymd_and_hms(2025, 2, 1, 11, 0, 0).unwrap();
    let t_ing1 = Utc.with_ymd_and_hms(2025, 2, 1, 11, 5, 0).unwrap();
    let t_ing2 = Utc.with_ymd_and_hms(2025, 2, 5, 12, 0, 0).unwrap();

    let pos = make_position(
        "AAPL.US",
        d("10"),
        Some(Money {
            amount: d("1500"),
            currency: usd(),
        }),
    );
    let projection = make_projection(vec![pos.clone()]);
    let matches = vec![(
        pos.instrument.key(),
        IssuerMatch::Matched {
            cik: Cik::new(320193).unwrap(),
            ticker: "AAPL".into(),
            title: "Apple Inc.".into(),
            map_fetched_at: t_obs,
        },
    )];

    let mut ev1 = make_evidence(
        "ev1",
        "0000320193-25-000001",
        320193,
        "8-K",
        vec!["7.01"],
        t_pub,
        t_obs,
        t_ing1,
    );
    ev1.filing.description = Some("Original revision".into());

    let mut ev2 = make_evidence(
        "ev2",
        "0000320193-25-000001",
        320193,
        "8-K",
        vec!["2.02"], // revision updated the content with a material item
        t_pub,
        t_obs,
        t_ing2,
    );
    ev2.filing.description = Some("Amended revision".into());

    let evidence = vec![ev1, ev2];

    // At known_by = 2025-02-03: only ev1 is visible
    let as_of_r1 = AsOf {
        occurred_by: t_pub,
        known_by: Utc.with_ymd_and_hms(2025, 2, 3, 0, 0, 0).unwrap(),
    };
    let items_r1 = watch_items(&projection, &matches, &evidence, as_of_r1);
    assert_eq!(items_r1.len(), 1);
    assert_eq!(items_r1[0].evidence_id, "ev1");
    assert_eq!(items_r1[0].action, ResearchAction::Watch);
    assert_eq!(items_r1[0].reason, "Current report (8-K: 7.01)");

    // At known_by = 2025-02-06: both are visible, later revision (ev2) wins
    let as_of_r2 = AsOf {
        occurred_by: t_pub,
        known_by: Utc.with_ymd_and_hms(2025, 2, 6, 0, 0, 0).unwrap(),
    };
    let items_r2 = watch_items(&projection, &matches, &evidence, as_of_r2);
    assert_eq!(items_r2.len(), 1);
    assert_eq!(items_r2[0].evidence_id, "ev2");
    assert_eq!(items_r2[0].action, ResearchAction::ResearchProposal);
    assert_eq!(
        items_r2[0].reason,
        "Current report: 2.02 results of operations"
    );
}

#[test]
fn share_of_owned_value_behavior() {
    let t = Utc.with_ymd_and_hms(2025, 2, 1, 10, 0, 0).unwrap();
    let as_of = AsOf {
        occurred_by: t,
        known_by: t,
    };

    let p_aapl = make_position(
        "AAPL.US",
        d("10"),
        Some(Money {
            amount: d("100"),
            currency: usd(),
        }),
    );
    let p_msft = make_position(
        "MSFT.US",
        d("5"),
        Some(Money {
            amount: d("300"),
            currency: usd(),
        }),
    );

    let matches = vec![
        (
            p_aapl.instrument.key(),
            IssuerMatch::Matched {
                cik: Cik::new(320193).unwrap(),
                ticker: "AAPL".into(),
                title: "Apple Inc.".into(),
                map_fetched_at: t,
            },
        ),
        (
            p_msft.instrument.key(),
            IssuerMatch::Matched {
                cik: Cik::new(789019).unwrap(),
                ticker: "MSFT".into(),
                title: "Microsoft Corp".into(),
                map_fetched_at: t,
            },
        ),
    ];

    let ev_aapl = make_evidence(
        "ev-aapl",
        "0000320193-25-000001",
        320193,
        "10-Q",
        vec![],
        t,
        t,
        t,
    );

    // 1. Single currency: 100 / 400 = 0.2500
    let proj_single_ccy = make_projection(vec![p_aapl.clone(), p_msft.clone()]);
    let items = watch_items(
        &proj_single_ccy,
        &matches,
        std::slice::from_ref(&ev_aapl),
        as_of,
    );
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].exposure.share_of_owned_value,
        Some(d("0.2500")),
        "share of owned value should be 0.2500"
    );

    // 2. Mixed currency: AAPL in USD, MSFT in EUR -> share_of_owned_value must be None
    let p_msft_eur = make_position(
        "MSFT.US",
        d("5"),
        Some(Money {
            amount: d("300"),
            currency: Currency::eur(),
        }),
    );
    let proj_mixed_ccy = make_projection(vec![p_aapl.clone(), p_msft_eur]);
    let items_mixed = watch_items(
        &proj_mixed_ccy,
        &matches,
        std::slice::from_ref(&ev_aapl),
        as_of,
    );
    assert_eq!(items_mixed.len(), 1);
    assert_eq!(
        items_mixed[0].exposure.share_of_owned_value, None,
        "mixed currencies must yield None for share_of_owned_value"
    );

    // 3. Missing broker market value on one position -> share_of_owned_value must be None
    let p_msft_no_val = make_position("MSFT.US", d("5"), None);
    let proj_missing_val = make_projection(vec![p_aapl.clone(), p_msft_no_val]);
    let items_missing = watch_items(&proj_missing_val, &matches, &[ev_aapl], as_of);
    assert_eq!(items_missing.len(), 1);
    assert_eq!(
        items_missing[0].exposure.share_of_owned_value, None,
        "missing broker_market_value on any owned position must yield None"
    );
}

#[test]
fn determinism_and_sorting_across_shuffled_inputs() {
    let t1 = Utc.with_ymd_and_hms(2025, 2, 1, 10, 0, 0).unwrap();
    let t2 = Utc.with_ymd_and_hms(2025, 2, 2, 10, 0, 0).unwrap();
    let t3 = Utc.with_ymd_and_hms(2025, 2, 3, 10, 0, 0).unwrap();
    let as_of = AsOf {
        occurred_by: t3,
        known_by: t3,
    };

    let p_aapl = make_position(
        "AAPL.US",
        d("10"),
        Some(Money {
            amount: d("100"),
            currency: usd(),
        }),
    );
    let p_msft = make_position(
        "MSFT.US",
        d("5"),
        Some(Money {
            amount: d("100"),
            currency: usd(),
        }),
    );

    let matches = vec![
        (
            p_aapl.instrument.key(),
            IssuerMatch::Matched {
                cik: Cik::new(320193).unwrap(),
                ticker: "AAPL".into(),
                title: "Apple Inc.".into(),
                map_fetched_at: t1,
            },
        ),
        (
            p_msft.instrument.key(),
            IssuerMatch::Matched {
                cik: Cik::new(789019).unwrap(),
                ticker: "MSFT".into(),
                title: "Microsoft Corp".into(),
                map_fetched_at: t1,
            },
        ),
    ];

    let ev1 = make_evidence(
        "ev1",
        "0000320193-25-000001",
        320193,
        "10-K",
        vec![],
        t1,
        t1,
        t1,
    );
    let ev2 = make_evidence(
        "ev2",
        "0000789019-25-000002",
        789019,
        "10-Q",
        vec![],
        t2,
        t2,
        t2,
    );
    let ev3 = make_evidence(
        "ev3",
        "0000320193-25-000003",
        320193,
        "8-K",
        vec!["2.02"],
        t3,
        t3,
        t3,
    );

    let proj1 = make_projection(vec![p_aapl.clone(), p_msft.clone()]);
    let evidence_order_a = vec![ev1.clone(), ev2.clone(), ev3.clone()];
    let res_a = watch_items(&proj1, &matches, &evidence_order_a, as_of);

    // Verify ordering: published_at desc: ev3 (t3), then ev2 (t2), then ev1 (t1)
    assert_eq!(res_a.len(), 3);
    assert_eq!(res_a[0].evidence_id, "ev3");
    assert_eq!(res_a[1].evidence_id, "ev2");
    assert_eq!(res_a[2].evidence_id, "ev1");

    // Shuffled input order
    let proj2 = make_projection(vec![p_msft, p_aapl]);
    let evidence_order_b = vec![ev2, ev1, ev3];
    let res_b = watch_items(&proj2, &matches, &evidence_order_b, as_of);

    assert_eq!(res_a, res_b, "shuffled input must yield identical output");
}
