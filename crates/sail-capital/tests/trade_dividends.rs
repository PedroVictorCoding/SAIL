//! Round 24 K1: dividend-adjusted triggers and dividend cash flows in the replay.

mod trade_support;

use sail_capital::trade::{
    AssetClass, Bar, Costs, Exit, InstrumentContext, Region, SetupId, Side, adjust, fire, fire_for,
    relative_volume, replay,
};
use trade_support::{date_at, instrument};

fn bar(index: usize, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        session: date_at(index),
        open,
        high,
        low,
        close,
        volume: None,
        dividend: None,
    }
}

/// A slowly rising series whose last session drops 5% either as a dividend (adjusted
/// away) or as a plain fall (still a breakdown).
fn five_percent_drop(as_dividend: bool) -> Vec<Bar> {
    let mut bars = Vec::new();
    let mut close = 100.0;
    for index in 0..260 {
        let previous = close;
        let last = index == 259;
        close = if last {
            previous * 0.95
        } else {
            previous + 0.1
        };
        let mut next = bar(
            index,
            previous,
            previous.max(close) + 0.2,
            previous.min(close) - 0.2,
            close,
        );
        if last && as_dividend {
            next.dividend = Some(previous * 0.05);
        }
        bars.push(next);
    }
    bars
}

#[test]
fn a_five_percent_dividend_no_longer_fires_a_bollinger_reversal() {
    let with_dividend = five_percent_drop(true);
    let without_dividend = five_percent_drop(false);
    let plain = fire(SetupId::BollingerReversion, &without_dividend);
    assert!(
        plain.contains(&(259, Side::Long)),
        "the raw drop is a breakdown: {plain:?}"
    );
    let adjusted = fire(SetupId::BollingerReversion, &with_dividend);
    assert!(
        !adjusted.iter().any(|(index, _)| *index == 259),
        "the dividend is not a breakdown: {adjusted:?}"
    );
    assert!(
        !fire_for(
            SetupId::BollingerReversion,
            &instrument("X", "X.US", AssetClass::Stock, false, with_dividend)
        )
        .iter()
        .any(|(index, _)| *index == 259)
    );
}

#[test]
fn adjust_scales_only_history_before_an_ex_date() {
    let bars = five_percent_drop(true);
    let adjusted = adjust(&bars);
    assert_eq!(adjusted.bars.len(), bars.len());
    assert_eq!(adjusted.factor.last().copied(), Some(1.0));
    assert!((adjusted.factor[258] - 0.95).abs() < 1e-12);
    for window in adjusted.factor.windows(2) {
        assert!(window[0] <= window[1] + 1e-12);
    }
    assert!((adjusted.bars[258].close - bars[258].close * 0.95).abs() < 1e-12);
    assert!((adjusted.bars[259].close - bars[259].close).abs() < 1e-12);
}

#[test]
fn adjust_ignores_a_dividend_with_no_usable_previous_close() {
    let mut bars = vec![
        bar(0, 100.0, 101.0, 99.0, 100.0),
        bar(1, 100.0, 101.0, 99.0, 100.0),
        bar(2, 100.0, 101.0, 99.0, 100.0),
    ];
    bars[1].dividend = Some(100.0);
    bars[2].dividend = Some(0.0);
    let adjusted = adjust(&bars);
    assert_eq!(adjusted.factor, vec![1.0, 1.0, 1.0]);
}

/// A long volume breakout that holds to its time stop, with one ex-date inside.
fn breakout(stop_gap: bool) -> sail_capital::trade::Instrument {
    let mut bars: Vec<Bar> = (0..60)
        .map(|index| bar(index, 100.0, 100.5, 99.5, 100.0))
        .collect();
    let mut spike = bar(60, 100.0, 111.0, 99.5, 110.0);
    spike.volume = Some(5_000.0);
    bars.push(spike);
    for index in 61..=80 {
        let mut next = bar(index, 110.0, 110.0, 110.0, 110.0);
        next.volume = Some(1_000.0);
        if index == 65 {
            next.dividend = Some(if stop_gap { 10.0 } else { 1.0 });
            if stop_gap {
                next.open = 100.0;
                next.high = 100.5;
                next.low = 99.0;
                next.close = 100.0;
            }
        }
        bars.push(next);
    }
    for earlier in &mut bars[..60] {
        earlier.volume = Some(1_000.0);
    }
    let mut instrument = instrument("X", "X.US", AssetClass::Stock, false, bars);
    instrument.context = InstrumentContext {
        region: Region::Us,
        earnings_events: Vec::new(),
        withholding: 0.15,
    };
    instrument
}

fn breakout_trade(instrument: &sail_capital::trade::Instrument) -> sail_capital::trade::Trade {
    replay(instrument, trade_support::zero_costs())
        .into_iter()
        .find(|trade| trade.setup == SetupId::VolumeBreakout && trade.side == Side::Long)
        .expect("a volume breakout")
}

#[test]
fn a_long_earns_the_dividend_net_of_withholding_inside_r() {
    let instrument = breakout(false);
    let trade = breakout_trade(&instrument);
    assert_eq!(trade.exit, Exit::Time);
    let risk = (trade.entry - trade.stop).abs();
    let expected = 0.85 / risk;
    assert!(
        (trade.dividend_r - expected).abs() < 1e-12,
        "{} vs {}",
        trade.dividend_r,
        expected
    );
    assert!((trade.r_multiple - expected).abs() < 1e-12);
    assert!(!trade.ex_date_gap);
}

#[test]
fn a_short_pays_the_dividend_inside_r() {
    let mut bars: Vec<Bar> = (0..60)
        .map(|index| bar(index, 100.0, 100.5, 99.5, 100.0))
        .collect();
    let mut spike = bar(60, 100.0, 100.5, 89.5, 90.0);
    spike.volume = Some(5_000.0);
    bars.push(spike);
    for index in 61..=80 {
        let mut next = bar(index, 90.0, 90.0, 90.0, 90.0);
        next.volume = Some(1_000.0);
        if index == 65 {
            next.dividend = Some(1.0);
        }
        bars.push(next);
    }
    for earlier in &mut bars[..60] {
        earlier.volume = Some(1_000.0);
    }
    let mut instrument = instrument("X", "X.US", AssetClass::Stock, false, bars);
    instrument.context = InstrumentContext {
        region: Region::Us,
        earnings_events: Vec::new(),
        withholding: 0.15,
    };
    let trade = replay(&instrument, trade_support::zero_costs())
        .into_iter()
        .find(|trade| trade.setup == SetupId::VolumeBreakout && trade.side == Side::Short)
        .expect("a short volume breakout");
    assert_eq!(trade.exit, Exit::Time);
    let risk = (trade.entry - trade.stop).abs();
    let expected = -1.0 / risk;
    assert!(
        (trade.dividend_r - expected).abs() < 1e-12,
        "{} vs {}",
        trade.dividend_r,
        expected
    );
    assert!(trade.dividend_r < 0.0);
}

#[test]
fn an_ex_date_gap_still_fills_the_stop_at_the_raw_open() {
    let instrument = breakout(true);
    let trade = breakout_trade(&instrument);
    assert_eq!(trade.exit, Exit::Stop);
    assert_eq!(trade.entry, 110.0);
    assert!((trade.stop - (110.0 - (24.5 / 14.0) * 2.0)).abs() < 1e-12);
    assert!(
        trade.ex_date_gap,
        "the stop was gapped through on an ex-date"
    );
    assert_eq!(trade.exit_session, date_at(65));
}

#[test]
fn volume_setups_never_fire_without_volume() {
    let bars: Vec<Bar> = (0..120)
        .map(|index| bar(index, 100.0, 101.0, 99.0, 100.0))
        .collect();
    let no_volume = instrument("X", "X.US", AssetClass::Stock, false, bars.clone());
    assert!(fire_for(SetupId::VolumeBreakout, &no_volume).is_empty());
    assert!(fire_for(SetupId::EarningsGapDrift, &no_volume).is_empty());
    let mut fx_bars = bars.clone();
    for (index, bar) in fx_bars.iter_mut().enumerate() {
        bar.volume = Some(1_000.0 + index as f64);
    }
    let fx = instrument("EURUSD", "EURUSD", AssetClass::Fx, true, fx_bars);
    assert!(fire_for(SetupId::VolumeBreakout, &fx).is_empty());
}

#[test]
fn an_instrument_without_dividends_has_no_cash_flow() {
    let instrument = breakout(false);
    let trades = replay(&instrument, Costs::default_for(AssetClass::Stock));
    assert!(trades.iter().any(|trade| trade.dividend_r > 0.0));
    let mut plain = instrument.clone();
    for bar in &mut plain.bars {
        bar.dividend = None;
    }
    let trades = replay(&plain, Costs::default_for(AssetClass::Stock));
    assert!(trades.iter().all(|trade| trade.dividend_r == 0.0));
    assert!(trades.iter().all(|trade| !trade.ex_date_gap));
}

#[test]
fn replay_from_matches_a_full_replay_around_a_dividend() {
    let instrument = breakout(false);
    let costs = Costs::default_for(AssetClass::Stock);
    let full = replay(&instrument, costs);
    let from = instrument.bars[70].session;
    let tail = sail_capital::trade::replay_from(&instrument, costs, from);
    let prefix: Vec<_> = full
        .iter()
        .filter(|trade| trade.signal_session < from)
        .cloned()
        .collect();
    let mut combined = prefix;
    combined.extend(tail);
    combined.sort_by_key(|trade| {
        (
            trade.signal_session,
            trade.entry_session,
            trade.setup,
            trade.side,
        )
    });
    let mut expected = full;
    expected.sort_by_key(|trade| {
        (
            trade.signal_session,
            trade.entry_session,
            trade.setup,
            trade.side,
        )
    });
    assert_eq!(combined, expected);
}

#[test]
fn adjust_and_relative_volume_are_pure() {
    let bars = five_percent_drop(true);
    assert_eq!(adjust(&bars), adjust(&bars));

    let mut volumes: Vec<Bar> = (0..40)
        .map(|index| {
            let mut bar = bar(index, 100.0, 101.0, 99.0, 100.0);
            bar.volume = Some(1_000.0);
            bar
        })
        .collect();
    volumes[39].volume = Some(2_000.0);
    assert_eq!(relative_volume(&volumes, 39, 20), Some(2.0));
    assert_eq!(
        relative_volume(&volumes, 39, 20),
        relative_volume(&volumes, 39, 20)
    );
    volumes[39].volume = Some(1_900.0);
    assert_eq!(relative_volume(&volumes, 39, 20), Some(1.9));
    volumes[20].volume = None;
    assert_eq!(relative_volume(&volumes, 39, 20), None);
    assert_eq!(relative_volume(&volumes, 19, 20), None);
}
