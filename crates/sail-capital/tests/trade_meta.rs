//! Round 25 A8: the meta-labeling features, the fit gate and the shrink-only policy.

mod trade_support;

use std::collections::BTreeMap;

use sail_capital::trade::meta::{
    MIN_SIZE_FACTOR, MIN_TRAIN_TRADES, MetaModel, MetaReport, apply, features, fit,
};
use sail_capital::trade::regime::Condition;
use sail_capital::trade::{AssetClass, Bar, SetupId, Side};
use trade_support::{card, date_at, instrument, random_walk};

fn bar(index: usize, open: f64, high: f64, low: f64, close: f64, volume: f64) -> Bar {
    Bar {
        session: date_at(index),
        open,
        high,
        low,
        close,
        volume: Some(volume),
        dividend: None,
    }
}

/// 281 bars with a volume spike and a gap on the last one.
fn fired_instrument() -> sail_capital::trade::Instrument {
    let mut bars: Vec<Bar> = (0..280)
        .map(|index| bar(index, 100.0, 100.5, 99.5, 100.0, 1_000.0))
        .collect();
    bars.push(bar(280, 103.0, 104.0, 102.5, 103.5, 3_000.0));
    instrument("X", "X.US", AssetClass::Stock, false, bars)
}

#[test]
fn features_read_the_firing_context() {
    let universe = fired_instrument();
    let first = features(&universe, 280, SetupId::VolumeBreakout, Side::Long, true)
        .expect("features at 281 bars");
    assert_eq!(first.setup, SetupId::VolumeBreakout);
    assert_eq!(first.side, Side::Long);
    assert!(first.agrees_calm);
    assert_eq!(first.relative_volume, Some(3.0));
    assert!((0.0..=1.0).contains(&first.atr_percentile));
    assert!(first.distance_200d_atr > 0.0);
    assert!(first.gap_atr > 0.0);
    assert_eq!(
        first,
        features(&universe, 280, SetupId::VolumeBreakout, Side::Long, true).expect("again")
    );
}

#[test]
fn features_need_enough_history() {
    let mut bars: Vec<Bar> = (0..100)
        .map(|index| bar(index, 100.0, 100.5, 99.5, 100.0, 1_000.0))
        .collect();
    bars[99].volume = Some(5_000.0);
    let universe = instrument("X", "X.US", AssetClass::Stock, false, bars);
    assert!(features(&universe, 99, SetupId::VolumeBreakout, Side::Long, false).is_none());
    assert!(features(&universe, 1000, SetupId::VolumeBreakout, Side::Long, false).is_none());
}

#[test]
fn a_future_bar_does_not_change_an_earlier_feature_vector() {
    let mut bars: Vec<Bar> = (0..280)
        .map(|index| bar(index, 100.0, 100.5, 99.5, 100.0, 1_000.0))
        .collect();
    bars.push(bar(280, 103.0, 104.0, 102.5, 103.5, 3_000.0));
    let before = features(
        &instrument("X", "X.US", AssetClass::Stock, false, bars.clone()),
        280,
        SetupId::VolumeBreakout,
        Side::Long,
        true,
    )
    .expect("features");
    for index in 281..300 {
        bars.push(bar(index, 90.0, 95.0, 85.0, 90.0, 9_000.0));
    }
    let after = features(
        &instrument("X", "X.US", AssetClass::Stock, false, bars),
        280,
        SetupId::VolumeBreakout,
        Side::Long,
        true,
    )
    .expect("features");
    assert_eq!(before, after);
}

#[test]
fn a_small_replay_gets_no_model() {
    let universe = vec![fired_instrument()];
    let pooled: BTreeMap<String, Vec<_>> = BTreeMap::new();
    let report = fit(&universe, &pooled);
    assert_eq!(report.n, 0);
    assert!(report.model.is_none());
    assert!(report.notes.iter().any(|note| note.contains("400")));
    assert!(report.brier.is_none());
    assert!(report.base_brier.is_none());
    assert_eq!(report, fit(&universe, &pooled));
}

#[test]
fn the_meta_fit_is_deterministic() {
    let universe: Vec<_> = (0..3)
        .map(|index| {
            let (walk, _) = random_walk(900, 0.0002, 0.012, 100 + index);
            let mut bars = trade_support::bars_with_prior_open(&walk, 0.4);
            for bar in &mut bars {
                bar.volume = Some(1_000_000.0);
            }
            instrument(
                &format!("M{index}"),
                &format!("M{index}.US"),
                AssetClass::Stock,
                false,
                bars,
            )
        })
        .collect();
    let mut pooled: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for instrument in &universe {
        pooled
            .entry("stocks".to_string())
            .or_default()
            .extend(sail_capital::trade::replay(
                instrument,
                sail_capital::trade::Costs::default_for(instrument.class),
            ));
    }
    assert_eq!(fit(&universe, &pooled), fit(&universe, &pooled));
}

fn model() -> MetaModel {
    // The encoded features are 8 setup slots, 2 side slots, `agrees_calm`, then the
    // numerics. A coefficient on `agrees_calm` makes an agreeing card look better.
    let mut coefficients = vec![0.0; 16];
    coefficients[10] = 1.0;
    MetaModel {
        features: (0..16).map(|index| format!("f{index}")).collect(),
        coefficients,
        intercept: 0.0,
        scaling: vec![(0.0, 1.0); 16],
    }
}

fn report(model: Option<MetaModel>) -> MetaReport {
    MetaReport {
        model,
        n: MIN_TRAIN_TRADES as u32,
        base_rate: 0.5,
        brier: Some(0.20),
        base_brier: Some(0.25),
        brier_diff_se: Some(0.01),
        notes: Vec::new(),
    }
}

#[test]
fn apply_shrinks_and_re_ranks_but_never_adds_or_drops() {
    let universe = vec![fired_instrument()];
    let mut cards = vec![
        card(
            Side::Long,
            SetupId::VolumeBreakout,
            100.0,
            95.0,
            [105.0; 3],
            5,
            false,
        ),
        card(
            Side::Long,
            SetupId::VolumeBreakout,
            100.0,
            95.0,
            [105.0; 3],
            5,
            false,
        ),
    ];
    cards[0].key = "X".into();
    cards[0].id = "calm".into();
    cards[0].signal_session = date_at(280);
    cards[0].verdict.condition = Condition::AgreesCalm;
    cards[1].key = "X".into();
    cards[1].id = "plain".into();
    cards[1].signal_session = date_at(280);
    cards[1].verdict.condition = Condition::None;
    let mut candidates = vec![card(
        Side::Long,
        SetupId::VolumeBreakout,
        100.0,
        95.0,
        [105.0; 3],
        5,
        false,
    )];
    candidates[0].key = "X".into();
    candidates[0].id = "candidate".into();
    candidates[0].signal_session = date_at(280);
    candidates[0].verdict.condition = Condition::AgreesCalm;

    let report = report(Some(model()));
    apply(&report, &universe, &mut cards, &mut candidates);
    assert_eq!(cards.len(), 2);
    assert_eq!(candidates.len(), 1);
    assert_eq!(cards[0].id, "calm");
    assert!(cards[0].score >= cards[1].score);
    for card in &cards {
        let meta = card.meta.as_ref().expect("a meta view");
        assert!((MIN_SIZE_FACTOR..=1.0).contains(&meta.size_factor));
        assert!(meta.size_factor <= 1.0);
        assert!(card.risk_pct <= 0.5);
    }
    let calm = cards[0].meta.as_ref().expect("calm meta");
    let plain = cards[1].meta.as_ref().expect("plain meta");
    assert!(calm.p_hit > plain.p_hit);
    assert!(calm.size_factor > plain.size_factor);
    assert!(candidates[0].meta.is_some());
    assert_eq!(calm.base_rate, 0.5);
}

#[test]
fn no_model_leaves_the_cards_alone() {
    let universe = vec![fired_instrument()];
    let mut cards = vec![card(
        Side::Long,
        SetupId::VolumeBreakout,
        100.0,
        95.0,
        [105.0; 3],
        5,
        false,
    )];
    cards[0].key = "X".into();
    cards[0].signal_session = date_at(280);
    cards[0].verdict.condition = Condition::AgreesCalm;
    let before = cards.clone();
    apply(&report(None), &universe, &mut cards, &mut []);
    assert_eq!(cards, before);
}
