//! Round 19 selection: false discovery, pooling, the live update, walk-forward and verdicts.

mod trade_support;

use std::collections::BTreeMap;

use chrono::{Days, Months, NaiveDate};
use sail_capital::trade::select::{
    LiveTrade, MIN_CANDIDATE_OOS_TRADES, benjamini_hochberg, judge, p_value, pool, walk_forward,
};
use sail_capital::trade::{AssetClass, Costs, Evidence, Exit, SetupId, Side, Tier, Trade, replay};
use trade_support::{bars_with_prior_open, instrument, planted_closes, random_walk};

const Z: f64 = 1.959_964;

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("a date")
}

fn evidence(trades: u32, expectancy_r: f64, se: f64) -> Evidence {
    Evidence {
        setup: SetupId::TrendPullback,
        side: Side::Long,
        trades,
        hit_target1: 0.0,
        hit_target1_ci: (0.0, 0.0),
        expectancy_r,
        expectancy_ci: (expectancy_r - Z * se, expectancy_r + Z * se),
        profit_factor: 0.0,
        max_drawdown_r: 0.0,
        mean_sessions: 0.0,
    }
}

fn trade(
    setup: SetupId,
    side: Side,
    signal_session: NaiveDate,
    exit_session: NaiveDate,
    r_multiple: f64,
) -> Trade {
    Trade {
        key: "test".into(),
        setup,
        side,
        signal_session,
        entry_session: signal_session + Days::new(1),
        entry: 100.0,
        stop: 95.0,
        exit_session,
        exit: Exit::Target1,
        r_multiple,
    }
}

fn group(trades: Vec<Trade>) -> BTreeMap<String, Vec<Trade>> {
    BTreeMap::from([("stocks".to_string(), trades)])
}

fn group_trades(group: &str, trades: Vec<Trade>) -> BTreeMap<String, Vec<Trade>> {
    BTreeMap::from([(group.to_string(), trades)])
}

fn alternating(
    setup: SetupId,
    side: Side,
    count: usize,
    high: f64,
    start: NaiveDate,
) -> Vec<Trade> {
    (0..count)
        .map(|index| {
            let day = start + Days::new(index as u64);
            trade(
                setup,
                side,
                day,
                day,
                if index % 2 == 0 { high } else { 0.0 },
            )
        })
        .collect()
}

fn cell(
    verdicts: &[sail_capital::trade::Verdict],
    setup: SetupId,
    side: Side,
) -> &sail_capital::trade::Verdict {
    verdicts
        .iter()
        .find(|verdict| {
            verdict.group == "stocks"
                && verdict.selection.setup == setup
                && verdict.selection.side == side
        })
        .expect("a verdict")
}

// BH worked example. m = 5, q = 0.10, thresholds k/5 * 0.10 =
// 0.02, 0.04, 0.06, 0.08, 0.10. Sorted p = 0.005, 0.02, 0.03, 0.2, 0.9.
// The first three pass, so the step-up rejects p(1)..p(3).
#[test]
fn benjamini_hochberg_matches_the_worked_example() {
    let rejected = benjamini_hochberg(&[0.005, 0.02, 0.03, 0.2, 0.9], 0.10);
    assert_eq!(rejected, vec![true, true, true, false, false]);
}

#[test]
fn benjamini_hochberg_never_rejects_nan() {
    let rejected = benjamini_hochberg(&[f64::NAN, 0.001, f64::NAN], 0.10);
    assert_eq!(rejected, vec![false, true, false]);
}

// m = 1, q = 0.10: 0.05 <= 0.10 rejects; 0.20 > 0.10 does not.
#[test]
fn benjamini_hochberg_handles_one_test() {
    assert_eq!(benjamini_hochberg(&[0.05], 0.10), vec![true]);
    assert_eq!(benjamini_hochberg(&[0.20], 0.10), vec![false]);
    assert!(benjamini_hochberg(&[], 0.10).is_empty());
}

#[test]
fn benjamini_hochberg_keeps_ties_in_input_order() {
    // m = 3, q = 0.10, thresholds 0.0333, 0.0667, 0.10. The two 0.02 ties both pass.
    let rejected = benjamini_hochberg(&[0.02, 0.20, 0.02], 0.10);
    assert_eq!(rejected, vec![true, false, true]);
}

#[test]
fn benjamini_hochberg_does_not_depend_on_input_order() {
    let p = [0.004, 0.021, 0.019, 0.20, 0.03, 0.5, 0.0008, 0.07];
    let expected = benjamini_hochberg(&p, 0.10);
    let order = [5usize, 2, 7, 0, 4, 1, 6, 3];
    let permuted: Vec<f64> = order.iter().map(|index| p[*index]).collect();
    let seen = benjamini_hochberg(&permuted, 0.10);
    let mut restored = vec![false; p.len()];
    for (position, original) in order.iter().enumerate() {
        restored[*original] = seen[position];
    }
    assert_eq!(restored, expected);
}

// DL with y = 0, 1, 2, 3 and v = 1 each:
// ybar = 1.5, Q = 2.25 + 0.25 + 0.25 + 2.25 = 5,
// tau^2 = (5 - 3) / (4 - 4/4) = 2/3,
// prior = 1.5, B = 1 / (1 + 2/3) = 0.6,
// posterior = 0.6 * 1.5 + 0.4 * y = 0.9, 1.3, 1.7, 2.1,
// posterior sd = sqrt(0.4).
#[test]
fn pool_matches_hand_computed_der_simonian_laird_tau2() {
    let cells: Vec<Evidence> = [0.0, 1.0, 2.0, 3.0]
        .into_iter()
        .map(|mean| evidence(100, mean, 1.0))
        .collect();
    let pooled = pool(&cells);
    for (index, cell) in pooled.iter().enumerate() {
        assert!((cell.prior_mean - 1.5).abs() < 1e-12);
        assert!((cell.prior_sd - (2.0f64 / 3.0).sqrt()).abs() < 1e-12);
        assert!((cell.shrinkage - 0.6).abs() < 1e-12);
        let expected = 0.6 * 1.5 + 0.4 * index as f64;
        assert!((cell.posterior_mean - expected).abs() < 1e-12);
        assert!((cell.posterior_sd - 0.4f64.sqrt()).abs() < 1e-12);
    }
}

// y = 2, 2, 2 with v = 1: Q = 0, tau^2 = 0, B = 1, every cell is the prior.
#[test]
fn pool_with_zero_tau_keeps_every_cell_at_the_prior() {
    let cells: Vec<Evidence> = [2.0, 2.0, 2.0]
        .into_iter()
        .map(|mean| evidence(100, mean, 1.0))
        .collect();
    for cell in pool(&cells) {
        assert!((cell.prior_mean - 2.0).abs() < 1e-12);
        assert_eq!(cell.prior_sd, 0.0);
        assert_eq!(cell.shrinkage, 1.0);
        assert!((cell.posterior_mean - 2.0).abs() < 1e-12);
        assert_eq!(cell.posterior_sd, 0.0);
    }
}

#[test]
fn pool_leaves_a_thin_cell_at_the_prior() {
    let cells = vec![
        evidence(100, 2.0, 1.0),
        evidence(100, 2.0, 1.0),
        evidence(9, 100.0, 1.0),
    ];
    let pooled = pool(&cells);
    assert!((pooled[2].posterior_mean - 2.0).abs() < 1e-12);
    assert_eq!(pooled[2].shrinkage, 1.0);
    assert_eq!(pooled[2].posterior_sd, 0.0);
}

#[test]
fn pool_shrinkage_is_between_zero_and_one() {
    let cells: Vec<Evidence> = [0.0, 1.0, 2.0, 3.0]
        .into_iter()
        .map(|mean| evidence(100, mean, 0.5))
        .collect();
    for cell in pool(&cells) {
        assert!((0.0..=1.0).contains(&cell.shrinkage));
    }
}

// Posterior, before live evidence, from the same two cells as the live test:
// y1 = 1, v1 = 1/9; y2 = 2, v2 = 4/9.
// ybar = 1.2, Q = 1.8, tau^2 = 0.8 / 3.6 = 2/9,
// prior = (3*1 + 1.5*2) / 4.5 = 4/3,
// B1 = 1/3, posterior = 4/9 + 2/3 = 10/9, sd = sqrt(2/27).
// Two live trades at 3R and 5R (s^2 = 10/9):
// precision = 1/(2/27) + 2/(10/9) = 13.5 + 1.8 = 15.3,
// mean = (10/9 * 13.5 + 8 * 0.9) / 15.3 = 22.2 / 15.3, sd = sqrt(1/15.3).
#[test]
fn judge_updates_the_pooled_posterior_with_live_trades() {
    let mut trades = alternating(
        SetupId::TrendPullback,
        Side::Long,
        10,
        2.0,
        date(2020, 1, 1),
    );
    trades.extend(alternating(
        SetupId::TrendPullback,
        Side::Short,
        10,
        4.0,
        date(2020, 1, 1),
    ));
    let trades_by_group = group(trades);
    let as_of = date(2026, 1, 1);
    let without = judge(&trades_by_group, &[], as_of);
    let long = cell(&without, SetupId::TrendPullback, Side::Long);
    let judgement = long.judgement.as_ref().expect("a judgement");
    assert!((judgement.posterior_mean - 10.0 / 9.0).abs() < 1e-5);
    assert!((judgement.posterior_sd - (2.0f64 / 27.0).sqrt()).abs() < 1e-5);
    assert!((judgement.posterior_mean - judgement.pooled.posterior_mean).abs() < 1e-12);
    assert!((judgement.posterior_sd - judgement.pooled.posterior_sd).abs() < 1e-12);

    let live = |exit_session: NaiveDate| LiveTrade {
        group: "stocks".into(),
        setup: SetupId::TrendPullback,
        side: Side::Long,
        tier: Tier::Proven,
        exit_session,
        r_multiple: 0.0,
    };
    let live_trades = [
        LiveTrade {
            r_multiple: 3.0,
            ..live(date(2025, 1, 2))
        },
        LiveTrade {
            r_multiple: 5.0,
            ..live(date(2025, 2, 3))
        },
    ];
    let with = judge(&trades_by_group, &live_trades, as_of);
    let judgement = cell(&with, SetupId::TrendPullback, Side::Long)
        .judgement
        .as_ref()
        .expect("a judgement");
    assert!((judgement.posterior_mean - 22.2 / 15.3).abs() < 1e-5);
    assert!((judgement.posterior_sd - (1.0f64 / 15.3).sqrt()).abs() < 1e-5);
    assert_eq!(judgement.live.trades, 2);
    assert!((judgement.live.mean_r - 4.0).abs() < 1e-12);

    let after = [LiveTrade {
        r_multiple: 100.0,
        ..live(date(2026, 2, 1))
    }];
    let ignored = judge(&trades_by_group, &after, as_of);
    let judgement = cell(&ignored, SetupId::TrendPullback, Side::Long)
        .judgement
        .as_ref()
        .expect("a judgement");
    assert_eq!(judgement.live.trades, 0);
    assert!((judgement.posterior_mean - 10.0 / 9.0).abs() < 1e-5);
}

fn fit_trades(count: usize, exit: NaiveDate) -> Vec<Trade> {
    (0..count)
        .map(|index| {
            let signal = exit - Days::new(2);
            trade(
                SetupId::ThreeDayReversal,
                Side::Long,
                signal + Days::new(index as u64),
                exit + Days::new(index as u64),
                if index % 2 == 0 { 2.0 } else { 0.0 },
            )
        })
        .collect()
}

// as_of = 2026-01-01 gives folds 2022, 2023, 2024 and 2025.
// Ten fit trades exiting in 2021 make the cell selected in fold 0.
// Fold 0 has two out-of-sample trades (+1R each), mean 1.0.
// Fold 1 has one (+2R), mean 2.0. The three together mean 4/3.
#[test]
fn walk_forward_reports_two_hand_computed_folds() {
    let as_of = date(2026, 1, 1);
    let mut trades = fit_trades(10, date(2021, 3, 1));
    trades.push(trade(
        SetupId::ThreeDayReversal,
        Side::Long,
        date(2022, 2, 1),
        date(2022, 2, 1),
        1.0,
    ));
    trades.push(trade(
        SetupId::ThreeDayReversal,
        Side::Long,
        date(2022, 3, 1),
        date(2022, 3, 1),
        1.0,
    ));
    trades.push(trade(
        SetupId::ThreeDayReversal,
        Side::Long,
        date(2023, 2, 1),
        date(2023, 2, 1),
        2.0,
    ));
    let walk = walk_forward(&group_trades("fx", trades), as_of);
    let cell = walk
        .get(&("fx".into(), SetupId::ThreeDayReversal, Side::Long))
        .expect("a cell");
    assert_eq!(cell.folds.len(), 4);
    assert!(cell.folds[0].selected);
    assert_eq!(cell.folds[0].trades, 2);
    assert!((cell.folds[0].mean_r - 1.0).abs() < 1e-12);
    assert!(cell.folds[1].selected);
    assert_eq!(cell.folds[1].trades, 1);
    assert!((cell.folds[1].mean_r - 2.0).abs() < 1e-12);
    assert_eq!(cell.oos_trades, 3);
    assert!((cell.oos_mean_r - 4.0 / 3.0).abs() < 1e-12);
    assert!((cell.all_mean_r - 4.0 / 3.0).abs() < 1e-12);
    assert!(cell.oos_ci.0 < cell.oos_mean_r && cell.oos_mean_r < cell.oos_ci.1);
}

#[test]
fn walk_forward_purges_a_trade_that_spans_the_fold_start() {
    let as_of = date(2026, 1, 1);
    let spanning: Vec<Trade> = (0..10)
        .map(|index| {
            trade(
                SetupId::ThreeDayReversal,
                Side::Long,
                date(2021, 6, 1) + Days::new(index as u64),
                date(2022, 6, 1) + Days::new(index as u64),
                5.0,
            )
        })
        .collect();
    let walk = walk_forward(&group_trades("fx", spanning), as_of);
    let cell = walk
        .get(&("fx".into(), SetupId::ThreeDayReversal, Side::Long))
        .expect("a cell");
    assert!(!cell.folds[0].selected);
    assert_eq!(cell.folds[0].trades, 0);

    let before: Vec<Trade> = (0..10)
        .map(|index| {
            trade(
                SetupId::ThreeDayReversal,
                Side::Long,
                date(2021, 6, 1) + Days::new(index as u64),
                date(2021, 12, 1) + Days::new(index as u64),
                if index % 2 == 0 { 5.0 } else { 0.0 },
            )
        })
        .collect();
    let walk = walk_forward(&group_trades("fx", before), as_of);
    assert!(
        walk.get(&("fx".into(), SetupId::ThreeDayReversal, Side::Long))
            .expect("a cell")
            .folds[0]
            .selected
    );
}

#[test]
fn walk_forward_embargoes_trades_just_before_the_fold() {
    let as_of = date(2026, 1, 1);
    let inside: Vec<Trade> = (0..10)
        .map(|index| {
            trade(
                SetupId::ThreeDayReversal,
                Side::Long,
                date(2021, 12, 1) + Days::new(index as u64),
                date(2021, 12, 25) + Days::new(index as u64),
                if index % 2 == 0 { 5.0 } else { 0.0 },
            )
        })
        .collect();
    let walk = walk_forward(&group_trades("fx", inside), as_of);
    let cell = walk
        .get(&("fx".into(), SetupId::ThreeDayReversal, Side::Long))
        .expect("a cell");
    assert!(!cell.folds[0].selected);

    let outside: Vec<Trade> = (0..10)
        .map(|index| {
            trade(
                SetupId::ThreeDayReversal,
                Side::Long,
                date(2021, 11, 1) + Days::new(index as u64),
                date(2021, 12, 10) + Days::new(index as u64),
                if index % 2 == 0 { 5.0 } else { 0.0 },
            )
        })
        .collect();
    let walk = walk_forward(&group_trades("fx", outside), as_of);
    assert!(
        walk.get(&("fx".into(), SetupId::ThreeDayReversal, Side::Long))
            .expect("a cell")
            .folds[0]
            .selected
    );
}

#[test]
fn walk_forward_earlier_folds_ignore_future_trades() {
    let as_of = date(2026, 1, 1);
    let mut trades = fit_trades(10, date(2021, 3, 1));
    trades.push(trade(
        SetupId::ThreeDayReversal,
        Side::Long,
        date(2022, 2, 1),
        date(2022, 2, 1),
        1.0,
    ));
    let baseline = walk_forward(&group_trades("fx", trades.clone()), as_of);
    trades.push(trade(
        SetupId::ThreeDayReversal,
        Side::Long,
        date(2026, 2, 1),
        date(2026, 3, 1),
        100.0,
    ));
    let with_future = walk_forward(&group_trades("fx", trades), as_of);
    let key = ("fx".into(), SetupId::ThreeDayReversal, Side::Long);
    let before = baseline.get(&key).expect("a cell");
    let after = with_future.get(&key).expect("a cell");
    assert_eq!(before.folds, after.folds);
    assert_eq!(before.oos_trades, after.oos_trades);
    assert_eq!(before.all_trades, after.all_trades);
}

fn replays(universe: &[sail_capital::trade::Instrument]) -> BTreeMap<String, Vec<Trade>> {
    let mut by_group: BTreeMap<String, Vec<Trade>> = BTreeMap::new();
    for instrument in universe {
        let trades = replay(instrument, Costs::default_for(instrument.class));
        by_group
            .entry(sail_capital::trade::group_of(instrument.class).to_string())
            .or_default()
            .extend(trades);
    }
    by_group
}

fn planted_universe(count: usize, length: usize) -> Vec<sail_capital::trade::Instrument> {
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

#[test]
fn a_planted_edge_verifies() {
    let universe = planted_universe(20, 1999);
    let as_of = universe[0].bars.last().expect("a bar").session;
    let verdicts = judge(&replays(&universe), &[], as_of);
    let planted = verdicts
        .iter()
        .find(|verdict| {
            verdict.group == "stocks"
                && verdict.selection.setup == SetupId::ThreeDayReversal
                && verdict.selection.side == Side::Long
        })
        .expect("a verdict");
    assert!(planted.publishes, "{}", planted.reason);
    assert!(planted.judgement.is_some());
    assert!(planted.reason.starts_with("Publishes:"));
}

#[test]
fn random_walks_verify_nothing() {
    for seed in 0..5 {
        let universe: Vec<sail_capital::trade::Instrument> = (0..20)
            .map(|index| {
                let (closes, _) = random_walk(2000, 0.0, 0.01, 1000 + seed * 100 + index as u64);
                instrument(
                    &format!("key{index}"),
                    &format!("SYM{index}"),
                    AssetClass::Stock,
                    false,
                    bars_with_prior_open(&closes, 0.5),
                )
            })
            .collect();
        let as_of = universe[0].bars.last().expect("a bar").session;
        let verdicts = judge(&replays(&universe), &[], as_of);
        assert!(
            verdicts.iter().all(|verdict| !verdict.publishes),
            "seed {seed} verified a random walk"
        );
    }
}

#[test]
fn the_reason_names_the_first_failed_condition() {
    let verdicts = judge(&BTreeMap::new(), &[], date(2026, 1, 1));
    assert_eq!(verdicts.len(), 36);
    assert!(
        verdicts
            .iter()
            .all(|verdict| verdict.reason.starts_with("Doesn't publish: "))
    );
    assert!(
        verdicts[0]
            .reason
            .contains("false-discovery-rate test did not reject")
    );
    assert!(verdicts.iter().all(|verdict| verdict.judgement.is_some()));
}

#[test]
fn the_candidate_rule_matches_the_spec() {
    // 72 monthly trades alternating +1.2R and -1.0R: mean +0.1R, too noisy for FDR,
    // but positive, and enough out-of-sample trades over all folds.
    let mut trades = Vec::new();
    for month in 0..72 {
        let signal = date(2019, 1, 1) + Months::new(month);
        trades.push(trade(
            SetupId::ThreeDayReversal,
            Side::Long,
            signal,
            signal,
            if month % 2 == 0 { 1.2 } else { -1.0 },
        ));
    }
    let verdicts = judge(&group_trades("stocks", trades), &[], date(2026, 1, 1));
    let verdict = cell(&verdicts, SetupId::ThreeDayReversal, Side::Long);
    assert!(!verdict.publishes);
    assert!(verdict.candidate, "{}", verdict.reason);
    assert!(verdict.reason.starts_with("Candidate: "));
    assert!(verdict.selection.trades >= sail_capital::trade::MIN_CANDIDATE_TRADES);
    let judgement = verdict.judgement.as_ref().expect("a judgement");
    assert!(judgement.posterior_mean > 0.0);
    assert!(judgement.walk_forward.all_trades >= MIN_CANDIDATE_OOS_TRADES);
}

#[test]
fn judge_is_deterministic() {
    let universe = planted_universe(20, 1999);
    let as_of = universe[0].bars.last().expect("a bar").session;
    let replays = replays(&universe);
    let first = judge(&replays, &[], as_of);
    let second = judge(&replays, &[], as_of);
    assert_eq!(first, second);
}

// p = 1 - Phi(2) = 0.0227501 for mean 1 and se 0.5.
#[test]
fn p_value_uses_the_normal_approximation() {
    let evidence = evidence(10, 1.0, 0.5);
    assert!((p_value(&evidence) - 0.022_750_132).abs() < 1e-6);
}

#[test]
fn p_value_is_one_for_degenerate_evidence() {
    assert_eq!(p_value(&evidence(1, 10.0, 0.1)), 1.0);
    assert_eq!(p_value(&evidence(10, 10.0, 0.0)), 1.0);
    assert_eq!(p_value(&evidence(10, 10.0, f64::NAN)), 1.0);
}
