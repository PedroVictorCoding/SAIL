//! Round 25 A5: the research cross-section, its point-in-time membership and the
//! five-session horizon.

use std::collections::BTreeSet;

use chrono::{Days, NaiveDate};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use sail_capital::forecast::CloseSeries;
use sail_capital::signals::{
    SHORT_HORIZON, SIGNAL_HORIZONS, SignalEvalConfig, SignalId, SignalInputs, SignalReport,
    evaluate_and_combine, evaluate_with,
};

fn base_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2018, 1, 1).expect("a date")
}

fn date_at(index: usize) -> NaiveDate {
    base_date() + Days::new(index as u64)
}

struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self { state: seed | 1 }
    }

    fn uniform(&mut self) -> f64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.state >> 11) as f64 / (1u64 << 53) as f64
    }

    fn normal(&mut self) -> f64 {
        let first = self.uniform().max(f64::MIN_POSITIVE);
        let second = self.uniform();
        (-2.0 * first.ln()).sqrt() * (std::f64::consts::TAU * second).cos()
    }
}

fn planted(count: usize, length: usize) -> Vec<Vec<(NaiveDate, Decimal)>> {
    (0..count)
        .map(|index| {
            let drift = 0.0002 + 0.00008 * index as f64;
            let mut rng = Lcg::new(91 + index as u64 * 7919);
            let mut price = 100.0;
            (0..length)
                .map(|session| {
                    if session > 0 {
                        price *= (drift + 0.01 * rng.normal()).exp();
                    }
                    (
                        date_at(session),
                        Decimal::from_f64(price).expect("a decimal"),
                    )
                })
                .collect()
        })
        .collect()
}

fn config() -> SignalEvalConfig {
    SignalEvalConfig {
        horizons: vec![5, 20, 60],
        min_origins: 12,
        min_effective_origins: 2.0,
        ..SignalEvalConfig::default()
    }
}

fn sigma(_key: &str, _horizon: u32) -> Option<f64> {
    Some(0.2)
}

fn origins(report: &SignalReport) -> u32 {
    report
        .skill
        .iter()
        .filter(|skill| skill.horizon_sessions == SHORT_HORIZON)
        .map(|skill| skill.origins)
        .max()
        .unwrap_or(0)
}

#[test]
fn the_default_horizons_include_five_sessions() {
    assert_eq!(SHORT_HORIZON, 5);
    assert_eq!(SIGNAL_HORIZONS, [5, 20, 60]);
    assert_eq!(SignalEvalConfig::default().horizons, vec![5, 20, 60]);
}

#[test]
fn research_names_add_breadth_but_keep_their_scores_private() {
    let rows = planted(80, 320);
    let names: Vec<String> = (0..80).map(|index| format!("S{index:02}")).collect();
    let series: Vec<CloseSeries<'_>> = rows
        .iter()
        .enumerate()
        .map(|(index, closes)| CloseSeries {
            instrument_key: &names[index],
            symbol: &names[index],
            closes,
        })
        .collect();
    let research: BTreeSet<String> = names[20..].iter().cloned().collect();
    let always = |_: &str, _: NaiveDate| true;
    let config = config();
    let report = evaluate_with(&SignalInputs {
        series: &series,
        external: &[],
        sigma: &sigma,
        config: &config,
        evidence_only: &research,
        member_on: &always,
    });

    assert_eq!(report.evidence_instruments, 60);
    assert!(
        report
            .latest
            .iter()
            .all(|score| !research.contains(&score.instrument_key))
    );
    assert!(
        report
            .latest
            .iter()
            .any(|score| !research.contains(&score.instrument_key))
    );
    assert!(report.notes.iter().any(|note| note.contains("evidence")));
    for horizon in [5, 20, 60] {
        assert!(
            report
                .skill
                .iter()
                .any(|skill| skill.horizon_sessions == horizon),
            "no skill row at {horizon}"
        );
        assert!(
            report
                .combined_ic
                .iter()
                .any(|(value, _)| *value == horizon)
        );
    }
    let five = report
        .skill
        .iter()
        .find(|skill| skill.horizon_sessions == 5 && skill.signal == SignalId::Momentum12_1)
        .expect("a five-session momentum row");
    assert!(five.origins > 0);
    assert_eq!(
        report,
        evaluate_with(&SignalInputs {
            series: &series,
            external: &[],
            sigma: &sigma,
            config: &config,
            evidence_only: &research,
            member_on: &always,
        })
    );
}

#[test]
fn membership_removes_research_history_from_earlier_cross_sections() {
    let rows = planted(60, 320);
    let names: Vec<String> = (0..60).map(|index| format!("S{index:02}")).collect();
    let series: Vec<CloseSeries<'_>> = rows
        .iter()
        .enumerate()
        .map(|(index, closes)| CloseSeries {
            instrument_key: &names[index],
            symbol: &names[index],
            closes,
        })
        .collect();
    let research: BTreeSet<String> = names[10..].iter().cloned().collect();
    let joined = date_at(180);
    let member_on = |key: &str, day: NaiveDate| !research.contains(key) || day >= joined;
    let config = config();
    let filtered = evaluate_with(&SignalInputs {
        series: &series,
        external: &[],
        sigma: &sigma,
        config: &config,
        evidence_only: &research,
        member_on: &member_on,
    });
    let always = |_: &str, _: NaiveDate| true;
    let unfiltered = evaluate_with(&SignalInputs {
        series: &series,
        external: &[],
        sigma: &sigma,
        config: &config,
        evidence_only: &research,
        member_on: &always,
    });
    assert!(origins(&filtered) <= origins(&unfiltered));
    assert!(filtered.evidence_instruments == 50);
}

#[test]
fn the_unmarked_entry_point_is_unchanged() {
    let rows = planted(16, 320);
    let names: Vec<String> = (0..16).map(|index| format!("S{index:02}")).collect();
    let series: Vec<CloseSeries<'_>> = rows
        .iter()
        .enumerate()
        .map(|(index, closes)| CloseSeries {
            instrument_key: &names[index],
            symbol: &names[index],
            closes,
        })
        .collect();
    let config = config();
    let always = |_: &str, _: NaiveDate| true;
    let marked = evaluate_with(&SignalInputs {
        series: &series,
        external: &[],
        sigma: &sigma,
        config: &config,
        evidence_only: &BTreeSet::new(),
        member_on: &always,
    });
    assert_eq!(evaluate_and_combine(&series, &[], &sigma, &config), marked);
}
