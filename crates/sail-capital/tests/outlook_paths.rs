//! Outlook component paths: closed-form GBM quantiles and the insufficient-history guard.
//! Synthetic closes only, no files and no clock.

use chrono::{Days, NaiveDate};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use sail_capital::forecast::{CloseSeries, ForecastStatus};
use sail_capital::outlook::{ComponentId, Instrument, LEVELS, builtin_paths, outlook, steps};

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

#[test]
fn gbm_quantiles_match_the_closed_form() {
    let drift = 0.0004;
    let sigma = 0.012;
    let closes = random_walk(640, drift, sigma, 11);
    let series = CloseSeries {
        instrument_key: "gbm",
        symbol: "GBM",
        closes: &closes,
    };
    let origin = closes.len() - 1;
    let horizon = 63u32;
    let paths = builtin_paths(&series, origin, horizon);
    let gbm = paths
        .iter()
        .find(|(component, _)| *component == ComponentId::Gbm)
        .and_then(|(_, path)| path.as_ref())
        .expect("gbm path");
    assert_eq!(gbm.steps, steps(horizon));

    let log_closes: Vec<f64> = closes
        .iter()
        .map(|(_, close)| close.to_f64().expect("finite").ln())
        .collect();
    let returns: Vec<f64> = log_closes
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect();
    let window = &returns[returns.len() - 252..];
    let mean = window.iter().sum::<f64>() / window.len() as f64;
    let variance = window
        .iter()
        .map(|value| (value - mean) * (value - mean))
        .sum::<f64>()
        / (window.len() - 1) as f64;
    let sigma_daily = variance.sqrt();

    for (index, step) in gbm.steps.iter().enumerate() {
        let t = *step as f64;
        for (level, p) in LEVELS.iter().enumerate() {
            let z = normal_quantile(*p);
            let expected = mean * t + sigma_daily * t.sqrt() * z;
            assert!(
                (gbm.values[index][level] - expected).abs() < 1e-9,
                "step {step} level {p}: {} vs {expected}",
                gbm.values[index][level]
            );
        }
    }
}

#[test]
fn too_little_history_is_insufficient() {
    let closes = random_walk(200, 0.0002, 0.01, 5);
    let series = CloseSeries {
        instrument_key: "short",
        symbol: "SHRT",
        closes: &closes,
    };
    let paths = builtin_paths(&series, closes.len() - 1, 21);
    assert!(paths.iter().all(|(_, path)| path.is_none()));

    let instrument = Instrument {
        series,
        external: Vec::new(),
        tilt: Vec::new(),
        tilt_signals: Vec::new(),
    };
    let result = outlook(&instrument, 21, None);
    assert!(matches!(
        result.status,
        ForecastStatus::InsufficientEvidence { .. }
    ));
    if let ForecastStatus::InsufficientEvidence { reason } = &result.status {
        assert!(reason.contains("252"), "reason: {reason}");
    }
    assert!(result.fan.is_empty());
}

fn normal_quantile(p: f64) -> f64 {
    let p = p.clamp(1e-12, 1.0 - 1e-12);
    let a = [
        -3.969_683_028_665_376e+01,
        2.209_460_984_245_205e+02,
        -2.759_285_104_469_687e+02,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e+01,
        2.506_628_277_459_239e+00,
    ];
    let b = [
        -5.447_609_879_822_406e+01,
        1.615_858_368_580_409e+02,
        -1.556_989_798_598_866e+02,
        6.680_131_188_771_972e+01,
        -1.328_068_155_288_572e+01,
    ];
    let c = [
        -7.784_894_002_430_293e-03,
        -3.223_964_580_411_365e-01,
        -2.400_758_277_161_838e+00,
        -2.549_732_539_343_734e+00,
        4.374_664_141_464_968e+00,
        2.938_163_982_698_783e+00,
    ];
    let d = [
        7.784_695_709_041_462e-03,
        3.224_671_290_700_398e-01,
        2.445_134_137_142_996e+00,
        3.754_408_661_907_416e+00,
    ];
    if p < 0.024_25 {
        let q = (-2.0 * p.ln()).sqrt();
        (((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0)
    } else if p <= 1.0 - 0.024_25 {
        let q = p - 0.5;
        let r = q * q;
        (((((a[0] * r + a[1]) * r + a[2]) * r + a[3]) * r + a[4]) * r + a[5]) * q
            / (((((b[0] * r + b[1]) * r + b[2]) * r + b[3]) * r + b[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0)
    }
}
