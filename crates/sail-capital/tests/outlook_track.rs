//! Outlook track record: deterministic monthly replays with empirical coverage close to
//! the nominal bands on synthetic random-walk data.

use chrono::{Days, NaiveDate};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use sail_capital::forecast::CloseSeries;
use sail_capital::outlook::{Instrument, TrackRecord, track_record};

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

fn instrument<'a>(series: CloseSeries<'a>) -> Instrument<'a> {
    Instrument {
        series,
        external: Vec::new(),
        tilt: Vec::new(),
        tilt_signals: Vec::new(),
    }
}

#[test]
fn track_record_is_deterministic_and_near_nominal() {
    let len = 1500;
    let datasets: Vec<Vec<(NaiveDate, Decimal)>> = (0..10)
        .map(|index| random_walk(len, 0.0002, 0.01, 100 + index))
        .collect();
    let keys: Vec<String> = (0..datasets.len())
        .map(|index| format!("track-{index}"))
        .collect();
    let inputs: Vec<Instrument<'_>> = datasets
        .iter()
        .enumerate()
        .map(|(index, closes)| {
            instrument(CloseSeries {
                instrument_key: keys[index].as_str(),
                symbol: "TRK",
                closes,
            })
        })
        .collect();
    let today = date_at(len - 1);
    let first = track_record(&inputs, 21, today);
    let second = track_record(&inputs, 21, today);
    assert_eq!(first, second);
    assert!(first.replays > 200, "replays: {}", first.replays);
    assert!(
        (first.inside_68 - 0.68).abs() < 0.05,
        "inside_68: {} ({first:?})",
        first.inside_68
    );
    assert!(
        (first.inside_90 - 0.90).abs() < 0.05,
        "inside_90: {} ({first:?})",
        first.inside_90
    );
    assert!(
        (first.mean_percentile - 50.0).abs() < 5.0,
        "mean_percentile: {} ({first:?})",
        first.mean_percentile
    );
    assert!(first.skill_vs_no_change.is_finite());
    assert!(first.direction_hit_rate.is_finite());
}

#[test]
fn track_record_without_enough_history_is_empty() {
    let closes = random_walk(100, 0.0002, 0.01, 3);
    let inputs = [Instrument {
        series: CloseSeries {
            instrument_key: "short",
            symbol: "SHRT",
            closes: &closes,
        },
        external: Vec::new(),
        tilt: Vec::new(),
        tilt_signals: Vec::new(),
    }];
    let record: TrackRecord = track_record(&inputs, 21, date_at(99));
    assert_eq!(record.replays, 0);
    assert_eq!(record.inside_68, 0.0);
    assert_eq!(record.inside_90, 0.0);
    assert_eq!(record.mean_percentile, 0.0);
    assert_eq!(record.skill_vs_no_change, 0.0);
    assert_eq!(record.direction_hit_rate, 0.0);
}
