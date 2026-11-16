//! Round 25 end to end for Kilo's slice: the short signal horizon and the evidence-only
//! research universe (A5), meta-labeling (A8) and the dividend projection record (B8).
//! Red until that slice lands.

mod trade_support;

use std::collections::{BTreeMap, BTreeSet};

use chrono::{NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use sail_capital::dividends::record::{self, Receipt, SnapshotHolding};
use sail_capital::forecast::CloseSeries;
use sail_capital::signals::{
    SHORT_HORIZON, SIGNAL_HORIZONS, SignalEvalConfig, SignalInputs, evaluate_with,
};
use sail_capital::trade::{AssetClass, Costs, Instrument, group_of, meta, replay};
use trade_support::{bars_with_prior_open, instrument, random_walk};

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn closes(seed: u64) -> Vec<(NaiveDate, Decimal)> {
    let (walk, _) = random_walk(900, 0.0002, 0.012, seed);
    let mut day = d(2022, 1, 3);
    let mut out = Vec::new();
    for close in walk {
        out.push((day, Decimal::from_f64(close).unwrap()));
        day = day.succ_opt().unwrap();
        while matches!(day.format("%a").to_string().as_str(), "Sat" | "Sun") {
            day = day.succ_opt().unwrap();
        }
    }
    out
}

#[test]
fn signals_score_a_five_session_horizon_and_research_names_stay_evidence() {
    assert!(SIGNAL_HORIZONS.contains(&SHORT_HORIZON));
    type Row = (String, String, Vec<(NaiveDate, Decimal)>);
    let data: Vec<Row> = (0..12)
        .map(|i| (format!("k{i}"), format!("S{i}.US"), closes(10 + i)))
        .collect();
    let series: Vec<CloseSeries<'_>> = data
        .iter()
        .map(|(k, s, c)| CloseSeries {
            instrument_key: k,
            symbol: s,
            closes: c,
        })
        .collect();
    let evidence: BTreeSet<String> = (6..12).map(|i| format!("k{i}")).collect();
    // Research names joined the index on 2023-01-02; before that they don't count.
    let joined = d(2023, 1, 2);
    let member_on = |key: &str, day: NaiveDate| !evidence.contains(key) || day >= joined;
    let config = SignalEvalConfig::default();
    let report = evaluate_with(&SignalInputs {
        series: &series,
        external: &[],
        sigma: &|_, _| Some(0.05),
        config: &config,
        evidence_only: &evidence,
        member_on: &member_on,
    });
    assert_eq!(report.evidence_instruments, 6);
    assert!(
        report
            .latest
            .iter()
            .all(|s| !evidence.contains(&s.instrument_key))
    );
    assert!(
        report
            .skill
            .iter()
            .any(|s| s.horizon_sessions == SHORT_HORIZON && s.origins > 0)
    );
    // Membership is honoured: with every research name filtered out until 2023, the
    // short-horizon skill has fewer origins than with them always members.
    let always = |_: &str, _: NaiveDate| true;
    let unfiltered = evaluate_with(&SignalInputs {
        member_on: &always,
        series: &series,
        external: &[],
        sigma: &|_, _| Some(0.05),
        config: &config,
        evidence_only: &evidence,
    });
    let origins = |r: &sail_capital::signals::SignalReport| {
        r.skill
            .iter()
            .filter(|s| s.horizon_sessions == SHORT_HORIZON)
            .map(|s| s.origins)
            .max()
            .unwrap_or(0)
    };
    assert!(origins(&report) <= origins(&unfiltered));
    assert!(
        report.notes.iter().any(|n| n.contains("evidence")),
        "the notes say research names are evidence only"
    );
}

fn universe() -> Vec<Instrument> {
    (0..10)
        .map(|i| {
            let (walk, _) = random_walk(2000, 0.0002, 0.013, 300 + i);
            let mut bars = bars_with_prior_open(&walk, 0.45);
            for (j, bar) in bars.iter_mut().enumerate() {
                bar.volume = Some(1_000_000.0 * (1.0 + (j % 9) as f64 / 8.0));
            }
            instrument(
                &format!("M{i}"),
                &format!("M{i}.US"),
                AssetClass::Stock,
                false,
                bars,
            )
        })
        .collect()
}

#[test]
fn meta_labeling_is_fitted_and_compared_with_the_base_rate() {
    let universe = universe();
    let mut pooled: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for inst in &universe {
        pooled
            .entry(group_of(inst.class).to_string())
            .or_default()
            .extend(replay(inst, Costs::default_for(inst.class)));
    }
    let total: usize = pooled.values().map(Vec::len).sum();
    assert!(
        total >= meta::MIN_TRAIN_TRADES,
        "fixture has {total} trades"
    );
    let report = meta::fit(&universe, &pooled);
    assert!(report.n as usize >= meta::MIN_TRAIN_TRADES);
    assert!(report.base_rate > 0.0 && report.base_rate < 1.0);
    assert!(report.brier.is_some() && report.base_brier.is_some());
    // A random walk carries no edge: the model must not earn weight on it.
    assert!(report.model.is_none(), "{:?}", report.notes);
    let first = &universe[0];
    let features = meta::features(
        first,
        first.bars.len() - 1,
        sail_capital::trade::SetupId::ALL[0],
        sail_capital::trade::Side::Long,
        false,
    )
    .expect("features at the last bar of 2000");
    assert!((0.0..=1.0).contains(&features.atr_percentile));
}

#[test]
fn the_projection_record_grades_a_year_of_receipts() {
    let snap = record::snapshot(
        d(2025, 1, 1),
        Utc.with_ymd_and_hms(2025, 1, 2, 9, 0, 0).unwrap(),
        "EUR",
        vec![SnapshotHolding {
            key: "A".into(),
            symbol: "A.US".into(),
            units: 10.0,
            projected_net: 100.0,
        }],
        80.0,
        100.0,
        120.0,
        Some((85.0, 115.0)),
    );
    let receipts = vec![
        Receipt {
            key: "A".into(),
            paid: d(2025, 3, 10),
            net: 30.0,
            units_then: 10.0,
        },
        Receipt {
            key: "A".into(),
            paid: d(2025, 6, 10),
            net: 30.0,
            units_then: 10.0,
        },
        // Bought more: twice the units, so it counts half.
        Receipt {
            key: "A".into(),
            paid: d(2025, 9, 10),
            net: 60.0,
            units_then: 20.0,
        },
        Receipt {
            key: "A".into(),
            paid: d(2025, 12, 10),
            net: 30.0,
            units_then: 10.0,
        },
        // After the 12 months: not counted.
        Receipt {
            key: "A".into(),
            paid: d(2026, 1, 5),
            net: 30.0,
            units_then: 10.0,
        },
        // Another holding: not in the snapshot.
        Receipt {
            key: "B".into(),
            paid: d(2025, 5, 1),
            net: 999.0,
            units_then: 1.0,
        },
    ];
    let early = record::grade(std::slice::from_ref(&snap), &receipts, d(2025, 12, 31));
    assert_eq!(early.pending, 1);
    assert!(early.graded.is_empty());

    let summary = record::grade(&[snap], &receipts, d(2026, 2, 1));
    assert_eq!(summary.pending, 0);
    assert_eq!(summary.graded.len(), 1);
    let graded = &summary.graded[0];
    assert!((graded.received - 120.0).abs() < 1e-9);
    assert!((graded.error.unwrap() - 0.2).abs() < 1e-9);
    assert!(graded.in_scenarios);
    assert_eq!(graded.in_band, Some(false));
    assert!((summary.bias.unwrap() - 0.2).abs() < 1e-9);
    assert_eq!(summary.band_coverage, Some(0.0));
}
