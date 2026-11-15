//! Replay, per-trade simulation, grading and the protective holding guard.

use chrono::NaiveDate;

use super::indicators::{adjust, atr};
use super::setups::fire_for_from;
use super::{Bar, Costs, Exit, HoldingGuard, Instrument, SetupId, Side, SignalCard, Trade};

const ENTRY_ZONE_ATR: f64 = 0.25;

pub(super) fn replay(instrument: &Instrument, costs: Costs) -> Vec<Trade> {
    replay_since(instrument, costs, None)
}

pub(super) fn replay_from(instrument: &Instrument, costs: Costs, from: NaiveDate) -> Vec<Trade> {
    replay_since(instrument, costs, Some(from))
}

fn replay_since(instrument: &Instrument, costs: Costs, from: Option<NaiveDate>) -> Vec<Trade> {
    let bars = &instrument.bars;
    // Triggers and the ATR see dividend-adjusted bars; fills and stops stay raw. The ATR
    // is converted back to raw units at each firing (`Adjusted` documents the factor).
    let adjusted = adjust(bars);
    let adjusted_atr = atr(&adjusted.bars, 14);
    let mut trades = Vec::new();
    for setup in SetupId::ALL {
        // Indicators always see every bar (the running sums in `sma` and `bollinger`
        // would round differently from a shorter start and could flip a firing); only
        // the per-session conditions before `from` are skipped.
        let start = from.map_or(0, |from| bars.partition_point(|bar| bar.session < from));
        let firings = fire_for_from(setup, instrument, start);
        for (index, side) in firings {
            // Only firings from `from` on are simulated; earlier ones come from the cache.
            if from.is_some_and(|from| bars[index].session < from) {
                continue;
            }
            let Some(entry_index) = index.checked_add(1) else {
                continue;
            };
            let Some(next) = bars.get(entry_index) else {
                continue;
            };
            let Some(value) = adjusted_atr[index] else {
                continue;
            };
            let factor = adjusted.factor[index];
            if !(factor.is_finite() && factor > 0.0) {
                continue;
            }
            let atr = value / factor;
            if !(atr.is_finite() && atr > 0.0) {
                continue;
            }
            let close = bars[index].close;
            let zone = (close - ENTRY_ZONE_ATR * atr, close + ENTRY_ZONE_ATR * atr);
            let entry = next.open;
            if entry < zone.0 || entry > zone.1 {
                continue;
            }
            if setup == SetupId::InsideBarBreak
                && !match side {
                    Side::Long => entry > bars[index].high,
                    Side::Short => entry < bars[index].low,
                }
            {
                continue;
            }
            let (stop_atr, max_sessions) = setup.plan();
            let risk = stop_atr * atr;
            if !(risk.is_finite() && risk > 0.0) {
                continue;
            }
            let stop = match side {
                Side::Long => entry - risk,
                Side::Short => entry + risk,
            };
            let targets = match side {
                Side::Long => [entry + risk, entry + 2.0 * risk, entry + 3.0 * risk],
                Side::Short => [entry - risk, entry - 2.0 * risk, entry - 3.0 * risk],
            };
            let Some(trade) = simulate(
                &instrument.key,
                setup,
                side,
                bars[index].session,
                bars,
                entry_index,
                entry,
                stop,
                targets,
                max_sessions,
                costs,
                instrument.needs_cfd(side),
                instrument.foreign_currency,
                instrument.context.withholding,
                true,
            ) else {
                continue;
            };
            trades.push(trade);
        }
    }
    trades
}

#[allow(clippy::too_many_arguments)]
pub(super) fn simulate(
    key: &str,
    setup: SetupId,
    side: Side,
    signal_session: NaiveDate,
    bars: &[Bar],
    entry_index: usize,
    entry: f64,
    stop: f64,
    targets: [f64; 3],
    max_sessions: u32,
    costs: Costs,
    needs_cfd: bool,
    foreign: bool,
    withholding: f64,
    pay_dividends: bool,
) -> Option<Trade> {
    if max_sessions == 0 || entry_index >= bars.len() {
        return None;
    }
    let risk = (entry - stop).abs();
    if !(risk.is_finite() && risk > 0.0) {
        return None;
    }
    let direction = if side == Side::Long { 1.0 } else { -1.0 };
    let entry_session = bars[entry_index].session;
    let initial_stop = stop;
    let last_index = entry_index
        .checked_add(max_sessions as usize)?
        .checked_sub(1)?;
    let mut stop = stop;
    let mut remaining = 1.0;
    let mut gross_r = 0.0;
    let mut targets_hit = 0usize;
    let mut dividend_r = 0.0;
    let mut ex_date_gap = false;
    let withholding = withholding.clamp(0.0, 1.0);

    for (index, bar) in bars.iter().enumerate().skip(entry_index) {
        let bar = *bar;
        // Units still open at the ex-date's open earn the cash dividend (long, net of
        // withholding) or pay it (short CFD). Entering on the ex-date itself does not.
        if pay_dividends
            && index > entry_index
            && let Some(dividend) = bar.dividend
            && dividend.is_finite()
            && dividend > 0.0
        {
            let credit = match side {
                Side::Long => dividend * (1.0 - withholding),
                Side::Short => -dividend,
            };
            dividend_r += remaining * credit / risk;
        }
        if let Some(fill) = stop_fill(bar, side, stop) {
            let gapped = match side {
                Side::Long => bar.open <= stop,
                Side::Short => bar.open >= stop,
            };
            ex_date_gap = gapped && bar.dividend.is_some_and(|dividend| dividend > 0.0);
            gross_r += remaining * direction * (fill - entry) / risk;
            let exit = match targets_hit {
                0 => Exit::Stop,
                1 => Exit::Target1,
                2 => Exit::Target2,
                _ => Exit::Target3,
            };
            return Some(closed(
                key,
                setup,
                side,
                signal_session,
                entry_session,
                entry,
                initial_stop,
                bar.session,
                exit,
                gross_r,
                costs,
                needs_cfd,
                foreign,
                risk,
                dividend_r,
                ex_date_gap,
            ));
        }

        while targets_hit < 3 {
            let target = targets[targets_hit];
            let reached = match side {
                Side::Long => bar.high >= target,
                Side::Short => bar.low <= target,
            };
            if !reached {
                break;
            }
            gross_r += direction * (target - entry) / risk / 3.0;
            remaining -= 1.0 / 3.0;
            targets_hit += 1;
            if targets_hit == 1 {
                stop = entry;
            }
        }

        if targets_hit == 3 {
            return Some(closed(
                key,
                setup,
                side,
                signal_session,
                entry_session,
                entry,
                initial_stop,
                bar.session,
                Exit::Target3,
                gross_r,
                costs,
                needs_cfd,
                foreign,
                risk,
                dividend_r,
                ex_date_gap,
            ));
        }

        if index >= last_index {
            gross_r += remaining * direction * (bar.close - entry) / risk;
            let exit = match targets_hit {
                0 => Exit::Time,
                1 => Exit::Target1,
                _ => Exit::Target2,
            };
            return Some(closed(
                key,
                setup,
                side,
                signal_session,
                entry_session,
                entry,
                initial_stop,
                bar.session,
                exit,
                gross_r,
                costs,
                needs_cfd,
                foreign,
                risk,
                dividend_r,
                ex_date_gap,
            ));
        }
    }
    None
}

fn stop_fill(bar: Bar, side: Side, stop: f64) -> Option<f64> {
    match side {
        Side::Long if bar.open <= stop => Some(bar.open),
        Side::Long if bar.low <= stop => Some(stop),
        Side::Short if bar.open >= stop => Some(bar.open),
        Side::Short if bar.high >= stop => Some(stop),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn closed(
    key: &str,
    setup: SetupId,
    side: Side,
    signal_session: NaiveDate,
    entry_session: NaiveDate,
    entry: f64,
    stop: f64,
    exit_session: NaiveDate,
    exit: Exit,
    gross_r: f64,
    costs: Costs,
    needs_cfd: bool,
    foreign: bool,
    risk: f64,
    dividend_r: f64,
    ex_date_gap: bool,
) -> Trade {
    let conversion = if foreign && !needs_cfd {
        costs.fx_conversion
    } else {
        0.0
    };
    let days = (exit_session - entry_session).num_days().max(0) as f64;
    let financing = if needs_cfd {
        costs.cfd_daily_financing * days * entry / risk
    } else {
        0.0
    };
    Trade {
        key: key.into(),
        setup,
        side,
        signal_session,
        entry_session,
        entry,
        stop,
        exit_session,
        exit,
        r_multiple: gross_r + dividend_r
            - (costs.round_trip + conversion) * entry / risk
            - financing,
        dividend_r,
        ex_date_gap,
        regime: None,
    }
}

pub(super) fn grade(card: &SignalCard, bars_after: &[Bar], costs: Costs) -> Option<Trade> {
    let first = bars_after.first()?;
    let entry = first.open;
    if entry < card.entry_zone.0 || entry > card.entry_zone.1 {
        return None;
    }
    simulate(
        &card.key,
        card.setup,
        card.side,
        card.signal_session,
        bars_after,
        0,
        entry,
        card.stop,
        card.targets,
        card.max_sessions,
        costs,
        card.needs_cfd,
        !card.needs_cfd,
        0.0,
        false,
    )
}

pub(super) fn guard(instrument: &Instrument) -> Option<HoldingGuard> {
    let bars = &instrument.bars;
    let last = bars.last()?;
    let atr = atr(bars, 22);
    let value = atr.last().copied().flatten()?;
    if !(value.is_finite() && value > 0.0) || bars.len() < 22 {
        return None;
    }
    let start = bars.len() - 22;
    let highest = bars[start..]
        .iter()
        .map(|bar| bar.close)
        .fold(f64::NEG_INFINITY, f64::max);
    let trailing_stop = highest - 3.0 * value;
    let close = last.close;
    let stop_distance_pct = if close == 0.0 {
        0.0
    } else {
        (close - trailing_stop).abs() / close * 100.0
    };
    let above_200d = if bars.len() >= 200 {
        let average = bars[bars.len() - 200..]
            .iter()
            .map(|bar| bar.close)
            .sum::<f64>()
            / 200.0;
        Some(close > average)
    } else {
        None
    };
    let next_earnings = instrument
        .earnings
        .iter()
        .copied()
        .filter(|date| *date > last.session)
        .min();
    Some(HoldingGuard {
        key: instrument.key.clone(),
        symbol: instrument.symbol.clone(),
        close,
        trailing_stop,
        stop_distance_pct,
        stop_hit: close < trailing_stop,
        above_200d,
        next_earnings,
    })
}
