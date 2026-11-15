//! Round 24 K2: the market regime series, its references and its tagging of trades.

mod trade_support;

use std::collections::HashMap;

use sail_capital::trade::{
    AssetClass, Bar, Exit, InstrumentContext, Region, SetupId, Side, Trade,
    regime::{
        EU_REFERENCE, Reference, US_REFERENCE, Vol, for_instrument, reference_for, series, tag,
    },
};
use trade_support::{date_at, instrument};

fn bars(closes: &[f64]) -> Vec<Bar> {
    closes
        .iter()
        .enumerate()
        .map(|(index, close)| Bar {
            session: date_at(index),
            open: *close,
            high: close * 1.001,
            low: close * 0.999,
            close: *close,
            volume: None,
            dividend: None,
        })
        .collect()
}

fn trend_closes(length: usize, daily: f64) -> Vec<f64> {
    let mut close = 100.0;
    let mut out = Vec::with_capacity(length);
    for _ in 0..length {
        out.push(close);
        close *= 1.0 + daily;
    }
    out
}

#[test]
fn the_trend_is_the_side_of_the_200_session_average() {
    let up = series(&bars(&trend_closes(300, 0.001)));
    assert_eq!(
        up.last().copied().flatten().map(|regime| regime.trend),
        Some(sail_capital::trade::regime::Trend::Up)
    );
    let down = series(&bars(&trend_closes(300, -0.001)));
    assert_eq!(
        down.last().copied().flatten().map(|regime| regime.trend),
        Some(sail_capital::trade::regime::Trend::Down)
    );
}

#[test]
fn a_regime_needs_272_sessions_of_history() {
    let short = series(&bars(&trend_closes(271, 0.001)));
    assert!(short.iter().all(Option::is_none));
    let enough = series(&bars(&trend_closes(272, 0.001)));
    assert!(enough[270].is_none());
    assert!(enough[271].is_some());
}

#[test]
fn a_volatility_spike_reads_as_stressed() {
    // Calm alternating returns, then a month of large ones.
    let mut closes = vec![100.0];
    let mut close = 100.0;
    for index in 0..272 {
        let move_ = if index % 2 == 0 { 0.0005 } else { -0.0005 };
        close *= 1.0 + move_;
        closes.push(close);
    }
    let calm_at = closes.len() - 1;
    for index in 0..30 {
        let move_ = if index % 2 == 0 { 0.02 } else { -0.02 };
        close *= 1.0 + move_;
        closes.push(close);
    }
    let regimes = series(&bars(&closes));
    assert_eq!(regimes[calm_at].map(|regime| regime.vol), Some(Vol::Calm));
    assert_eq!(
        regimes.last().copied().flatten().map(|regime| regime.vol),
        Some(Vol::Stressed)
    );
}

#[test]
fn changing_a_future_close_changes_no_earlier_regime() {
    let mut closes = trend_closes(300, 0.0005);
    let baseline = series(&bars(&closes));
    closes[299] *= 1.2;
    let changed = series(&bars(&closes));
    assert_eq!(baseline[..299], changed[..299]);
}

fn reference_bars(length: usize) -> Vec<Bar> {
    bars(&trend_closes(length, 0.0005))
}

fn shifted_sessions(source: &[Bar], days: u64) -> Vec<Bar> {
    source
        .iter()
        .map(|bar| Bar {
            session: bar.session + chrono::Days::new(days),
            ..*bar
        })
        .collect()
}

#[test]
fn a_stock_uses_its_regions_reference_with_a_five_day_staleness_cut() {
    let reference = reference_bars(300);
    let regimes = series(&reference);
    let mut instrument = instrument(
        "X",
        "X.US",
        AssetClass::Stock,
        false,
        shifted_sessions(&reference, 3),
    );
    instrument.context = InstrumentContext {
        region: Region::Us,
        earnings_events: Vec::new(),
        withholding: 0.0,
    };
    let references = HashMap::from([(US_REFERENCE.to_string(), reference.clone())]);
    let aligned = for_instrument(&instrument, &references);
    assert_eq!(aligned[290], regimes[287]);
    assert!(aligned[290].is_some());

    let mut stale = instrument.clone();
    stale.bars.push(Bar {
        session: date_at(306),
        ..*stale.bars.last().expect("a bar")
    });
    let aligned = for_instrument(&stale, &references);
    assert!(aligned.last().copied().flatten().is_none());
}

#[test]
fn region_and_class_pick_the_reference() {
    let mut us = instrument("X", "X.US", AssetClass::Stock, false, reference_bars(300));
    us.context.region = Region::Us;
    assert_eq!(reference_for(&us), Reference::Key(US_REFERENCE));
    let mut eu = instrument("Y", "Y.DE", AssetClass::Stock, false, reference_bars(300));
    eu.context.region = Region::Eu;
    assert_eq!(reference_for(&eu), Reference::Key(EU_REFERENCE));
    let other = instrument("Z", "Z.ST", AssetClass::Stock, false, reference_bars(300));
    assert_eq!(reference_for(&other), Reference::Unconditioned);
    let index = instrument("IDX", "IDX", AssetClass::Index, false, reference_bars(300));
    assert_eq!(reference_for(&index), Reference::Own);
    let fx = instrument("EURUSD", "EURUSD", AssetClass::Fx, true, Vec::new());
    assert_eq!(reference_for(&fx), Reference::Unconditioned);

    let references = HashMap::from([
        (US_REFERENCE.to_string(), reference_bars(300)),
        (EU_REFERENCE.to_string(), reference_bars(300)),
    ]);
    assert!(for_instrument(&us, &references)[299].is_some());
    assert!(for_instrument(&eu, &references)[299].is_some());
    assert!(
        for_instrument(&other, &references)
            .iter()
            .all(Option::is_none)
    );
    assert!(for_instrument(&fx, &references).iter().all(Option::is_none));
}

fn trade(session: chrono::NaiveDate) -> Trade {
    Trade {
        key: "X".into(),
        setup: SetupId::ThreeDayReversal,
        side: Side::Long,
        signal_session: session,
        entry_session: session,
        entry: 100.0,
        stop: 95.0,
        exit_session: session,
        exit: Exit::Target1,
        r_multiple: 1.0,
        dividend_r: 0.0,
        ex_date_gap: false,
        regime: None,
    }
}

#[test]
fn tag_reads_the_regime_on_each_signal_session() {
    let reference = reference_bars(300);
    let regimes = series(&reference);
    let mut instrument = instrument("X", "X.US", AssetClass::Stock, false, reference.clone());
    instrument.context = InstrumentContext {
        region: Region::Us,
        earnings_events: Vec::new(),
        withholding: 0.0,
    };
    let references = HashMap::from([(US_REFERENCE.to_string(), reference.clone())]);
    let mut trades = vec![trade(instrument.bars[280].session), trade(date_at(400))];
    tag(&mut trades, &instrument, &references);
    assert_eq!(trades[0].regime, regimes[280]);
    assert!(trades[0].regime.is_some());
    assert!(trades[1].regime.is_none());
}

#[test]
fn the_regime_functions_are_pure() {
    let reference = reference_bars(300);
    assert_eq!(series(&reference), series(&reference));
    let mut instrument = instrument("X", "X.US", AssetClass::Stock, false, reference.clone());
    instrument.context = InstrumentContext {
        region: Region::Us,
        earnings_events: Vec::new(),
        withholding: 0.0,
    };
    let references = HashMap::from([(US_REFERENCE.to_string(), reference)]);
    assert_eq!(
        for_instrument(&instrument, &references),
        for_instrument(&instrument, &references)
    );
    let mut first = vec![trade(instrument.bars[280].session)];
    let mut second = first.clone();
    tag(&mut first, &instrument, &references);
    tag(&mut second, &instrument, &references);
    assert_eq!(first, second);
}
