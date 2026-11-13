//! Round 20 Signal v2 baseline features: frozen windows, multi-horizon returns and squeeze.

use chrono::{Days, NaiveDate};
use sail_capital::forecast::baseline::{
    BASELINE_HORIZONS, FEATURES_VERSION, MIN_BASELINE_BARS, features,
};
use sail_capital::trade::Bar;

fn start() -> NaiveDate {
    NaiveDate::from_ymd_opt(2020, 1, 1).unwrap()
}

fn bar(index: usize, close: f64, high: f64, low: f64) -> Bar {
    Bar {
        session: start() + Days::new(index as u64),
        open: close,
        high,
        low,
        close,
    }
}

fn flat(count: usize) -> Vec<Bar> {
    (0..count)
        .map(|index| bar(index, 100.0, 101.0, 99.0))
        .collect()
}

#[test]
fn flat_bars_give_zero_returns_and_a_neutral_squeeze() {
    let bars = flat(MIN_BASELINE_BARS);
    let found = features(&bars).expect("enough bars");
    assert_eq!(found.version, FEATURES_VERSION);
    assert_eq!(found.as_of_session, bars.last().unwrap().session);
    assert_eq!(found.bars, 253);
    assert_eq!(found.return_5, 0.0);
    assert_eq!(found.return_21, 0.0);
    assert_eq!(found.return_63, 0.0);
    assert_eq!(found.return_126, 0.0);
    assert_eq!(found.return_252, 0.0);
    assert_eq!(found.momentum_12_1, 0.0);
    assert_eq!(found.trailing_drift_daily, 0.0);
    assert_eq!(found.ewma_volatility_daily, 0.0);
    assert!((found.squeeze_range_20 - 0.02).abs() < 1e-12);
    assert_eq!(found.squeeze_compression, 1.0);
    assert_eq!(found.squeeze_atr, 1.0);
    assert!((found.close_in_range_20 - 0.5).abs() < 1e-12);
    assert_eq!(BASELINE_HORIZONS, [5, 21, 63]);
}

#[test]
fn multi_horizon_returns_and_twelve_minus_one_momentum_use_the_documented_windows() {
    let mut closes = vec![100.0; 253];
    closes[0] = 50.0;
    closes[126] = 80.0;
    closes[189] = 90.0;
    closes[231] = 95.0;
    closes[247] = 100.0;
    closes[252] = 110.0;
    let bars: Vec<Bar> = closes
        .iter()
        .enumerate()
        .map(|(index, close)| bar(index, *close, close * 1.01, close * 0.99))
        .collect();
    let found = features(&bars).expect("enough bars");
    assert!((found.return_5 - (110.0f64 / 100.0).ln()).abs() < 1e-12);
    assert!((found.return_21 - (110.0f64 / 95.0).ln()).abs() < 1e-12);
    assert!((found.return_63 - (110.0f64 / 90.0).ln()).abs() < 1e-12);
    assert!((found.return_126 - (110.0f64 / 80.0).ln()).abs() < 1e-12);
    assert!((found.return_252 - (110.0f64 / 50.0).ln()).abs() < 1e-12);
    assert!((found.momentum_12_1 - (95.0f64 / 50.0).ln()).abs() < 1e-12);

    let expected_drift = (1..closes.len())
        .map(|index| (closes[index] / closes[index - 1]).ln())
        .sum::<f64>()
        / 252.0;
    assert!((found.trailing_drift_daily - expected_drift).abs() < 1e-12);
    assert!(found.ewma_volatility_daily > 0.0);
    assert!(found.ewma_volatility_daily.is_finite());
}

#[test]
fn a_narrow_last_month_has_compression_below_one() {
    let mut bars: Vec<Bar> = (0..300)
        .map(|index| bar(index, 100.0, 110.0, 90.0))
        .collect();
    for (index, slot) in bars.iter_mut().enumerate().skip(280) {
        *slot = bar(index, 100.0, 101.0, 99.0);
    }
    let found = features(&bars).expect("enough bars");
    // Last 20 range = 2; last 60 range still includes the wide bars = 20.
    assert!((found.squeeze_range_20 - 0.02).abs() < 1e-12);
    assert!((found.squeeze_compression - 0.1).abs() < 1e-12);
    assert!(found.squeeze_atr > 0.0);
    assert!((found.close_in_range_20 - 0.5).abs() < 1e-12);
}

#[test]
fn a_close_at_the_top_of_its_range_is_near_one() {
    let mut bars = flat(260);
    bars[259] = bar(259, 100.9, 101.0, 99.0);
    let found = features(&bars).expect("enough bars");
    assert!(found.close_in_range_20 > 0.94 && found.close_in_range_20 <= 1.0);
}

#[test]
fn insufficient_or_invalid_bars_are_refused() {
    assert!(features(&flat(252)).is_err());
    let mut bars = flat(260);
    bars[10].high = 98.0;
    bars[10].low = 99.0;
    assert!(features(&bars).is_err());
    let mut bars = flat(260);
    bars.swap(10, 11);
    assert!(features(&bars).is_err());
    let mut bars = flat(260);
    bars[20].close = f64::NAN;
    assert!(features(&bars).is_err());
}
