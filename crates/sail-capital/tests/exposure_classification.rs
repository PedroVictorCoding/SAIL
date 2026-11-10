//! Tests for SEC filing classification rules.

use chrono::{NaiveDate, Utc};
use sail_capital::evidence::{Cik, Filing, PublishedBasis, ResearchAction};
use sail_capital::exposure::classify;

fn make_filing(form: &str, items: Vec<&str>) -> Filing {
    Filing {
        accession: "0000320193-24-000123".into(),
        cik: Cik::new(320193).expect("valid cik"),
        form: form.into(),
        filing_date: NaiveDate::from_ymd_opt(2025, 2, 10).expect("valid date"),
        published_at: Utc::now(),
        published_basis: PublishedBasis::Acceptance,
        report_date: Some(NaiveDate::from_ymd_opt(2024, 12, 31).expect("valid date")),
        items: items.into_iter().map(String::from).collect(),
        primary_document: Some("doc.htm".into()),
        description: Some("Test filing".into()),
        url: "https://www.sec.gov/Archives/edgar/data/320193/000032019324000123/doc.htm".into(),
    }
}

#[test]
fn classifies_annual_reports() {
    for form in ["10-K", "20-F", "40-F", "10-k"] {
        let f = make_filing(form, vec![]);
        let (action, reason) = classify(&f).expect("classified");
        assert_eq!(action, ResearchAction::ResearchProposal);
        assert_eq!(
            reason,
            format!("Annual report ({})", form.to_ascii_uppercase())
        );
    }
}

#[test]
fn classifies_quarterly_report() {
    let f = make_filing("10-Q", vec![]);
    let (action, reason) = classify(&f).expect("classified");
    assert_eq!(action, ResearchAction::ResearchProposal);
    assert_eq!(reason, "Quarterly report (10-Q)");
}

#[test]
fn classifies_8k_with_material_items_as_proposal() {
    let f = make_filing("8-K", vec!["2.02", "9.01"]);
    let (action, reason) = classify(&f).expect("classified");
    assert_eq!(action, ResearchAction::ResearchProposal);
    assert_eq!(reason, "Current report: 2.02 results of operations");
}

#[test]
fn classifies_8k_without_material_items_as_watch() {
    let f = make_filing("8-K", vec!["7.01", "9.01"]);
    let (action, reason) = classify(&f).expect("classified");
    assert_eq!(action, ResearchAction::Watch);
    assert_eq!(reason, "Current report (8-K: 7.01, 9.01)");

    let f_empty = make_filing("8-K", vec![]);
    let (action_empty, reason_empty) = classify(&f_empty).expect("classified");
    assert_eq!(action_empty, ResearchAction::Watch);
    assert_eq!(reason_empty, "Current report (8-K)");
}

#[test]
fn classifies_6k_as_watch() {
    let f = make_filing("6-K", vec![]);
    let (action, reason) = classify(&f).expect("classified");
    assert_eq!(action, ResearchAction::Watch);
    assert_eq!(reason, "Current report (6-K)");
}

#[test]
fn classifies_insider_and_large_holder_reports() {
    let f4 = make_filing("4", vec![]);
    let (act4, reason4) = classify(&f4).expect("classified");
    assert_eq!(act4, ResearchAction::Watch);
    assert_eq!(reason4, "Insider transaction report (Form 4)");

    for form in [
        "SC 13D",
        "SC 13G",
        "13D",
        "13G",
        "SCHEDULE 13D",
        "SCHEDULE 13G",
    ] {
        let f = make_filing(form, vec![]);
        let (act, reason) = classify(&f).expect("classified");
        assert_eq!(act, ResearchAction::Watch);
        assert_eq!(reason, format!("Beneficial ownership report ({form})"));
    }
}

#[test]
fn classifies_proxy_and_registration_statements() {
    for form in ["DEF 14A", "S-1", "S-3", "424B2", "424B5"] {
        let f = make_filing(form, vec![]);
        let (act, reason) = classify(&f).expect("classified");
        assert_eq!(act, ResearchAction::Watch);
        assert_eq!(reason, "Proxy statement / registration");
    }
}

#[test]
fn classifies_amendments() {
    // 10-K/A: keeps proposal action, appends (amended)
    let f_10k_a = make_filing("10-K/A", vec![]);
    let (act_10k_a, reason_10k_a) = classify(&f_10k_a).expect("classified");
    assert_eq!(act_10k_a, ResearchAction::ResearchProposal);
    assert_eq!(reason_10k_a, "Annual report (10-K) (amended)");

    // 8-K/A with material item 2.02: MUST stay at Watch!
    let f_8k_a = make_filing("8-K/A", vec!["2.02"]);
    let (act_8k_a, reason_8k_a) = classify(&f_8k_a).expect("classified");
    assert_eq!(
        act_8k_a,
        ResearchAction::Watch,
        "8-K/A must never exceed Watch"
    );
    assert_eq!(
        reason_8k_a,
        "Current report: 2.02 results of operations (amended)"
    );

    // Form 4/A
    let f_4a = make_filing("4/A", vec![]);
    let (act_4a, reason_4a) = classify(&f_4a).expect("classified");
    assert_eq!(act_4a, ResearchAction::Watch);
    assert_eq!(reason_4a, "Insider transaction report (Form 4) (amended)");
}

#[test]
fn unclassified_forms_return_none() {
    for form in ["144", "SD", "8-A12B", "POS AM", "UNKNOWN"] {
        let f = make_filing(form, vec![]);
        assert!(classify(&f).is_none(), "expected None for form {form}");
    }
}
