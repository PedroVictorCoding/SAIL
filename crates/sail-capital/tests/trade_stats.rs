//! Evidence intervals, the publish verdict and its holdout split.

mod trade_support;

use chrono::{Days, NaiveDate};
use sail_capital::trade::{Exit, SetupId, Side, Trade, evidence, verdict};

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("a date")
}

fn trade(exit_session: NaiveDate, r_multiple: f64, exit: Exit) -> Trade {
    Trade {
        key: "test".into(),
        setup: SetupId::ThreeDayReversal,
        side: Side::Long,
        signal_session: exit_session - Days::new(3),
        entry_session: exit_session - Days::new(2),
        entry: 100.0,
        stop: 95.0,
        exit_session,
        exit,
        r_multiple,
    }
}

fn selection(count: usize, r_multiple: f64) -> Vec<Trade> {
    (0..count)
        .map(|index| {
            trade(
                date(2019, 1, 1) + Days::new(index as u64),
                r_multiple,
                Exit::Target1,
            )
        })
        .collect()
}

fn holdout(count: usize, r_multiple: f64) -> Vec<Trade> {
    (0..count)
        .map(|index| {
            trade(
                date(2025, 1, 1) + Days::new(index as u64),
                r_multiple,
                Exit::Target1,
            )
        })
        .collect()
}

#[test]
fn evidence_reports_hits_factor_and_drawdown() {
    let trades = vec![
        trade(date(2025, 1, 6), 1.0, Exit::Target1),
        trade(date(2025, 1, 7), 2.0, Exit::Target2),
        trade(date(2025, 1, 8), -1.0, Exit::Stop),
        trade(date(2025, 1, 9), -3.0, Exit::Stop),
    ];
    let evidence = evidence(SetupId::ThreeDayReversal, Side::Long, &trades);
    assert_eq!(evidence.trades, 4);
    assert!((evidence.hit_target1 - 0.5).abs() < 1e-12);
    assert!(evidence.hit_target1_ci.0 > 0.0 && evidence.hit_target1_ci.1 < 1.0);
    assert!((evidence.expectancy_r + 0.25).abs() < 1e-12);
    assert!((evidence.profit_factor - 0.75).abs() < 1e-12);
    assert!((evidence.max_drawdown_r - 4.0).abs() < 1e-12);
}

#[test]
fn wilson_interval_is_centred_on_the_hit_rate() {
    let trades = selection(100, 0.2);
    let evidence = evidence(SetupId::ThreeDayReversal, Side::Long, &trades);
    assert!((evidence.hit_target1 - 1.0).abs() < 1e-12);
    assert!(evidence.hit_target1_ci.0 > 0.9);
    assert!((evidence.hit_target1_ci.1 - 1.0).abs() < 1e-12);
}

#[test]
fn verdict_publishes_with_a_positive_selection_and_holdout() {
    let mut trades = selection(120, 0.2);
    trades.extend(holdout(25, 0.1));
    let verdict = verdict(
        "stocks",
        SetupId::ThreeDayReversal,
        Side::Long,
        &trades,
        date(2026, 1, 1),
    );
    assert!(verdict.publishes, "{}", verdict.reason);
    assert_eq!(verdict.selection.trades, 120);
    assert_eq!(verdict.holdout.trades, 25);
    assert!(verdict.reason.starts_with("Publishes:"));
}

#[test]
fn verdict_refuses_a_non_positive_lower_bound() {
    let mut trades = selection(60, 0.3);
    trades.extend(selection(60, -0.3).into_iter().map(|mut trade| {
        trade.exit_session = trade.exit_session + Days::new(1000);
        trade
    }));
    trades.extend(holdout(25, 0.1));
    let verdict = verdict(
        "stocks",
        SetupId::ThreeDayReversal,
        Side::Long,
        &trades,
        date(2026, 1, 1),
    );
    assert!(!verdict.publishes);
    assert!(verdict.reason.contains("lower bound"), "{}", verdict.reason);
}

#[test]
fn verdict_refuses_a_weak_holdout() {
    let mut trades = selection(120, 0.2);
    trades.extend(holdout(25, -0.1));
    let verdict = verdict(
        "stocks",
        SetupId::ThreeDayReversal,
        Side::Long,
        &trades,
        date(2026, 1, 1),
    );
    assert!(!verdict.publishes);
    assert!(verdict.reason.contains("holdout"), "{}", verdict.reason);
}

#[test]
fn verdict_refuses_too_few_trades() {
    let mut trades = selection(40, 0.5);
    trades.extend(holdout(25, 0.5));
    let verdict = verdict(
        "stocks",
        SetupId::ThreeDayReversal,
        Side::Long,
        &trades,
        date(2026, 1, 1),
    );
    assert!(!verdict.publishes);
    assert!(verdict.reason.contains("fewer"), "{}", verdict.reason);
}

#[test]
fn verdict_ignores_trades_that_exit_after_the_evaluation_date() {
    let mut trades = selection(120, 0.2);
    trades.extend(holdout(25, 0.1));
    let early = verdict(
        "stocks",
        SetupId::ThreeDayReversal,
        Side::Long,
        &trades,
        date(2024, 6, 1),
    );
    assert_eq!(early.selection.trades, 120);
    assert_eq!(early.holdout.trades, 0);
    assert!(!early.publishes);
}
