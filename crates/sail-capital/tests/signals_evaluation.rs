//! Signal scoring and combination (Kilo's slice, round 11): hand-computed formulae,
//! planted skill, pure noise, and no look-ahead. Synthetic series only, no files and no
//! network.

use chrono::{Days, NaiveDate};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use sail_capital::forecast::CloseSeries;
use sail_capital::signals::{
    ExternalScores, SignalEvalConfig, SignalId, SignalReport, evaluate_and_combine,
};

fn base_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2018, 1, 1).expect("valid date")
}

fn date_at(index: usize) -> NaiveDate {
    base_date() + Days::new(index as u64)
}

fn decimal(value: f64) -> Decimal {
    Decimal::from_f64(value).expect("finite decimal")
}

struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self { state: seed | 1 }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn normal(&mut self) -> f64 {
        let first = self.uniform().max(f64::MIN_POSITIVE);
        let second = self.uniform();
        (-2.0 * first.ln()).sqrt() * (std::f64::consts::TAU * second).cos()
    }
}

fn random_walk(len: usize, drift: f64, sigma: f64, seed: u64) -> Vec<(NaiveDate, Decimal)> {
    let mut rng = Lcg::new(seed);
    let mut price = 100.0f64;
    let mut rows = Vec::with_capacity(len);
    for index in 0..len {
        if index > 0 {
            price *= (drift + sigma * rng.normal()).exp();
        }
        rows.push((date_at(index), decimal(price)));
    }
    rows
}

fn series<'a>(
    closes: &'a [(NaiveDate, Decimal)],
    key: &'a str,
    symbol: &'a str,
) -> CloseSeries<'a> {
    CloseSeries {
        instrument_key: key,
        symbol,
        closes,
    }
}

fn config(horizons: &[u32]) -> SignalEvalConfig {
    SignalEvalConfig {
        horizons: horizons.to_vec(),
        ..SignalEvalConfig::default()
    }
}

/// For the hand-computed formula check only: three cross-sections are judged.
fn formula_config(horizons: &[u32]) -> SignalEvalConfig {
    SignalEvalConfig {
        min_origins: 2,
        min_effective_origins: 0.0,
        ..config(horizons)
    }
}

fn sigma_constant(_key: &str, _horizon: u32) -> Option<f64> {
    Some(0.2)
}

fn drift_universe(
    drifts: &[f64],
    len: usize,
    sigma: f64,
    seed: u64,
) -> Vec<Vec<(NaiveDate, Decimal)>> {
    drifts
        .iter()
        .enumerate()
        .map(|(index, drift)| random_walk(len, *drift, sigma, seed + index as u64 * 7919))
        .collect()
}

fn report_for<'a>(
    rows: &'a [Vec<(NaiveDate, Decimal)>],
    symbols: &'a [String],
    config: &SignalEvalConfig,
) -> SignalReport {
    let series: Vec<CloseSeries<'a>> = rows
        .iter()
        .enumerate()
        .map(|(index, closes)| CloseSeries {
            instrument_key: &symbols[index],
            symbol: &symbols[index],
            closes,
        })
        .collect();
    evaluate_and_combine(&series, &[], &sigma_constant, config)
}

fn symbols(count: usize) -> Vec<String> {
    (0..count).map(|index| format!("SYM{index:02}")).collect()
}

fn skill_row(
    report: &SignalReport,
    signal: SignalId,
    horizon: u32,
) -> &sail_capital::signals::SignalSkill {
    report
        .skill
        .iter()
        .find(|row| row.signal == signal && row.horizon_sessions == horizon)
        .expect("a skill row")
}

/// Pure rank correlation of two equal-length vectors, written out here so the test does
/// not reuse the module under test.
fn rank_correlation(left: &[f64], right: &[f64]) -> f64 {
    let rank = |values: &[f64]| -> Vec<f64> {
        let mut order: Vec<usize> = (0..values.len()).collect();
        order.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
        let mut ranks = vec![0.0; values.len()];
        for (position, index) in order.iter().enumerate() {
            ranks[*index] = position as f64;
        }
        ranks
    };
    let left_ranks = rank(left);
    let right_ranks = rank(right);
    let count = left.len() as f64;
    let left_mean = left_ranks.iter().sum::<f64>() / count;
    let right_mean = right_ranks.iter().sum::<f64>() / count;
    let mut covariance = 0.0;
    let mut left_variance = 0.0;
    let mut right_variance = 0.0;
    for (left_rank, right_rank) in left_ranks.iter().zip(right_ranks.iter()) {
        covariance += (left_rank - left_mean) * (right_rank - right_mean);
        left_variance += (left_rank - left_mean).powi(2);
        right_variance += (right_rank - right_mean).powi(2);
    }
    covariance / (left_variance * right_variance).sqrt()
}

#[test]
fn momentum_survives_when_it_is_planted() {
    let drifts: Vec<f64> = (0..12)
        .map(|index| -0.0006 + 0.00016 * index as f64)
        .collect();
    let rows = drift_universe(&drifts, 1100, 0.002, 11);
    let names = symbols(12);
    let report = report_for(&rows, &names, &config(&[20, 60]));

    let row = skill_row(&report, SignalId::Momentum12_1, 60);
    assert!(row.origins >= 40, "{row:?}");
    assert!(row.ic_mean > 0.0, "{row:?}");
    assert!(row.ic_used > 0.0, "{row:?}");
    assert!(row.weight > 0.0, "{row:?}");
    assert!(row.t_stat > 2.0, "{row:?}");

    let mut alphas = Vec::new();
    let mut planted = Vec::new();
    for score in report
        .latest
        .iter()
        .filter(|score| score.horizon_sessions == 60)
    {
        let index = names
            .iter()
            .position(|name| name == &score.symbol)
            .expect("a known symbol");
        planted.push(drifts[index]);
        alphas.push(score.alpha);
    }
    let correlation = rank_correlation(&planted, &alphas);
    assert!(
        correlation > 0.8,
        "alpha should rank as planted, got {correlation}"
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("momentum 12-1"))
    );
}

#[test]
fn pure_noise_passes_nothing() {
    let drifts = vec![0.0; 12];
    let rows = drift_universe(&drifts, 900, 0.008, 7);
    let names = symbols(12);
    let report = report_for(&rows, &names, &config(&[20, 60]));

    for row in &report.skill {
        assert_eq!(row.ic_used, 0.0, "{row:?}");
        assert_eq!(row.weight, 0.0);
    }
    for (_, ic) in &report.combined_ic {
        assert_eq!(*ic, 0.0);
    }
    for score in &report.latest {
        assert_eq!(score.alpha, 0.0, "{score:?}");
    }
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("No signal passed validation")),
        "{:?}",
        report.notes
    );
}

#[test]
fn the_combination_matches_a_hand_computed_case() {
    let closes: Vec<Vec<(NaiveDate, Decimal)>> = (0..6)
        .map(|instrument| {
            let mut log_price = 0.0;
            let mut rows = vec![(date_at(0), decimal(100.0))];
            for session in 1..20 {
                let in_window = (1..=5).contains(&session)
                    || (8..=12).contains(&session)
                    || (15..=19).contains(&session);
                if in_window {
                    log_price += 0.001 + 0.01 * instrument as f64;
                }
                rows.push((date_at(session), decimal(100.0 * log_price.exp())));
            }
            rows
        })
        .collect();
    let names = symbols(6);
    let series: Vec<CloseSeries<'_>> = closes
        .iter()
        .enumerate()
        .map(|(index, rows)| series(rows, &names[index], &names[index]))
        .collect();
    let external = vec![
        ExternalScores {
            signal: SignalId::ChartPattern,
            values: names
                .iter()
                .enumerate()
                .flat_map(|(index, name)| {
                    (0..20).map(move |session| (name.clone(), date_at(session), index as f64))
                })
                .collect(),
        },
        ExternalScores {
            signal: SignalId::FilingSentiment,
            values: names
                .iter()
                .enumerate()
                .flat_map(|(index, name)| {
                    (0..20).map(move |session| (name.clone(), date_at(session), index as f64))
                })
                .collect(),
        },
    ];
    let report = evaluate_and_combine(&series, &external, &sigma_constant, &formula_config(&[5]));

    let z: Vec<f64> = (0..6)
        .map(|index| (index as f64 - 2.5) / 3.5f64.sqrt())
        .collect();
    let expected_weight: f64 = 0.1 / 0.21;
    let expected_ic = (2.0 * expected_weight).sqrt();
    assert!((expected_ic - 0.975_900_163).abs() < 1e-7, "{expected_ic}");

    for signal in [SignalId::ChartPattern, SignalId::FilingSentiment] {
        let row = skill_row(&report, signal, 5);
        assert_eq!(row.origins, 3, "{row:?}");
        assert!((row.ic_mean - 1.0).abs() < 1e-12, "{row:?}");
        assert!((row.ic_used - 1.0).abs() < 1e-12, "{row:?}");
        assert!(
            (row.weight - expected_weight).abs() < 1e-9,
            "{row:?} expected {expected_weight}"
        );
    }
    let combined_ic = report
        .combined_ic
        .iter()
        .find(|(horizon, _)| *horizon == 5)
        .map(|(_, value)| *value)
        .expect("a combined IC");
    assert!((combined_ic - expected_ic).abs() < 1e-9, "{combined_ic}");

    for score in report
        .latest
        .iter()
        .filter(|score| score.horizon_sessions == 5)
    {
        let index = names
            .iter()
            .position(|name| name == &score.symbol)
            .expect("a known symbol");
        assert!((score.combined - z[index]).abs() < 1e-9, "{score:?}");
        assert!((score.sigma - 0.2).abs() < 1e-12);
        assert!(
            (score.alpha - expected_ic * 0.2 * z[index]).abs() < 1e-9,
            "{score:?}"
        );
        assert_eq!(score.z.len(), 2);
    }
    assert_eq!(report.latest.len(), 6);
}

#[test]
fn appending_future_rows_does_not_change_earlier_cross_sections() {
    let drifts: Vec<f64> = (0..8)
        .map(|index| -0.0002 + 0.00008 * index as f64)
        .collect();
    let short = drift_universe(&drifts, 700, 0.009, 41);
    let mut long = short.clone();
    let extra = random_walk(120, 0.0005, 0.02, 99);
    for (index, extra_row) in extra.iter().enumerate() {
        long[0].push((date_at(700 + index), extra_row.1));
    }
    let names = symbols(8);
    let before = report_for(&short, &names, &config(&[20, 60]));
    let after = report_for(&long, &names, &config(&[20, 60]));

    assert_eq!(before.skill.len(), after.skill.len());
    for (left, right) in before.skill.iter().zip(after.skill.iter()) {
        assert_eq!(left.signal, right.signal);
        assert_eq!(left.horizon_sessions, right.horizon_sessions);
        assert_eq!(left.origins, right.origins, "{left:?} {right:?}");
        assert!((left.ic_mean - right.ic_mean).abs() < 1e-12);
        assert!((left.ic_se - right.ic_se).abs() < 1e-12);
        assert!((left.t_stat - right.t_stat).abs() < 1e-12);
        assert!((left.ic_used - right.ic_used).abs() < 1e-12);
    }
}
