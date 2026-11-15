//! One hand-made firing and one non-firing session for every setup.

mod trade_support;

use sail_capital::trade::{Bar, SetupId, Side, fire};
use trade_support::{bar, bars_from_closes};

fn fires_at_last(setup: SetupId, bars: &[Bar]) -> Option<Side> {
    let last = bars.len() - 1;
    fire(setup, bars)
        .into_iter()
        .find(|(index, _)| *index == last)
        .map(|(_, side)| side)
}

fn trend_closes() -> Vec<f64> {
    (0..260).map(|index| 100.0 + 0.2 * index as f64).collect()
}

#[test]
fn trend_pullback_fires_long_after_two_down_days() {
    let mut closes = trend_closes();
    let last = closes.len() - 1;
    closes[last - 1] -= 1.0;
    closes[last] = closes[last - 1] - 1.0;
    let bars = bars_from_closes(&closes, 0.4);
    assert_eq!(
        fires_at_last(SetupId::TrendPullback, &bars),
        Some(Side::Long)
    );
}

#[test]
fn trend_pullback_does_not_fire_on_an_up_day() {
    let mut closes = trend_closes();
    let last = closes.len() - 1;
    closes[last - 1] += 0.5;
    closes[last] += 1.0;
    let bars = bars_from_closes(&closes, 0.4);
    assert_eq!(fires_at_last(SetupId::TrendPullback, &bars), None);
}

fn squeeze_bars(breakout: f64) -> Vec<Bar> {
    let mut bars = Vec::new();
    for index in 0..=59 {
        bars.push(bar(index, 100.0, 5.0));
    }
    bars.push(Bar {
        session: trade_support::date_at(60),
        open: 100.0,
        high: 100.05,
        low: 90.0,
        close: 100.0,
        volume: None,
        dividend: None,
    });
    for index in 61..=79 {
        bars.push(bar(index, 100.0, 0.2));
    }
    bars.push(Bar {
        session: trade_support::date_at(80),
        open: 100.0,
        high: breakout + 0.1,
        low: 99.9,
        close: breakout,
        volume: None,
        dividend: None,
    });
    bars
}

#[test]
fn squeeze_breakout_fires_on_a_narrow_range_breakout() {
    let bars = squeeze_bars(100.5);
    assert_eq!(
        fires_at_last(SetupId::SqueezeBreakout, &bars),
        Some(Side::Long)
    );
}

#[test]
fn squeeze_breakout_does_not_fire_inside_the_range() {
    let bars = squeeze_bars(100.0);
    assert_eq!(fires_at_last(SetupId::SqueezeBreakout, &bars), None);
}

#[test]
fn donchian_fires_on_a_new_high_in_an_uptrend() {
    let closes: Vec<f64> = (0..260).map(|index| 100.0 + index as f64).collect();
    let bars = bars_from_closes(&closes, 0.5);
    assert_eq!(fires_at_last(SetupId::Donchian55, &bars), Some(Side::Long));
}

#[test]
fn donchian_does_not_fire_below_the_prior_high() {
    let mut closes: Vec<f64> = (0..260).map(|index| 100.0 + index as f64).collect();
    let last = closes.len() - 1;
    closes[last] = closes[last - 1] - 2.0;
    let bars = bars_from_closes(&closes, 0.5);
    assert_eq!(fires_at_last(SetupId::Donchian55, &bars), None);
}

#[test]
fn bollinger_reversion_fires_below_the_lower_band_above_the_200_day() {
    let mut closes: Vec<f64> = (0..300).map(|index| 100.0 + 0.2 * index as f64).collect();
    let last = closes.len() - 1;
    closes[last] -= 6.0;
    let bars = bars_from_closes(&closes, 0.3);
    assert_eq!(
        fires_at_last(SetupId::BollingerReversion, &bars),
        Some(Side::Long)
    );
}

#[test]
fn bollinger_reversion_does_not_fire_at_the_average() {
    let closes: Vec<f64> = (0..300).map(|index| 100.0 + 0.2 * index as f64).collect();
    let bars = bars_from_closes(&closes, 0.3);
    assert_eq!(fires_at_last(SetupId::BollingerReversion, &bars), None);
}

fn inside_bars(inside: bool) -> Vec<Bar> {
    let mut bars = bars_from_closes(&trend_closes(), 0.2);
    let last = bars.len() - 1;
    bars[last - 1].high += 2.0;
    bars[last - 1].low -= 2.0;
    if inside {
        bars[last].high = bars[last - 1].high - 0.2;
        bars[last].low = bars[last - 1].low + 0.2;
    } else {
        bars[last].high = bars[last - 1].high + 0.2;
        bars[last].low = bars[last - 1].low - 0.2;
    }
    bars
}

#[test]
fn inside_bar_fires_in_the_direction_of_the_average() {
    let bars = inside_bars(true);
    assert_eq!(
        fires_at_last(SetupId::InsideBarBreak, &bars),
        Some(Side::Long)
    );
}

#[test]
fn inside_bar_does_not_fire_outside_the_previous_range() {
    let bars = inside_bars(false);
    assert_eq!(fires_at_last(SetupId::InsideBarBreak, &bars), None);
}

#[test]
fn three_day_reversal_fires_after_three_lower_closes_above_the_200_day() {
    let mut closes = trend_closes();
    let last = closes.len() - 1;
    let start = closes[last - 3];
    closes[last - 2] = start - 0.4;
    closes[last - 1] = start - 0.8;
    closes[last] = start - 1.2;
    let bars = bars_from_closes(&closes, 0.3);
    assert_eq!(
        fires_at_last(SetupId::ThreeDayReversal, &bars),
        Some(Side::Long)
    );
}

#[test]
fn three_day_reversal_does_not_fire_when_closes_rise() {
    let mut closes = trend_closes();
    let last = closes.len() - 1;
    closes[last - 2] = closes[last - 3] + 0.2;
    closes[last - 1] = closes[last - 2] + 0.2;
    closes[last] = closes[last - 1] + 0.2;
    let bars = bars_from_closes(&closes, 0.3);
    assert_eq!(fires_at_last(SetupId::ThreeDayReversal, &bars), None);
}

#[test]
fn short_mirrors_fire_in_a_downtrend() {
    let mut closes: Vec<f64> = (0..260).map(|index| 200.0 - 0.2 * index as f64).collect();
    let last = closes.len() - 1;
    closes[last - 1] += 1.0;
    closes[last] = closes[last - 1] + 2.0;
    let bars = bars_from_closes(&closes, 0.4);
    assert_eq!(
        fires_at_last(SetupId::TrendPullback, &bars),
        Some(Side::Short)
    );
}
