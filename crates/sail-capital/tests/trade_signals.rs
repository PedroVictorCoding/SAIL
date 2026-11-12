//! Day cards, ranking, sizing, the scoreboard, guards and the earnings window.

mod trade_support;

use std::time::Instant;

use chrono::Days;
use sail_capital::trade::{AssetClass, Instrument, SetupId, Side, guard, signal_day};
use trade_support::{
    bars_from_closes, bars_with_prior_open, date_at, instrument, planted_closes, random_walk,
};

fn planted_universe(count: usize, length: usize) -> Vec<Instrument> {
    (0..count)
        .map(|index| {
            let closes = planted_closes(length, 100 + index as u64);
            instrument(
                &format!("key{index}"),
                &format!("SYM{index}"),
                AssetClass::Stock,
                false,
                bars_with_prior_open(&closes, 0.5),
            )
        })
        .collect()
}

fn day(universe: &[Instrument]) -> sail_capital::trade::SignalDay {
    signal_day(universe, Some(100_000.0), 0.5, &|_| Some(1.0), &|_| None)
}

#[test]
fn a_random_walk_publishes_nothing() {
    let universe: Vec<Instrument> = (0..20)
        .map(|index| {
            let (closes, _) = random_walk(2000, 0.0, 0.01, 1000 + index as u64);
            instrument(
                &format!("key{index}"),
                &format!("SYM{index}"),
                AssetClass::Stock,
                false,
                bars_with_prior_open(&closes, 0.5),
            )
        })
        .collect();
    let day = day(&universe);
    for verdict in &day.scoreboard {
        assert!(
            !verdict.publishes,
            "{} {:?}",
            verdict.reason, verdict.selection.setup
        );
    }
    assert!(day.cards.is_empty());
}

#[test]
fn a_flat_market_has_no_cards_and_keeps_the_scoreboard() {
    let closes = vec![100.0; 300];
    let market = instrument(
        "flat",
        "FLAT",
        AssetClass::Stock,
        true,
        bars_from_closes(&closes, 0.5),
    );
    let day = day(&[market]);
    assert!(day.cards.is_empty());
    assert_eq!(day.scoreboard.len(), 36);
    assert_eq!(day.suppressed.len(), 0);
    assert!(!day.notes.is_empty());
}

#[test]
fn the_planted_edge_publishes_and_ranks_first() {
    let universe = planted_universe(20, 1999);
    let day = day(&universe);
    let verdict = day
        .scoreboard
        .iter()
        .find(|verdict| {
            verdict.group == "stocks"
                && verdict.selection.setup == SetupId::ThreeDayReversal
                && verdict.selection.side == Side::Long
        })
        .expect("a verdict");
    assert!(verdict.publishes, "{}", verdict.reason);
    assert!(!day.cards.is_empty());
    assert_eq!(day.cards[0].setup, SetupId::ThreeDayReversal);
    assert_eq!(day.cards[0].side, Side::Long);
}

#[test]
fn cards_size_from_the_risk_and_the_account_value() {
    let universe = planted_universe(20, 1999);
    let day = day(&universe);
    let card = day.cards.first().expect("a card");
    let middle = (card.entry_zone.0 + card.entry_zone.1) / 2.0;
    let risk = (middle - card.stop).abs();
    let (units, value) = card.size.expect("a size");
    assert!((units - 500.0 / risk).abs() < 1e-9, "{units}");
    assert!((value - units * middle).abs() < 1e-6, "{value}");
    assert_eq!(card.risk_pct, 0.5);
}

#[test]
fn earnings_inside_the_window_suppress_the_card() {
    let mut universe = planted_universe(20, 1999);
    let first = universe.first_mut().expect("an instrument");
    let key = first.key.clone();
    let symbol = first.symbol.clone();
    let session = first.bars.last().expect("a bar").session;
    first.earnings.push(session + Days::new(1));
    let day = day(&universe);
    assert!(
        day.cards.iter().all(|card| card.key != key),
        "the earnings card was still published"
    );
    assert!(day.suppressed.iter().any(|(seen, _, _)| seen == &symbol));
}

#[test]
fn guard_reports_the_chandelier_stop_and_the_next_earnings_date() {
    let closes: Vec<f64> = (0..250).map(|index| 100.0 + 0.2 * index as f64).collect();
    let mut market = instrument(
        "held",
        "HELD",
        AssetClass::Stock,
        true,
        bars_from_closes(&closes, 0.4),
    );
    let session = market.bars.last().expect("a bar").session;
    market.earnings = vec![session - Days::new(10), session + Days::new(20)];
    let guard = guard(&market).expect("a guard");
    assert!(guard.close > guard.trailing_stop);
    assert!(!guard.stop_hit);
    assert_eq!(guard.above_200d, Some(true));
    assert_eq!(guard.next_earnings, Some(session + Days::new(20)));
    assert!(guard.stop_distance_pct > 0.0);
}

#[test]
fn guard_marks_a_broken_trend() {
    let mut closes: Vec<f64> = (0..228).map(|index| 100.0 + 0.2 * index as f64).collect();
    let last = *closes.last().expect("a close");
    for step in 1..=22 {
        closes.push(last - step as f64);
    }
    let market = instrument(
        "held",
        "HELD",
        AssetClass::Stock,
        true,
        bars_from_closes(&closes, 0.4),
    );
    let guard = guard(&market).expect("a guard");
    assert!(guard.stop_hit, "{} {}", guard.close, guard.trailing_stop);
}

#[test]
fn the_largest_universe_finishes_quickly() {
    let universe = planted_universe(80, 2000);
    let start = Instant::now();
    let day = day(&universe);
    let elapsed = start.elapsed();
    assert!(!day.scoreboard.is_empty());
    assert!(elapsed.as_secs() < 60, "{elapsed:?}");
}

#[test]
fn card_ids_are_stable() {
    let universe = planted_universe(20, 1999);
    let first = day(&universe);
    let second = day(&universe);
    let ids: Vec<&str> = first.cards.iter().map(|card| card.id.as_str()).collect();
    let again: Vec<&str> = second.cards.iter().map(|card| card.id.as_str()).collect();
    assert_eq!(ids, again);
    assert!(first.cards.iter().all(|card| !card.id.is_empty()));
}

#[test]
fn holding_relations_name_the_position() {
    let universe = planted_universe(20, 1999);
    let day = signal_day(&universe, Some(100_000.0), 0.5, &|_| Some(1.0), &|_| {
        Some("12 shares".to_string())
    });
    let card = day.cards.first().expect("a card");
    let relation = card.relation.as_deref().expect("a relation");
    assert!(relation.contains("12 shares"), "{relation}");
}

#[test]
fn dates_are_the_latest_sessions() {
    let universe = planted_universe(2, 300);
    let day = day(&universe);
    assert_eq!(day.session, date_at(299));
}
