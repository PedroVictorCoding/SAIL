//! The six fixed daily-bar setups.

use super::indicators::{bollinger, previous_extreme, rolling_range, rsi, sma};
use super::{Bar, SetupId, Side};

pub(super) fn fire(setup: SetupId, bars: &[Bar]) -> Vec<(usize, Side)> {
    match setup {
        SetupId::TrendPullback => trend_pullback(bars),
        SetupId::SqueezeBreakout => squeeze_breakout(bars),
        SetupId::Donchian55 => donchian55(bars),
        SetupId::BollingerReversion => bollinger_reversion(bars),
        SetupId::InsideBarBreak => inside_bar_break(bars),
        SetupId::ThreeDayReversal => three_day_reversal(bars),
    }
}

fn trend_pullback(bars: &[Bar]) -> Vec<(usize, Side)> {
    let fast = sma(bars, 50);
    let slow = sma(bars, 200);
    let rsi = rsi(bars, 2);
    let mut firings = Vec::new();
    for index in 0..bars.len() {
        let (Some(fast), Some(slow), Some(rsi)) = (fast[index], slow[index], rsi[index]) else {
            continue;
        };
        let close = bars[index].close;
        if close > fast && fast > slow && rsi < 10.0 {
            firings.push((index, Side::Long));
        } else if close < fast && fast < slow && rsi > 90.0 {
            firings.push((index, Side::Short));
        }
    }
    firings
}

fn squeeze_breakout(bars: &[Bar]) -> Vec<(usize, Side)> {
    let ranges = rolling_range(bars, 20);
    let mut firings = Vec::new();
    for index in 0..bars.len() {
        if index < 78 {
            continue;
        }
        let Some(current) = ranges[index] else {
            continue;
        };
        let narrowest = (index - 59..index)
            .all(|earlier| ranges[earlier].is_none_or(|range| current <= range + f64::EPSILON));
        if !narrowest {
            continue;
        }
        let Some(high) = previous_extreme(bars, index, 20, true) else {
            continue;
        };
        let Some(low) = previous_extreme(bars, index, 20, false) else {
            continue;
        };
        let close = bars[index].close;
        if close > high {
            firings.push((index, Side::Long));
        } else if close < low {
            firings.push((index, Side::Short));
        }
    }
    firings
}

fn donchian55(bars: &[Bar]) -> Vec<(usize, Side)> {
    let slow = sma(bars, 200);
    let mut firings = Vec::new();
    for index in 55..bars.len() {
        if index == 0 {
            continue;
        }
        let (Some(now), Some(previous)) = (slow[index], slow[index - 1]) else {
            continue;
        };
        let close = bars[index].close;
        let Some(high) = previous_extreme(bars, index, 55, true) else {
            continue;
        };
        let Some(low) = previous_extreme(bars, index, 55, false) else {
            continue;
        };
        if close > high && now > previous {
            firings.push((index, Side::Long));
        } else if close < low && now < previous {
            firings.push((index, Side::Short));
        }
    }
    firings
}

fn bollinger_reversion(bars: &[Bar]) -> Vec<(usize, Side)> {
    let bands = bollinger(bars, 20);
    let slow = sma(bars, 200);
    let mut firings = Vec::new();
    for index in 0..bars.len() {
        let (Some((_, lower, upper)), Some(slow)) = (bands[index], slow[index]) else {
            continue;
        };
        let close = bars[index].close;
        if close < lower && close > slow {
            firings.push((index, Side::Long));
        } else if close > upper && close < slow {
            firings.push((index, Side::Short));
        }
    }
    firings
}

fn inside_bar_break(bars: &[Bar]) -> Vec<(usize, Side)> {
    let average = sma(bars, 20);
    let mut firings = Vec::new();
    for index in 1..bars.len() {
        let Some(now) = average[index] else {
            continue;
        };
        let Some(previous) = average[index - 1] else {
            continue;
        };
        let bar = bars[index];
        let earlier = bars[index - 1];
        if bar.high >= earlier.high || bar.low <= earlier.low {
            continue;
        }
        if now > previous {
            firings.push((index, Side::Long));
        } else if now < previous {
            firings.push((index, Side::Short));
        }
    }
    firings
}

fn three_day_reversal(bars: &[Bar]) -> Vec<(usize, Side)> {
    let slow = sma(bars, 200);
    let mut firings = Vec::new();
    for index in 2..bars.len() {
        let Some(slow) = slow[index] else {
            continue;
        };
        let close = bars[index].close;
        let one = bars[index - 1].close;
        let two = bars[index - 2].close;
        if close < one && one < two && close > slow {
            firings.push((index, Side::Long));
        } else if close > one && one > two && close < slow {
            firings.push((index, Side::Short));
        }
    }
    firings
}
