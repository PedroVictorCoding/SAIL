//! Simulator paths, costs, grading and no look-ahead.

mod trade_support;

use sail_capital::trade::{AssetClass, Bar, Costs, Exit, SetupId, Side, grade, replay, verdict};
use trade_support::{bars_with_prior_open, card, date_at, instrument, planted_closes, zero_costs};

fn ohlc(index: usize, open: f64, high: f64, low: f64, close: f64) -> Bar {
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

fn long_card(
    stop: f64,
    targets: [f64; 3],
    max_sessions: u32,
    needs_cfd: bool,
) -> sail_capital::trade::SignalCard {
    card(
        Side::Long,
        SetupId::ThreeDayReversal,
        100.0,
        stop,
        targets,
        max_sessions,
        needs_cfd,
    )
}

#[test]
fn straight_to_target_three_returns_two_r() {
    let bars = vec![ohlc(0, 100.0, 116.0, 99.0, 115.0)];
    let card = long_card(95.0, [105.0, 110.0, 115.0], 5, false);
    let trade = grade(&card, &bars, zero_costs()).expect("a closed trade");
    assert_eq!(trade.exit, Exit::Target3);
    assert!(
        (trade.r_multiple - 2.0).abs() < 1e-12,
        "{}",
        trade.r_multiple
    );
}

#[test]
fn target_one_then_breakeven_returns_one_third_r() {
    let bars = vec![
        ohlc(0, 100.0, 106.0, 99.0, 105.0),
        ohlc(1, 100.0, 101.0, 94.0, 96.0),
    ];
    let card = long_card(95.0, [105.0, 110.0, 115.0], 5, false);
    let trade = grade(&card, &bars, zero_costs()).expect("a closed trade");
    assert_eq!(trade.exit, Exit::Target1);
    assert!(
        (trade.r_multiple - 1.0 / 3.0).abs() < 1e-12,
        "{}",
        trade.r_multiple
    );
}

#[test]
fn a_stop_first_returns_minus_one_r() {
    let bars = vec![ohlc(0, 100.0, 101.0, 94.0, 95.0)];
    let card = long_card(95.0, [105.0, 110.0, 115.0], 5, false);
    let trade = grade(&card, &bars, zero_costs()).expect("a closed trade");
    assert_eq!(trade.exit, Exit::Stop);
    assert!(
        (trade.r_multiple + 1.0).abs() < 1e-12,
        "{}",
        trade.r_multiple
    );
}

#[test]
fn a_bar_with_stop_and_target_counts_as_the_stop() {
    let bars = vec![ohlc(0, 100.0, 116.0, 94.0, 100.0)];
    let card = long_card(95.0, [105.0, 110.0, 115.0], 5, false);
    let trade = grade(&card, &bars, zero_costs()).expect("a closed trade");
    assert_eq!(trade.exit, Exit::Stop);
    assert!(
        (trade.r_multiple + 1.0).abs() < 1e-12,
        "{}",
        trade.r_multiple
    );
}

#[test]
fn a_gap_through_the_stop_fills_at_the_open() {
    let bars = vec![
        ohlc(0, 100.0, 100.5, 99.5, 100.0),
        ohlc(1, 90.0, 91.0, 89.0, 90.0),
    ];
    let card = long_card(95.0, [105.0, 110.0, 115.0], 5, false);
    let trade = grade(&card, &bars, zero_costs()).expect("a closed trade");
    assert_eq!(trade.exit, Exit::Stop);
    assert!(
        (trade.r_multiple + 2.0).abs() < 1e-12,
        "{}",
        trade.r_multiple
    );
}

#[test]
fn the_time_stop_closes_at_the_last_session() {
    let bars = vec![
        ohlc(0, 100.0, 100.5, 99.5, 101.0),
        ohlc(1, 101.0, 102.5, 100.5, 102.0),
        ohlc(2, 102.0, 103.5, 101.5, 103.0),
    ];
    let card = long_card(95.0, [200.0, 210.0, 220.0], 3, false);
    let trade = grade(&card, &bars, zero_costs()).expect("a closed trade");
    assert_eq!(trade.exit, Exit::Time);
    assert_eq!(trade.exit_session, date_at(2));
    assert!(
        (trade.r_multiple - 0.6).abs() < 1e-12,
        "{}",
        trade.r_multiple
    );
}

#[test]
fn an_open_outside_the_zone_fills_nothing() {
    let bars = vec![ohlc(0, 101.0, 102.0, 99.0, 100.0)];
    let card = long_card(95.0, [105.0, 110.0, 115.0], 5, false);
    assert!(grade(&card, &bars, zero_costs()).is_none());
}

#[test]
fn a_trade_still_open_has_no_grade() {
    let bars = vec![ohlc(0, 100.0, 101.0, 99.0, 100.0)];
    let card = long_card(80.0, [200.0, 210.0, 220.0], 10, false);
    assert!(grade(&card, &bars, zero_costs()).is_none());
}

#[test]
fn cfd_financing_grows_with_the_days_held() {
    let bars: Vec<Bar> = (0..5)
        .map(|index| {
            ohlc(
                index,
                if index == 0 { 100.0 } else { 103.0 },
                103.5,
                102.5,
                103.0,
            )
        })
        .collect();
    let costs = Costs {
        round_trip: 0.0,
        fx_conversion: 0.0,
        cfd_daily_financing: 0.001,
    };
    let short = grade(
        &long_card(95.0, [200.0, 210.0, 220.0], 3, true),
        &bars,
        costs,
    )
    .expect("a short trade");
    let long = grade(
        &long_card(95.0, [200.0, 210.0, 220.0], 5, true),
        &bars,
        costs,
    )
    .expect("a long trade");
    assert!(
        (short.r_multiple - 0.56).abs() < 1e-12,
        "{}",
        short.r_multiple
    );
    assert!(
        (long.r_multiple - 0.52).abs() < 1e-12,
        "{}",
        long.r_multiple
    );
}

#[test]
fn fx_conversion_applies_to_a_non_cfd_trade_only() {
    let bars: Vec<Bar> = (0..3)
        .map(|index| {
            ohlc(
                index,
                if index == 0 { 100.0 } else { 103.0 },
                103.5,
                102.5,
                103.0,
            )
        })
        .collect();
    let costs = Costs {
        round_trip: 0.0,
        fx_conversion: 0.01,
        cfd_daily_financing: 0.0,
    };
    let cash = grade(
        &long_card(95.0, [200.0, 210.0, 220.0], 3, false),
        &bars,
        costs,
    )
    .expect("a cash trade");
    let cfd = grade(
        &long_card(95.0, [200.0, 210.0, 220.0], 3, true),
        &bars,
        costs,
    )
    .expect("a CFD trade");
    assert!((cash.r_multiple - 0.4).abs() < 1e-12, "{}", cash.r_multiple);
    assert!((cfd.r_multiple - 0.6).abs() < 1e-12, "{}", cfd.r_multiple);
}

#[test]
fn replay_applies_fx_only_to_a_foreign_non_cfd_trade() {
    let closes = planted_closes(2000, 7);
    let bars = bars_with_prior_open(&closes, 0.5);
    let costs = Costs {
        round_trip: 0.0,
        fx_conversion: 0.01,
        cfd_daily_financing: 0.0,
    };
    let domestic = instrument("domestic", "DOM", AssetClass::Stock, false, bars.clone());
    let foreign = instrument("foreign", "FOR", AssetClass::Stock, true, bars);
    let domestic_trades = replay(&domestic, costs);
    let foreign_trades = replay(&foreign, costs);
    let domestic_trade = domestic_trades
        .iter()
        .find(|trade| trade.setup == SetupId::ThreeDayReversal && trade.side == Side::Long)
        .expect("a domestic trade");
    let foreign_trade = foreign_trades
        .iter()
        .find(|trade| {
            trade.setup == SetupId::ThreeDayReversal
                && trade.side == Side::Long
                && trade.signal_session == domestic_trade.signal_session
        })
        .expect("a foreign trade");
    let risk = (domestic_trade.entry - domestic_trade.stop).abs();
    let expected = costs.fx_conversion * domestic_trade.entry / risk;
    assert!(
        (domestic_trade.r_multiple - foreign_trade.r_multiple - expected).abs() < 1e-12,
        "domestic {} foreign {}",
        domestic_trade.r_multiple,
        foreign_trade.r_multiple
    );
}

#[test]
fn grade_matches_replay_for_the_same_trade() {
    let closes = planted_closes(2000, 11);
    let bars = bars_with_prior_open(&closes, 0.5);
    let costs = Costs::default_for(AssetClass::Stock);
    let market = instrument("test", "TST", AssetClass::Stock, true, bars.clone());
    let trade = replay(&market, costs)
        .into_iter()
        .find(|trade| trade.setup == SetupId::ThreeDayReversal && trade.side == Side::Long)
        .expect("a trade");
    let entry_index = market
        .bars
        .iter()
        .position(|bar| bar.session == trade.entry_session)
        .expect("an entry bar");
    let risk = (trade.entry - trade.stop).abs();
    let targets = [
        trade.entry + risk,
        trade.entry + 2.0 * risk,
        trade.entry + 3.0 * risk,
    ];
    let card = card(
        trade.side,
        trade.setup,
        trade.entry,
        trade.stop,
        targets,
        5,
        false,
    );
    let graded = grade(&card, &market.bars[entry_index..], costs).expect("a graded trade");
    assert_eq!(graded.entry, trade.entry);
    assert_eq!(graded.stop, trade.stop);
    assert_eq!(graded.entry_session, trade.entry_session);
    assert_eq!(graded.exit_session, trade.exit_session);
    assert_eq!(graded.exit, trade.exit);
    assert!((graded.r_multiple - trade.r_multiple).abs() < 1e-12);
}

#[test]
fn appending_future_bars_changes_no_earlier_trade_or_verdict() {
    let closes = planted_closes(1200, 19);
    let short_bars = bars_with_prior_open(&closes[..1000], 0.5);
    let long_bars = bars_with_prior_open(&closes, 0.5);
    let market = instrument("test", "TST", AssetClass::Stock, true, short_bars);
    let costs = Costs::default_for(AssetClass::Stock);
    let earlier = replay(&market, costs);
    let as_of = market.bars.last().expect("a bar").session;
    let earlier_verdict = verdict(
        "stocks",
        SetupId::ThreeDayReversal,
        Side::Long,
        &earlier,
        as_of,
    );
    let longer = replay(
        &instrument("test", "TST", AssetClass::Stock, true, long_bars),
        costs,
    );
    let count = earlier
        .iter()
        .filter(|trade| trade.exit_session <= as_of)
        .count();
    let matching = longer
        .iter()
        .filter(|trade| trade.exit_session <= as_of)
        .count();
    assert_eq!(count, matching);
    let later_verdict = verdict(
        "stocks",
        SetupId::ThreeDayReversal,
        Side::Long,
        &longer,
        as_of,
    );
    assert_eq!(earlier_verdict, later_verdict);
}
