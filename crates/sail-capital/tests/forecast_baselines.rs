//! Forecast baseline tests (Kilo's slice, round 10): synthetic series built in code, with
//! no files and no network.

use chrono::{Days, NaiveDate};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use sail_capital::forecast::{
    CalibrationState, CloseSeries, EvalConfig, ForecastDistribution, ForecastStatus,
    MIN_OBSERVATIONS, ModelId, QUANTILE_LEVELS, evaluate, forecast,
};

fn base_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2015, 1, 5).expect("valid date")
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

/// A daily log random walk: `close_t = close_{t-1} * exp(drift + sigma * z)`.
fn random_walk(len: usize, drift: f64, sigma: f64, seed: u64) -> Vec<(NaiveDate, Decimal)> {
    random_walk_from(0, len, drift, sigma, seed)
}

fn random_walk_from(
    start: usize,
    len: usize,
    drift: f64,
    sigma: f64,
    seed: u64,
) -> Vec<(NaiveDate, Decimal)> {
    let mut rng = Lcg::new(seed);
    let mut price = 100.0f64;
    let mut rows = Vec::with_capacity(len);
    for index in start..start + len {
        if index > start {
            price *= (drift + sigma * rng.normal()).exp();
        }
        rows.push((date_at(index), decimal(price)));
    }
    rows
}

fn growing_series(len: usize, daily_log_return: f64) -> Vec<(NaiveDate, Decimal)> {
    let mut price = 100.0f64;
    let mut rows = Vec::with_capacity(len);
    for index in 0..len {
        if index > 0 {
            price *= daily_log_return.exp();
        }
        rows.push((date_at(index), decimal(price)));
    }
    rows
}

fn unverified() -> CalibrationState {
    CalibrationState {
        verified: false,
        cohort: None,
    }
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

fn ok(distribution: ForecastDistribution) -> ForecastDistribution {
    assert!(
        matches!(distribution.status, ForecastStatus::Ok),
        "expected a forecast, got {:?}",
        distribution.status
    );
    distribution
}

#[test]
fn gaussian_random_walk_is_calibrated_at_twenty_sessions() {
    let closes = random_walk(2_000, 0.0, 0.01, 42);
    let input = [series(&closes, "test", "TEST")];
    let config = EvalConfig::default();
    let report = evaluate(&input, &config);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);

    let score = report
        .scores
        .iter()
        .find(|s| s.model == ModelId::NoChange && s.horizon_sessions == 20)
        .expect("no-change score at 20 sessions");
    assert!(score.forecasts >= 100, "{score:?}");
    assert!(
        (0.85..0.95).contains(&score.interval_90),
        "interval_90 = {}",
        score.interval_90
    );
    assert!(score.calibrated, "{score:?}");

    let distribution = ok(forecast(
        &series(&closes, "test", "TEST"),
        ModelId::NoChange,
        20,
        unverified(),
    ));
    let sigma = distribution.sigma_log_return.expect("sigma");
    let expected = 0.01 * 20f64.sqrt();
    assert!(
        (sigma - expected).abs() < 0.15 * expected,
        "sigma = {sigma}, expected about {expected}"
    );
    let variance = distribution.variance_log_return.expect("variance");
    assert!((variance - sigma * sigma).abs() < 1e-15);
}

#[test]
fn trailing_drift_beats_no_change_on_a_trending_series() {
    let closes = growing_series(1_400, 0.0008);
    let input = [series(&closes, "trend", "TREND")];
    let config = EvalConfig::default();
    let report = evaluate(&input, &config);

    for horizon in [5, 20, 60] {
        let drift = report
            .scores
            .iter()
            .find(|s| s.model == ModelId::TrailingDrift && s.horizon_sessions == horizon)
            .unwrap_or_else(|| panic!("trailing-drift score at {horizon}"));
        assert!(
            drift.crps_skill > 0.0,
            "horizon {horizon}: skill = {}",
            drift.crps_skill
        );
    }
}

#[test]
fn insufficient_evidence_is_explicit() {
    let short = random_walk(MIN_OBSERVATIONS - 10, 0.0, 0.01, 7);
    let distribution = forecast(
        &series(&short, "short", "SHORT"),
        ModelId::NoChange,
        20,
        unverified(),
    );
    match distribution.status {
        ForecastStatus::InsufficientEvidence { reason } => {
            assert!(reason.contains("daily returns"), "{reason}");
        }
        ForecastStatus::Ok => panic!("expected insufficient evidence"),
    }
    assert!(distribution.expected_log_return.is_none());
    assert!(distribution.median_log_return.is_none());
    assert!(distribution.sigma_log_return.is_none());
    assert!(distribution.variance_log_return.is_none());
    assert!(distribution.quantiles.is_empty());
    assert!(distribution.probability_positive.is_none());
    assert!(distribution.expected_terminal_price.is_none());
    assert!(distribution.median_terminal_price.is_none());
    assert_eq!(distribution.observations_used, 0);

    let mut broken = random_walk(400, 0.0, 0.01, 8);
    broken[100].1 = Decimal::from_f64(-1.0).expect("decimal");
    let distribution = forecast(
        &series(&broken, "broken", "BROKEN"),
        ModelId::NoChange,
        20,
        unverified(),
    );
    assert!(matches!(
        distribution.status,
        ForecastStatus::InsufficientEvidence { .. }
    ));
}

#[test]
fn empirical_model_reports_its_sample_limit() {
    let closes = random_walk(320, 0.0, 0.01, 11);
    let distribution = forecast(
        &series(&closes, "empirical", "EMP"),
        ModelId::EmpiricalHorizon,
        60,
        unverified(),
    );
    match distribution.status {
        ForecastStatus::InsufficientEvidence { reason } => {
            assert!(reason.contains("20"), "{reason}");
        }
        ForecastStatus::Ok => panic!("expected too few empirical samples"),
    }
}

#[test]
fn gaussian_invariants_hold() {
    let closes = random_walk(900, 0.0002, 0.012, 21);
    let verified = CalibrationState {
        verified: true,
        cohort: Some("test cohort".into()),
    };
    let distribution = ok(forecast(
        &series(&closes, "invariants", "INV"),
        ModelId::TrailingDrift,
        20,
        verified.clone(),
    ));

    let expected_price = distribution
        .expected_terminal_price
        .expect("expected price");
    let median_price = distribution.median_terminal_price.expect("median price");
    assert!(expected_price > median_price);

    let sigma = distribution.sigma_log_return.expect("sigma");
    let variance = distribution.variance_log_return.expect("variance");
    assert!((variance - sigma * sigma).abs() < 1e-15);

    assert_eq!(distribution.quantiles.len(), QUANTILE_LEVELS.len());
    for (index, quantile) in distribution.quantiles.iter().enumerate() {
        assert!((quantile.p - QUANTILE_LEVELS[index]).abs() < 1e-12);
    }
    for pair in distribution.quantiles.windows(2) {
        assert!(
            pair[0].log_return < pair[1].log_return,
            "quantiles not monotone: {:?}",
            distribution.quantiles
        );
    }
    let median = distribution.median_log_return.expect("median");
    assert!((distribution.quantiles[2].log_return - median).abs() < 1e-12);
    assert_eq!(distribution.calibration, verified);
}

#[test]
fn empirical_invariants_hold() {
    let closes = random_walk(1_200, 0.0001, 0.011, 22);
    let distribution = ok(forecast(
        &series(&closes, "empirical", "EMP"),
        ModelId::EmpiricalHorizon,
        20,
        unverified(),
    ));
    assert!(distribution.expected_terminal_price.is_none());
    let sigma = distribution.sigma_log_return.expect("sigma");
    let variance = distribution.variance_log_return.expect("variance");
    assert!((variance - sigma * sigma).abs() < 1e-15);
    for pair in distribution.quantiles.windows(2) {
        assert!(pair[0].log_return <= pair[1].log_return);
    }
    let median = distribution.median_log_return.expect("median");
    assert!((distribution.quantiles[2].log_return - median).abs() < 1e-12);
    assert!(distribution.observations_used > 0);
}

#[test]
fn probability_positive_waits_for_verification() {
    let closes = random_walk(700, 0.0003, 0.01, 31);
    let input = series(&closes, "prob", "PROB");

    let unverified_distribution = ok(forecast(&input, ModelId::TrailingDrift, 20, unverified()));
    assert!(unverified_distribution.probability_positive.is_none());

    let verified = CalibrationState {
        verified: true,
        cohort: None,
    };
    let verified_distribution = ok(forecast(
        &input,
        ModelId::TrailingDrift,
        20,
        verified.clone(),
    ));
    let probability = verified_distribution
        .probability_positive
        .expect("calibrated probability");
    assert!((0.0..=1.0).contains(&probability));
    assert_eq!(verified_distribution.calibration, verified);

    // Zero-drift models would always say 50%, so they publish nothing even when verified.
    for model in [ModelId::NoChange, ModelId::EwmaVolatility] {
        let zero_drift = ok(forecast(&input, model, 20, verified.clone()));
        assert!(zero_drift.probability_positive.is_none(), "{model:?}");
    }
}

#[test]
fn appending_future_rows_does_not_change_a_cut_forecast() {
    let mut closes = random_walk(700, 0.0002, 0.01, 41);
    let cut = 500usize;
    let verified = CalibrationState {
        verified: true,
        cohort: None,
    };

    let before: Vec<ForecastDistribution> = ModelId::ALL
        .iter()
        .map(|model| {
            forecast(
                &CloseSeries {
                    instrument_key: "leak",
                    symbol: "LEAK",
                    closes: &closes[..=cut],
                },
                *model,
                20,
                verified.clone(),
            )
        })
        .collect();

    closes.extend(random_walk_from(cut + 1, 300, 0.0, 0.30, 99));
    let after: Vec<ForecastDistribution> = ModelId::ALL
        .iter()
        .map(|model| {
            forecast(
                &CloseSeries {
                    instrument_key: "leak",
                    symbol: "LEAK",
                    closes: &closes[..=cut],
                },
                *model,
                20,
                verified.clone(),
            )
        })
        .collect();

    assert_eq!(before, after);
}

#[test]
fn evaluation_never_reads_rows_past_the_outcome() {
    let mut closes = random_walk(400, 0.0002, 0.01, 51);
    let config = EvalConfig {
        horizons: vec![5],
        step_sessions: 1_000_000,
        min_train: MIN_OBSERVATIONS,
    };

    let before = {
        let input = [series(&closes, "guard", "GUARD")];
        evaluate(&input, &config)
    };
    assert_eq!(before.scores.len(), ModelId::ALL.len());

    let start = closes.len();
    closes.extend(random_walk_from(start, 300, 0.0, 0.40, 77));
    let after = {
        let input = [series(&closes, "guard", "GUARD")];
        evaluate(&input, &config)
    };
    assert_eq!(before.scores, after.scores);
    assert_eq!(before.rank_ic, after.rank_ic);
}

#[test]
fn rank_ic_is_about_one_when_forecasts_match_outcomes() {
    let drifts = [0.0002, 0.0004, 0.0006, 0.0008, 0.0010];
    let series_data: Vec<Vec<(NaiveDate, Decimal)>> = drifts
        .iter()
        .map(|drift| growing_series(500, *drift))
        .collect();
    let symbols = ["AAA", "BBB", "CCC", "DDD", "EEE"];
    let input: Vec<CloseSeries<'_>> = series_data
        .iter()
        .enumerate()
        .map(|(index, closes)| CloseSeries {
            instrument_key: symbols[index],
            symbol: symbols[index],
            closes,
        })
        .collect();
    let config = EvalConfig {
        horizons: vec![5],
        step_sessions: 5,
        min_train: MIN_OBSERVATIONS,
    };

    let report = evaluate(&input, &config);
    let drift_ic = report
        .rank_ic
        .iter()
        .find(|ic| ic.model == ModelId::TrailingDrift && ic.horizon_sessions == 5)
        .expect("trailing-drift rank IC");
    assert!(
        (drift_ic.mean - 1.0).abs() < 1e-9,
        "mean rank IC = {}",
        drift_ic.mean
    );
    assert!(drift_ic.origins >= 5);
    assert_eq!(report.instruments.len(), 5);
}

#[test]
fn short_instruments_are_skipped_with_a_reason() {
    let long = random_walk(400, 0.0, 0.01, 61);
    let short = random_walk(100, 0.0, 0.01, 62);
    let input = [
        series(&long, "long", "LONG"),
        series(&short, "short", "SHORT"),
    ];
    let config = EvalConfig {
        horizons: vec![5],
        step_sessions: 5,
        min_train: MIN_OBSERVATIONS,
    };
    let report = evaluate(&input, &config);
    assert_eq!(report.instruments, vec!["LONG".to_string()]);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].0, "SHORT");
    assert!(report.skipped[0].1.contains("needs at least"));
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("survivorship"))
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("before spreads, fees, taxes and FX"))
    );
}

#[test]
fn sparse_prices_are_insufficient_evidence_and_not_scored() {
    use chrono::Duration;
    use sail_capital::forecast::{
        CalibrationState, CloseSeries, EvalConfig, ForecastStatus, ModelId, evaluate, forecast,
    };
    // 400 closes a week apart: "daily" returns that really cover weeks.
    let start = chrono::NaiveDate::from_ymd_opt(2015, 1, 5).unwrap();
    let closes: Vec<(chrono::NaiveDate, rust_decimal::Decimal)> = (0..400)
        .map(|i| {
            let price = 100.0 * (1.0 + 0.01 * ((i % 7) as f64 - 3.0));
            (
                start + Duration::days(7 * i as i64),
                rust_decimal::Decimal::from_f64_retain(price)
                    .unwrap()
                    .round_dp(4),
            )
        })
        .collect();
    let series = CloseSeries {
        instrument_key: "sym:GAPS.DE",
        symbol: "GAPS.DE",
        closes: &closes,
    };
    let unverified = CalibrationState {
        verified: false,
        cohort: None,
    };
    for model in ModelId::ALL {
        let f = forecast(&series, model, 20, unverified.clone());
        match &f.status {
            ForecastStatus::InsufficientEvidence { reason } => {
                assert!(reason.contains("missing many trading days"), "{reason}")
            }
            other => panic!("{model:?} forecast from sparse prices: {other:?}"),
        }
        assert!(f.sigma_log_return.is_none());
    }
    let report = evaluate(&[series], &EvalConfig::default());
    assert!(report.scores.iter().all(|s| s.forecasts == 0) || report.scores.is_empty());
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("missing trading days"))
    );
}
