//! Round 24 K4: portfolio heat across the day's cards and the still-open ones.

mod trade_support;

use std::collections::HashMap;

use chrono::NaiveDate;
use sail_capital::trade::{
    AssetClass, SetupId, Side, SignalCard,
    heat::{CorrInputs, HeatLimits, OpenCard, cap},
};
use trade_support::{card, date_at};

fn heat_card(key: &str, side: Side, risk_pct: f64, score: f64) -> SignalCard {
    let mut card = card(
        side,
        SetupId::ThreeDayReversal,
        100.0,
        95.0,
        [105.0, 110.0, 115.0],
        5,
        false,
    );
    card.key = key.into();
    card.symbol = key.into();
    card.class = AssetClass::Stock;
    card.risk_pct = risk_pct;
    card.score = score;
    card
}

fn returns(keys: &[&str], correlated: bool) -> CorrInputs {
    let mut map: HashMap<String, Vec<(NaiveDate, f64)>> = HashMap::new();
    for (index, key) in keys.iter().enumerate() {
        let spike = if correlated { 0 } else { index * 5 };
        map.insert(
            (*key).to_string(),
            (0..60)
                .map(|session| (date_at(session), if session == spike { 0.01 } else { 0.0 }))
                .collect(),
        );
    }
    CorrInputs { returns: map }
}

#[test]
fn eight_perfectly_correlated_longs_stay_inside_the_cluster_limit() {
    let keys: Vec<String> = (0..8).map(|index| format!("S{index}")).collect();
    let names: Vec<&str> = keys.iter().map(String::as_str).collect();
    let cards: Vec<SignalCard> = keys
        .iter()
        .enumerate()
        .map(|(index, key)| heat_card(key, Side::Long, 0.5, 8.0 - index as f64))
        .collect();
    let outcome = cap(
        cards,
        Vec::new(),
        &[],
        &returns(&names, true),
        HeatLimits::default(),
    );
    assert_eq!(outcome.cards.len(), 3);
    assert_eq!(outcome.suppressed.len(), 5);
    assert!((outcome.summary.total_risk_pct - 1.5).abs() < 1e-12);
    assert!(outcome.summary.total_risk_pct <= outcome.summary.cluster_limit_pct + 1e-9);
    assert!(
        outcome
            .suppressed
            .iter()
            .all(|(_, reason)| reason.starts_with("portfolio heat: "))
    );
    assert_eq!(outcome.summary.clusters.len(), 1);
    assert_eq!(outcome.summary.clusters[0].keys.len(), 3);
    assert!((outcome.summary.clusters[0].risk_pct - 1.5).abs() < 1e-12);
}

#[test]
fn eight_uncorrelated_longs_stay_inside_the_total_limit() {
    let keys: Vec<String> = (0..8).map(|index| format!("S{index}")).collect();
    let names: Vec<&str> = keys.iter().map(String::as_str).collect();
    let cards: Vec<SignalCard> = keys
        .iter()
        .enumerate()
        .map(|(index, key)| heat_card(key, Side::Long, 0.5, 8.0 - index as f64))
        .collect();
    let outcome = cap(
        cards,
        Vec::new(),
        &[],
        &returns(&names, false),
        HeatLimits::default(),
    );
    assert_eq!(outcome.cards.len(), 6);
    assert_eq!(outcome.suppressed.len(), 2);
    assert!((outcome.summary.total_risk_pct - 3.0).abs() < 1e-12);
}

#[test]
fn admitted_cards_keep_their_score_order() {
    let keys = ["S0", "S1", "S2", "S3"];
    let cards = vec![
        heat_card("S0", Side::Long, 0.5, 4.0),
        heat_card("S1", Side::Long, 0.5, 3.0),
        heat_card("S2", Side::Long, 0.5, 2.0),
        heat_card("S3", Side::Long, 0.5, 1.0),
    ];
    let outcome = cap(
        cards,
        Vec::new(),
        &[],
        &returns(&keys, true),
        HeatLimits::default(),
    );
    let scores: Vec<f64> = outcome.cards.iter().map(|card| card.score).collect();
    assert_eq!(scores, vec![4.0, 3.0, 2.0]);
}

#[test]
fn an_open_card_counts_towards_its_cluster() {
    let keys = ["S0", "S1"];
    let open = vec![OpenCard {
        key: "S0".into(),
        side: Side::Long,
        risk_pct: 1.4,
    }];
    let cards = vec![
        heat_card("S0", Side::Long, 0.5, 2.0),
        heat_card("S1", Side::Long, 0.5, 1.0),
    ];
    let outcome = cap(
        cards,
        Vec::new(),
        &open,
        &returns(&keys, false),
        HeatLimits::default(),
    );
    assert_eq!(outcome.cards.len(), 1);
    assert_eq!(outcome.cards[0].key, "S1");
    assert!(outcome.suppressed[0].1.contains("cluster"));
    assert!((outcome.summary.total_risk_pct - 1.9).abs() < 1e-12);
}

#[test]
fn a_short_offsets_a_long_in_the_same_cluster() {
    let keys = ["S0"];
    let open = vec![OpenCard {
        key: "S0".into(),
        side: Side::Long,
        risk_pct: 1.4,
    }];
    let cards = vec![heat_card("S0", Side::Short, 0.5, 1.0)];
    let outcome = cap(
        cards,
        Vec::new(),
        &open,
        &returns(&keys, true),
        HeatLimits::default(),
    );
    assert_eq!(outcome.cards.len(), 1, "{:?}", outcome.suppressed);
    assert!((outcome.summary.total_risk_pct - 1.9).abs() < 1e-12);
    assert!((outcome.summary.clusters[0].risk_pct - 0.9).abs() < 1e-12);
}

#[test]
fn verified_cards_are_admitted_before_candidates() {
    let keys = ["S0", "S1"];
    let open = vec![OpenCard {
        key: "S2".into(),
        side: Side::Long,
        risk_pct: 2.5,
    }];
    let cards = vec![heat_card("S0", Side::Long, 0.5, 1.0)];
    let candidates = vec![heat_card("S1", Side::Long, 0.5, 0.0)];
    let mut all = keys.to_vec();
    all.push("S2");
    let outcome = cap(
        cards,
        candidates,
        &open,
        &returns(&all, false),
        HeatLimits::default(),
    );
    assert_eq!(outcome.cards.len(), 1);
    assert!(outcome.candidates.is_empty());
    assert_eq!(outcome.suppressed.len(), 1);
    assert!((outcome.summary.total_risk_pct - 3.0).abs() < 1e-12);
}

#[test]
fn a_cluster_without_history_is_its_own_cluster() {
    let cards = vec![
        heat_card("S0", Side::Long, 0.5, 2.0),
        heat_card("S1", Side::Long, 0.5, 1.0),
    ];
    let outcome = cap(
        cards,
        Vec::new(),
        &[],
        &CorrInputs::default(),
        HeatLimits::default(),
    );
    assert_eq!(outcome.cards.len(), 2);
    assert_eq!(outcome.summary.clusters.len(), 2);
    assert!(
        outcome
            .summary
            .clusters
            .iter()
            .all(|cluster| cluster.keys.len() == 1)
    );
}

#[test]
fn heat_cap_is_pure() {
    let keys = ["S0", "S1"];
    let cards = vec![
        heat_card("S0", Side::Long, 0.5, 2.0),
        heat_card("S1", Side::Long, 0.5, 1.0),
    ];
    let inputs = returns(&keys, false);
    let first = cap(
        cards.clone(),
        Vec::new(),
        &[],
        &inputs,
        HeatLimits::default(),
    );
    let second = cap(cards, Vec::new(), &[], &inputs, HeatLimits::default());
    assert_eq!(first, second);
}
