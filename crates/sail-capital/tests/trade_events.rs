//! Round 24 K3: the two event/volume setups.

mod trade_support;

use sail_capital::trade::{
    AssetClass, Bar, EarningsEvent, Instrument, InstrumentContext, Region, SetupId, Side, fire_for,
};
use trade_support::{date_at, instrument};

fn bar(index: usize, open: f64, high: f64, low: f64, close: f64, volume: f64) -> Bar {
    Bar {
        session: date_at(index),
        open,
        high,
        low,
        close,
        volume: Some(volume),
        dividend: None,
    }
}

/// Flat bars, then a gap session with a volume spike at `index`.
fn gap_bars(index: usize, spike_volume: f64) -> Vec<Bar> {
    let mut bars: Vec<Bar> = (0..index)
        .map(|at| bar(at, 100.0, 101.0, 99.0, 100.0, 1_000.0))
        .collect();
    bars.push(bar(index, 105.0, 106.0, 104.0, 105.0, spike_volume));
    bars
}

fn with_events(bars: Vec<Bar>, events: Vec<EarningsEvent>) -> Instrument {
    let mut instrument = instrument("X", "X.US", AssetClass::Stock, false, bars);
    instrument.context = InstrumentContext {
        region: Region::Us,
        earnings_events: events,
        withholding: 0.0,
    };
    instrument
}

fn positive_event(index: usize) -> EarningsEvent {
    EarningsEvent {
        date: date_at(index - 1),
        after_close: Some(true),
        surprise_pct: Some(10.0),
    }
}

#[test]
fn an_agreeing_gap_on_volume_fires_long() {
    let instrument = with_events(gap_bars(80, 5_000.0), vec![positive_event(80)]);
    assert_eq!(
        fire_for(SetupId::EarningsGapDrift, &instrument),
        vec![(80, Side::Long)]
    );
}

#[test]
fn an_unknown_or_opposite_surprise_does_not_fire() {
    let mut unknown = with_events(gap_bars(80, 5_000.0), vec![positive_event(80)]);
    unknown.context.earnings_events[0].surprise_pct = None;
    assert!(fire_for(SetupId::EarningsGapDrift, &unknown).is_empty());

    let mut opposite = with_events(gap_bars(80, 5_000.0), vec![positive_event(80)]);
    opposite.context.earnings_events[0].surprise_pct = Some(-10.0);
    assert!(fire_for(SetupId::EarningsGapDrift, &opposite).is_empty());

    let mut zero = with_events(gap_bars(80, 5_000.0), vec![positive_event(80)]);
    zero.context.earnings_events[0].surprise_pct = Some(0.0);
    assert!(fire_for(SetupId::EarningsGapDrift, &zero).is_empty());
}

#[test]
fn a_gap_below_one_atr_or_volume_below_two_does_not_fire() {
    let mut small_gap = gap_bars(80, 5_000.0);
    small_gap[80] = bar(80, 101.5, 102.0, 101.0, 101.5, 5_000.0);
    let instrument = with_events(small_gap, vec![positive_event(80)]);
    assert!(fire_for(SetupId::EarningsGapDrift, &instrument).is_empty());

    // The median of the previous 20 volumes is 1000, so 1900 is 1.9×.
    let quiet = with_events(gap_bars(80, 1_900.0), vec![positive_event(80)]);
    assert!(fire_for(SetupId::EarningsGapDrift, &quiet).is_empty());

    // Exactly 2× is enough.
    let enough = with_events(gap_bars(80, 2_000.0), vec![positive_event(80)]);
    assert_eq!(
        fire_for(SetupId::EarningsGapDrift, &enough),
        vec![(80, Side::Long)]
    );
}

#[test]
fn the_earnings_setup_is_for_stocks_and_etfs_only() {
    let mut fx = with_events(gap_bars(80, 5_000.0), vec![positive_event(80)]);
    fx.class = AssetClass::Fx;
    assert!(fire_for(SetupId::EarningsGapDrift, &fx).is_empty());
    let mut etf = with_events(gap_bars(80, 5_000.0), vec![positive_event(80)]);
    etf.class = AssetClass::Etf;
    assert_eq!(
        fire_for(SetupId::EarningsGapDrift, &etf),
        vec![(80, Side::Long)]
    );
}

#[test]
fn a_reaction_uses_the_first_session_after_the_date() {
    let mut instrument = with_events(gap_bars(80, 5_000.0), vec![positive_event(80)]);
    // Reported before the open of the gap session: it is that session's reaction.
    instrument.context.earnings_events[0].date = date_at(80);
    instrument.context.earnings_events[0].after_close = Some(false);
    assert_eq!(
        fire_for(SetupId::EarningsGapDrift, &instrument),
        vec![(80, Side::Long)]
    );
    // Reported after the close: the reaction is the next session, which does not exist.
    instrument.context.earnings_events[0].after_close = Some(true);
    assert!(fire_for(SetupId::EarningsGapDrift, &instrument).is_empty());
}

#[test]
fn a_volume_breakout_fires_both_ways() {
    let mut long: Vec<Bar> = (0..60)
        .map(|index| bar(index, 100.0, 100.5, 99.5, 100.0, 1_000.0))
        .collect();
    long.push(bar(60, 100.0, 111.0, 99.5, 110.0, 5_000.0));
    assert_eq!(
        fire_for(
            SetupId::VolumeBreakout,
            &instrument("X", "X.US", AssetClass::Stock, false, long)
        ),
        vec![(60, Side::Long)]
    );

    let mut short: Vec<Bar> = (0..60)
        .map(|index| bar(index, 100.0, 100.5, 99.5, 100.0, 1_000.0))
        .collect();
    short.push(bar(60, 100.0, 100.5, 89.5, 90.0, 5_000.0));
    assert_eq!(
        fire_for(
            SetupId::VolumeBreakout,
            &instrument("X", "X.US", AssetClass::Stock, false, short)
        ),
        vec![(60, Side::Short)]
    );
}

#[test]
fn a_breakout_without_two_times_volume_does_not_fire() {
    let mut bars: Vec<Bar> = (0..60)
        .map(|index| bar(index, 100.0, 100.5, 99.5, 100.0, 1_000.0))
        .collect();
    bars.push(bar(60, 100.0, 111.0, 99.5, 110.0, 1_900.0));
    let instrument = instrument("X", "X.US", AssetClass::Stock, false, bars);
    assert!(fire_for(SetupId::VolumeBreakout, &instrument).is_empty());
}

#[test]
fn appending_future_bars_changes_no_earlier_event_firing() {
    let mut bars = gap_bars(80, 5_000.0);
    let events = vec![positive_event(80)];
    let shorter = with_events(bars.clone(), events.clone());
    let before = fire_for(SetupId::EarningsGapDrift, &shorter);
    for index in 81..101 {
        bars.push(bar(index, 100.0, 101.0, 99.0, 100.0, 1_000.0));
    }
    let longer = with_events(bars, events);
    let after = fire_for(SetupId::EarningsGapDrift, &longer);
    assert_eq!(before, after);
}

#[test]
fn two_events_on_one_reaction_session_prefer_the_known_surprise() {
    let mut unknown = positive_event(80);
    unknown.surprise_pct = None;
    let instrument = with_events(gap_bars(80, 5_000.0), vec![unknown, positive_event(80)]);
    assert_eq!(
        fire_for(SetupId::EarningsGapDrift, &instrument),
        vec![(80, Side::Long)]
    );
    let mut unknown = positive_event(80);
    unknown.surprise_pct = None;
    let instrument = with_events(gap_bars(80, 5_000.0), vec![positive_event(80), unknown]);
    assert_eq!(
        fire_for(SetupId::EarningsGapDrift, &instrument),
        vec![(80, Side::Long)]
    );
}
