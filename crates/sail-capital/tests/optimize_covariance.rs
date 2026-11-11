//! The weekly covariance built for ideas mode (Kilo's slice, round 11).

use chrono::{Days, NaiveDate};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use sail_capital::forecast::CloseSeries;
use sail_capital::optimize::covariance;

fn base_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2021, 3, 1).expect("valid date")
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

fn correlated_universe(count: usize, len: usize, seed: u64) -> Vec<Vec<(NaiveDate, Decimal)>> {
    let mut rng = Lcg::new(seed);
    let mut factor = vec![0.0; len];
    for value in factor.iter_mut().skip(1) {
        *value = rng.normal();
    }
    (0..count)
        .map(|instrument| {
            let mut price = 100.0f64;
            let mut rows = Vec::with_capacity(len);
            rows.push((date_at(0), decimal(price)));
            for (index, market) in factor.iter().enumerate().skip(1) {
                let idio = rng.normal();
                let log_return = 0.0002 + 0.008 * market + 0.006 * idio + 0.001 * instrument as f64;
                price *= log_return.exp();
                rows.push((date_at(index), decimal(price)));
            }
            rows
        })
        .collect()
}

fn series<'a>(closes: &'a [(NaiveDate, Decimal)], key: &'a str) -> CloseSeries<'a> {
    CloseSeries {
        instrument_key: key,
        symbol: key,
        closes,
    }
}

fn gershgorin_lower(matrix: &[f64], count: usize) -> f64 {
    let mut lower = f64::INFINITY;
    for row in 0..count {
        let mut off = 0.0;
        for column in 0..count {
            if column != row {
                off += matrix[row * count + column].abs();
            }
        }
        lower = lower.min(matrix[row * count + row] - off);
    }
    lower
}

#[test]
fn the_covariance_is_symmetric_and_positive_semidefinite() {
    let data = correlated_universe(5, 320, 5);
    let keys: Vec<String> = (0..5).map(|index| format!("S{index}")).collect();
    let series: Vec<CloseSeries<'_>> = data
        .iter()
        .enumerate()
        .map(|(index, rows)| series(rows, &keys[index]))
        .collect();
    let matrix = covariance(&series, 20);
    let count = series.len();
    assert_eq!(matrix.len(), count * count);

    for row in 0..count {
        for column in 0..count {
            assert_eq!(matrix[row * count + column], matrix[column * count + row]);
        }
        assert!(matrix[row * count + row] > 0.0, "variance {row}");
    }
    let lower = gershgorin_lower(&matrix, count);
    assert!(lower >= -1e-12, "not positive semidefinite: {lower}");
    for weights in [
        vec![1.0, 0.0, 0.0, 0.0, 0.0],
        vec![0.3, 0.3, 0.2, 0.1, 0.1],
        vec![-0.5, 0.5, 0.0, 0.0, 0.0],
    ] {
        let mut quadratic = 0.0;
        for row in 0..count {
            for column in 0..count {
                quadratic += weights[row] * matrix[row * count + column] * weights[column];
            }
        }
        assert!(quadratic >= -1e-12, "{quadratic}");
    }
}

#[test]
fn the_covariance_scales_with_the_horizon() {
    let data = correlated_universe(4, 300, 9);
    let keys: Vec<String> = (0..4).map(|index| format!("S{index}")).collect();
    let series: Vec<CloseSeries<'_>> = data
        .iter()
        .enumerate()
        .map(|(index, rows)| series(rows, &keys[index]))
        .collect();
    let short = covariance(&series, 5);
    let long = covariance(&series, 20);
    for (short_value, long_value) in short.iter().zip(long.iter()) {
        assert!(
            (long_value - 4.0 * short_value).abs() < 1e-12,
            "{short_value} {long_value}"
        );
    }
    let empty = covariance(&[], 20);
    assert!(empty.is_empty());
}

#[test]
fn pairs_with_too_few_common_weeks_stay_uncorrelated() {
    let long = correlated_universe(1, 300, 13).pop().expect("one series");
    let short = correlated_universe(1, 60, 15).pop().expect("one series");
    let matrix = covariance(&[series(&long, "LONG"), series(&short, "SHORT")], 20);
    assert_eq!(matrix[1], 0.0, "{matrix:?}");
    assert_eq!(matrix[2], 0.0, "{matrix:?}");
    assert!(matrix[0] > 0.0);
    assert!(matrix[3] > 0.0);
}
