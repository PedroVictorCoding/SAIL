//! Outlook blending: monotone pooled quantiles, equal weights when evidence is thin,
//! weights that favour the component with the right distribution, no look-ahead, and the
//! direction tilt. Synthetic closes and fake helper paths only.

use chrono::{Days, NaiveDate};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use sail_capital::forecast::{CloseSeries, ForecastStatus};
use sail_capital::outlook::{
    ComponentId, ExternalPath, Instrument, LEVELS, PathQuantiles, outlook, steps,
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

fn log_returns(closes: &[(NaiveDate, Decimal)]) -> Vec<f64> {
    let logs: Vec<f64> = closes
        .iter()
        .map(|(_, close)| close.to_f64().expect("finite").ln())
        .collect();
    logs.windows(2).map(|pair| pair[1] - pair[0]).collect()
}

fn weight(outlook: &sail_capital::outlook::Outlook, component: ComponentId) -> f64 {
    outlook
        .weights
        .iter()
        .find(|(id, _)| *id == component)
        .map(|(_, value)| *value)
        .unwrap_or(0.0)
}

fn external(component: ComponentId, origin: usize, horizon: u32, values: [f64; 5]) -> ExternalPath {
    ExternalPath {
        component,
        instrument_key: "fake".into(),
        origin: date_at(origin),
        horizon,
        path: PathQuantiles {
            steps: vec![horizon],
            values: vec![values],
        },
    }
}

#[test]
fn pooled_quantiles_are_non_decreasing() {
    let closes = random_walk(600, 0.0003, 0.011, 7);
    let series = CloseSeries {
        instrument_key: "blend",
        symbol: "BLND",
        closes: &closes,
    };
    let instrument = Instrument {
        series,
        external: Vec::new(),
        tilt: Vec::new(),
        tilt_signals: Vec::new(),
    };
    let horizon = 63;
    let result = outlook(&instrument, horizon, None);
    assert!(matches!(result.status, ForecastStatus::Ok));
    assert_eq!(result.fan.len(), steps(horizon).len());
    for point in &result.fan {
        for level in 1..5 {
            assert!(
                point.prices[level] >= point.prices[level - 1],
                "step {}: {:?}",
                point.step,
                point.prices
            );
        }
        assert!(point.mean.is_finite() && point.mean > 0.0);
    }
    let total: f64 = result.weights.iter().map(|(_, value)| *value).sum();
    assert!((total - 1.0).abs() < 1e-9);
    assert!(result.weight_origins >= 20);
}

#[test]
fn equal_weights_below_twenty_origins() {
    let closes = random_walk(324, 0.0003, 0.01, 9);
    let series = CloseSeries {
        instrument_key: "thin",
        symbol: "THIN",
        closes: &closes,
    };
    let instrument = Instrument {
        series,
        external: Vec::new(),
        tilt: Vec::new(),
        tilt_signals: Vec::new(),
    };
    let result = outlook(&instrument, 21, None);
    assert!(matches!(result.status, ForecastStatus::Ok));
    assert_eq!(result.weight_origins, 11);
    assert!((weight(&result, ComponentId::Gbm) - 1.0 / 3.0).abs() < 1e-9);
    assert!((weight(&result, ComponentId::Ewma) - 1.0 / 3.0).abs() < 1e-9);
    assert!((weight(&result, ComponentId::Historical) - 1.0 / 3.0).abs() < 1e-9);
    assert_eq!(weight(&result, ComponentId::Chronos), 0.0);
    assert_eq!(weight(&result, ComponentId::Gbdt), 0.0);
}

#[test]
fn weights_favour_the_component_that_is_right() {
    let len = 900;
    let horizon = 21u32;
    let closes = random_walk(len, 0.0002, 0.01, 13);
    let returns = log_returns(&closes);
    let mut external_paths = Vec::new();
    for origin in 0..len - horizon as usize {
        let outcome = returns[origin..origin + horizon as usize]
            .iter()
            .sum::<f64>();
        external_paths.push(external(
            ComponentId::Chronos,
            origin,
            horizon,
            [
                outcome - 0.002,
                outcome - 0.001,
                outcome,
                outcome + 0.001,
                outcome + 0.002,
            ],
        ));
        external_paths.push(external(
            ComponentId::Gbdt,
            origin,
            horizon,
            [
                0.29 + outcome,
                0.295 + outcome,
                0.30 + outcome,
                0.305 + outcome,
                0.31 + outcome,
            ],
        ));
    }
    let series = CloseSeries {
        instrument_key: "weights",
        symbol: "WGHT",
        closes: &closes,
    };
    let instrument = Instrument {
        series,
        external: external_paths,
        tilt: Vec::new(),
        tilt_signals: Vec::new(),
    };
    let result = outlook(&instrument, horizon, Some(date_at(850)));
    assert!(matches!(result.status, ForecastStatus::Ok));
    assert!(
        weight(&result, ComponentId::Chronos) > 0.6,
        "weights: {:?}",
        result.weights
    );
    assert!(
        weight(&result, ComponentId::Gbdt) < 0.2,
        "weights: {:?}",
        result.weights
    );
}

#[test]
fn a_replay_ignores_future_closes_and_future_paths() {
    let len = 700;
    let horizon = 21u32;
    let cutoff = 500usize;
    let closes = random_walk(len, 0.0002, 0.01, 17);
    let returns = log_returns(&closes);
    let mut external_paths = Vec::new();
    for origin in 0..=cutoff {
        if origin + horizon as usize <= len {
            let outcome = returns[origin..origin + horizon as usize]
                .iter()
                .sum::<f64>();
            if origin % 5 == 0 || origin == cutoff {
                external_paths.push(external(
                    ComponentId::Chronos,
                    origin,
                    horizon,
                    [
                        outcome - 0.004,
                        outcome - 0.002,
                        outcome,
                        outcome + 0.002,
                        outcome + 0.004,
                    ],
                ));
            }
        }
    }
    let series = CloseSeries {
        instrument_key: "replay",
        symbol: "RPLY",
        closes: &closes,
    };
    let clean = Instrument {
        series,
        external: external_paths.clone(),
        tilt: Vec::new(),
        tilt_signals: Vec::new(),
    };
    let baseline = outlook(&clean, horizon, Some(date_at(cutoff)));

    let mut polluted_paths = external_paths;
    for origin in [550usize, 600, 650] {
        let outcome = returns[origin..origin + horizon as usize]
            .iter()
            .sum::<f64>();
        polluted_paths.push(external(
            ComponentId::Chronos,
            origin,
            horizon,
            [
                outcome + 5.0,
                outcome + 6.0,
                outcome + 7.0,
                outcome + 8.0,
                outcome + 9.0,
            ],
        ));
    }
    let polluted = Instrument {
        series: CloseSeries {
            instrument_key: "replay",
            symbol: "RPLY",
            closes: &closes,
        },
        external: polluted_paths,
        tilt: Vec::new(),
        tilt_signals: Vec::new(),
    };
    let with_future = outlook(&polluted, horizon, Some(date_at(cutoff)));

    let truncated = &closes[..=cutoff + horizon as usize];
    let short = Instrument {
        series: CloseSeries {
            instrument_key: "replay",
            symbol: "RPLY",
            closes: truncated,
        },
        external: clean.external.clone(),
        tilt: Vec::new(),
        tilt_signals: Vec::new(),
    };
    let short_result = outlook(&short, horizon, Some(date_at(cutoff)));

    assert_eq!(baseline.fan, with_future.fan);
    assert_eq!(baseline.weights, with_future.weights);
    assert_eq!(baseline.weight_origins, with_future.weight_origins);
    assert_eq!(baseline.origin, with_future.origin);
    assert_eq!(baseline.actual, with_future.actual);
    assert_eq!(baseline.fan, short_result.fan);
    assert_eq!(baseline.weights, short_result.weights);
    assert_eq!(baseline.actual, short_result.actual);
    assert_eq!(baseline.actual_percentile, short_result.actual_percentile);
}

#[test]
fn direction_tilt_shifts_the_fan_linearly_in_steps() {
    let closes = random_walk(600, 0.0002, 0.01, 21);
    let series = CloseSeries {
        instrument_key: "tilt",
        symbol: "TILT",
        closes: &closes,
    };
    let horizon = 63u32;
    let plain = Instrument {
        series: CloseSeries {
            instrument_key: "tilt",
            symbol: "TILT",
            closes: &closes,
        },
        external: Vec::new(),
        tilt: Vec::new(),
        tilt_signals: Vec::new(),
    };
    let alpha = 0.05;
    let tilted = Instrument {
        series,
        external: Vec::new(),
        tilt: vec![(horizon, alpha)],
        tilt_signals: vec!["chart_trend h63 t=2.1".into()],
    };
    let before = outlook(&plain, horizon, None);
    let after = outlook(&tilted, horizon, None);
    assert!((after.tilt - alpha).abs() < 1e-12);
    assert_eq!(
        after.tilt_signals,
        vec!["chart_trend h63 t=2.1".to_string()]
    );
    assert_eq!(before.fan.len(), after.fan.len());
    for (left, right) in before.fan.iter().zip(&after.fan) {
        let shift = alpha * right.step as f64 / horizon as f64;
        for level in 0..LEVELS.len() {
            let ratio = right.prices[level] / left.prices[level];
            assert!(
                (ratio - shift.exp()).abs() < 1e-9,
                "step {} level {}: {ratio} vs {}",
                right.step,
                level,
                shift.exp()
            );
        }
        assert!((right.mean / left.mean - shift.exp()).abs() < 1e-9);
    }
}
