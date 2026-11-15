//! Pure daily-bar indicators used by the trade setups.

use std::collections::VecDeque;

use super::Bar;

pub(super) fn true_range(bars: &[Bar], index: usize) -> f64 {
    let bar = bars[index];
    if index == 0 {
        return bar.high - bar.low;
    }
    let previous = bars[index - 1].close;
    (bar.high - bar.low)
        .max((bar.high - previous).abs())
        .max((bar.low - previous).abs())
}

pub(super) fn atr(bars: &[Bar], period: usize) -> Vec<Option<f64>> {
    let mut values = vec![None; bars.len()];
    if period == 0 || bars.len() <= period {
        return values;
    }
    let mut sum = 0.0;
    for index in 1..=period {
        sum += true_range(bars, index);
    }
    let mut value = sum / period as f64;
    values[period] = Some(value);
    for (index, slot) in values.iter_mut().enumerate().skip(period + 1) {
        value = (value * (period as f64 - 1.0) + true_range(bars, index)) / period as f64;
        *slot = Some(value);
    }
    values
}

pub(super) fn sma(bars: &[Bar], period: usize) -> Vec<Option<f64>> {
    let mut values = vec![None; bars.len()];
    if period == 0 || bars.len() < period {
        return values;
    }
    let mut sum = 0.0;
    for index in 0..bars.len() {
        sum += bars[index].close;
        if index >= period {
            sum -= bars[index - period].close;
        }
        if index + 1 >= period {
            values[index] = Some(sum / period as f64);
        }
    }
    values
}

pub(super) fn rsi(bars: &[Bar], period: usize) -> Vec<Option<f64>> {
    let mut values = vec![None; bars.len()];
    if period == 0 || bars.len() <= period {
        return values;
    }
    let mut gains = 0.0;
    let mut losses = 0.0;
    for index in 1..=period {
        let change = bars[index].close - bars[index - 1].close;
        if change >= 0.0 {
            gains += change;
        } else {
            losses -= change;
        }
    }
    let mut average_gain = gains / period as f64;
    let mut average_loss = losses / period as f64;
    values[period] = Some(rsi_value(average_gain, average_loss));
    for index in period + 1..bars.len() {
        let change = bars[index].close - bars[index - 1].close;
        let (gain, loss) = if change >= 0.0 {
            (change, 0.0)
        } else {
            (0.0, -change)
        };
        let weight = period as f64 - 1.0;
        average_gain = (average_gain * weight + gain) / period as f64;
        average_loss = (average_loss * weight + loss) / period as f64;
        values[index] = Some(rsi_value(average_gain, average_loss));
    }
    values
}

fn rsi_value(average_gain: f64, average_loss: f64) -> f64 {
    if average_loss == 0.0 {
        if average_gain == 0.0 { 50.0 } else { 100.0 }
    } else {
        100.0 - 100.0 / (1.0 + average_gain / average_loss)
    }
}

pub(super) fn bollinger(bars: &[Bar], period: usize) -> Vec<Option<(f64, f64, f64)>> {
    let mut values = vec![None; bars.len()];
    if period == 0 || bars.len() < period {
        return values;
    }
    let mut sum = 0.0;
    let mut squares = 0.0;
    for index in 0..bars.len() {
        let close = bars[index].close;
        sum += close;
        squares += close * close;
        if index >= period {
            let old = bars[index - period].close;
            sum -= old;
            squares -= old * old;
        }
        if index + 1 >= period {
            let mean = sum / period as f64;
            let variance = (squares / period as f64 - mean * mean).max(0.0);
            let sigma = variance.sqrt();
            values[index] = Some((mean, mean - 2.0 * sigma, mean + 2.0 * sigma));
        }
    }
    values
}

pub(super) fn rolling_range(bars: &[Bar], period: usize) -> Vec<Option<f64>> {
    let mut values = vec![None; bars.len()];
    if period == 0 || bars.len() < period {
        return values;
    }
    let mut highs: VecDeque<usize> = VecDeque::new();
    let mut lows: VecDeque<usize> = VecDeque::new();
    for index in 0..bars.len() {
        while highs
            .back()
            .is_some_and(|&back| bars[back].high <= bars[index].high)
        {
            highs.pop_back();
        }
        highs.push_back(index);
        while lows
            .back()
            .is_some_and(|&back| bars[back].low >= bars[index].low)
        {
            lows.pop_back();
        }
        lows.push_back(index);
        if index + 1 >= period {
            let start = index + 1 - period;
            while highs.front().is_some_and(|&front| front < start) {
                highs.pop_front();
            }
            while lows.front().is_some_and(|&front| front < start) {
                lows.pop_front();
            }
            let high = bars[*highs.front().expect("a high")].high;
            let low = bars[*lows.front().expect("a low")].low;
            values[index] = Some(high - low);
        }
    }
    values
}

pub(super) fn previous_extreme(
    bars: &[Bar],
    index: usize,
    period: usize,
    high: bool,
) -> Option<f64> {
    if index < period {
        return None;
    }
    let slice = &bars[index - period..index];
    if high {
        slice.iter().map(|bar| bar.high).reduce(f64::max)
    } else {
        slice.iter().map(|bar| bar.low).reduce(f64::min)
    }
}

/// Round 24 (Kilo, A2): see [`super::Adjusted`]. M0 stub: the bars unchanged, factor 1.
pub(super) fn adjust(bars: &[Bar]) -> super::Adjusted {
    super::Adjusted {
        bars: bars.to_vec(),
        factor: vec![1.0; bars.len()],
    }
}

/// Round 24 (Kilo, A1), *pure*: `volume[index] / median(volume[index − period .. index])`.
/// `None` when `index < period`, when any of those volumes (or today's) is missing, or
/// when the median is not positive.
pub(super) fn relative_volume(bars: &[Bar], index: usize, period: usize) -> Option<f64> {
    let _ = (bars, index, period);
    None
}
