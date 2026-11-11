//! Hand-checked price-signal formulas (Kilo's slice, round 11).

use chrono::{Days, NaiveDate};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use sail_capital::forecast::CloseSeries;
use sail_capital::signals::{SignalId, price_signal};

fn base_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2020, 1, 6).expect("valid date")
}

fn date_at(index: usize) -> NaiveDate {
    base_date() + Days::new(index as u64)
}

fn decimal(value: f64) -> Decimal {
    Decimal::from_f64(value).expect("finite decimal")
}

fn closes_from_returns(returns: &[f64]) -> Vec<(NaiveDate, Decimal)> {
    let mut price = 100.0f64;
    let mut rows = vec![(date_at(0), decimal(price))];
    for (index, log_return) in returns.iter().enumerate() {
        price *= log_return.exp();
        rows.push((date_at(index + 1), decimal(price)));
    }
    rows
}

fn linear_closes(count: usize, daily_log_return: f64) -> Vec<(NaiveDate, Decimal)> {
    (0..count)
        .map(|index| {
            (
                date_at(index),
                decimal(100.0 * (index as f64 * daily_log_return).exp()),
            )
        })
        .collect()
}

fn series<'a>(closes: &'a [(NaiveDate, Decimal)]) -> CloseSeries<'a> {
    CloseSeries {
        instrument_key: "test",
        symbol: "TST",
        closes,
    }
}

#[test]
fn momentum_skips_the_last_month() {
    let closes = linear_closes(301, 0.001);
    let value = price_signal(&series(&closes), SignalId::Momentum12_1).expect("a value");
    assert!((value - 0.231).abs() < 1e-12, "{value}");
}

#[test]
fn short_term_reversal_is_negative_last_month_return() {
    let closes = linear_closes(301, 0.001);
    let value = price_signal(&series(&closes), SignalId::ShortTermReversal).expect("a value");
    assert!((value + 0.021).abs() < 1e-12, "{value}");
}

#[test]
fn low_volatility_is_minus_the_sample_standard_deviation() {
    let returns: Vec<f64> = (0..252)
        .map(|index| if index % 2 == 0 { 0.01 } else { -0.01 })
        .collect();
    let closes = closes_from_returns(&returns);
    let value = price_signal(&series(&closes), SignalId::LowVolatility).expect("a value");
    let expected = -0.01 * (252.0f64 / 251.0).sqrt();
    assert!((value - expected).abs() < 1e-12, "{value} vs {expected}");
}

#[test]
fn trend_is_distance_above_the_200_session_mean() {
    let closes = linear_closes(260, 0.002);
    let value = price_signal(&series(&closes), SignalId::Trend).expect("a value");
    let window: Vec<f64> = closes[closes.len() - 200..]
        .iter()
        .map(|(_, close)| rust_decimal::prelude::ToPrimitive::to_f64(close).expect("finite close"))
        .collect();
    let window_mean = window.iter().sum::<f64>() / window.len() as f64;
    let expected = (window[window.len() - 1] / window_mean).ln();
    assert!((value - expected).abs() < 1e-12, "{value} vs {expected}");
}

#[test]
fn every_price_signal_needs_its_minimum_history() {
    for signal in SignalId::PRICE {
        let needed = signal.min_closes();
        let short = linear_closes(needed - 1, 0.001);
        assert!(
            price_signal(&series(&short), signal).is_none(),
            "{signal:?} should need {needed} closes"
        );
        let enough = linear_closes(needed, 0.001);
        assert!(
            price_signal(&series(&enough), signal).is_some(),
            "{signal:?} should have a value at {needed} closes"
        );
    }
}

#[test]
fn non_price_signals_have_no_local_formula() {
    let closes = linear_closes(400, 0.001);
    for signal in [
        SignalId::ChartPattern,
        SignalId::FoundationForecast,
        SignalId::FilingSentiment,
        SignalId::InsiderBuying,
    ] {
        assert!(price_signal(&series(&closes), signal).is_none());
        assert_eq!(signal.min_closes(), 0);
    }
}

#[test]
fn flat_prices_give_a_finite_value_or_none() {
    let closes: Vec<(NaiveDate, Decimal)> = (0..300)
        .map(|index| (date_at(index), decimal(50.0)))
        .collect();
    for signal in SignalId::PRICE {
        if let Some(value) = price_signal(&series(&closes), signal) {
            assert!(value.is_finite(), "{signal:?} gave {value}");
        }
    }
    assert!(
        price_signal(
            &series(&[(date_at(0), Decimal::ZERO)]),
            SignalId::ShortTermReversal
        )
        .is_none()
    );
}
