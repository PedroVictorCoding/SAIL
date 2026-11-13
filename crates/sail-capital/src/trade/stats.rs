//! Evidence, confidence intervals and the publish verdict.

use chrono::{Months, NaiveDate};

use super::{Evidence, Exit, MIN_TRADES, SetupId, Side, Trade, Verdict};

const Z: f64 = 1.96;
const HOLDOUT_MONTHS: u32 = 24;
const MIN_HOLDOUT_TRADES: u32 = 20;

pub(super) fn evidence(setup: SetupId, side: Side, trades: &[Trade]) -> Evidence {
    let mut values: Vec<&Trade> = trades
        .iter()
        .filter(|trade| trade.setup == setup && trade.side == side)
        .collect();
    values.sort_by_key(|trade| (trade.exit_session, trade.signal_session));
    let count = values.len();
    if count == 0 {
        return Evidence {
            setup,
            side,
            trades: 0,
            hit_target1: 0.0,
            hit_target1_ci: (0.0, 0.0),
            expectancy_r: 0.0,
            expectancy_ci: (0.0, 0.0),
            profit_factor: 0.0,
            max_drawdown_r: 0.0,
            mean_sessions: 0.0,
        };
    }
    let returns: Vec<f64> = values.iter().map(|trade| trade.r_multiple).collect();
    let hits = values
        .iter()
        .filter(|trade| matches!(trade.exit, Exit::Target1 | Exit::Target2 | Exit::Target3))
        .count();
    let hit_target1 = hits as f64 / count as f64;
    let hit_target1_ci = wilson(hits as f64, count as f64);
    let expectancy_r = mean(&returns);
    let std = sample_std(&returns);
    let autocorrelation = autocorrelation(&returns);
    let standard_error = std / (count as f64).sqrt() * (1.0 + 2.0 * autocorrelation).sqrt();
    let expectancy_ci = (
        expectancy_r - Z * standard_error,
        expectancy_r + Z * standard_error,
    );
    let wins: f64 = returns.iter().filter(|value| **value > 0.0).sum();
    let losses: f64 = returns
        .iter()
        .filter(|value| **value < 0.0)
        .map(|value| -value)
        .sum();
    let profit_factor = if losses == 0.0 {
        if wins == 0.0 { 0.0 } else { 999.0 }
    } else {
        wins / losses
    };
    let mean_sessions = values
        .iter()
        .map(|trade| (trade.exit_session - trade.entry_session).num_days().max(0) as f64)
        .sum::<f64>()
        / count as f64;
    Evidence {
        setup,
        side,
        trades: count as u32,
        hit_target1,
        hit_target1_ci,
        expectancy_r,
        expectancy_ci,
        profit_factor,
        max_drawdown_r: max_drawdown(&returns),
        mean_sessions,
    }
}

/// z for a two-sided 1 − 0.05/36 bound (36 setup, side and group tests).
const BONFERRONI_Z: f64 = 3.197;

pub(super) fn verdict(
    group: &str,
    setup: SetupId,
    side: Side,
    trades: &[Trade],
    as_of: NaiveDate,
) -> Verdict {
    let cutoff = as_of
        .checked_sub_months(Months::new(HOLDOUT_MONTHS))
        .unwrap_or(NaiveDate::MIN);
    let relevant: Vec<Trade> = trades
        .iter()
        .filter(|trade| trade.setup == setup && trade.side == side && trade.exit_session <= as_of)
        .cloned()
        .collect();
    let selection_trades: Vec<Trade> = relevant
        .iter()
        .filter(|trade| trade.exit_session <= cutoff)
        .cloned()
        .collect();
    let holdout_trades: Vec<Trade> = relevant
        .iter()
        .filter(|trade| trade.exit_session > cutoff)
        .cloned()
        .collect();
    let selection = evidence(setup, side, &selection_trades);
    let holdout = evidence(setup, side, &holdout_trades);
    // 6 setups × 2 sides × 3 groups are tested at once, so about two would clear a 95%
    // bar by luck. The publish bar is Bonferroni-corrected: the mean must clear zero by
    // `BONFERRONI_Z` standard errors (1 − 0.05/36 two-sided, z ≈ 3.2). Cards still show
    // the familiar 95% interval.
    let se = (selection.expectancy_ci.1 - selection.expectancy_ci.0) / (2.0 * 1.959_964);
    let corrected_lower = selection.expectancy_r - BONFERRONI_Z * se;
    let publishes = selection.trades >= MIN_TRADES
        && selection.expectancy_ci.0 > 0.0
        && corrected_lower > 0.0
        && holdout.trades >= MIN_HOLDOUT_TRADES
        && holdout.expectancy_r > 0.0;
    let candidate = !publishes
        && selection.trades >= super::MIN_CANDIDATE_TRADES
        && selection.expectancy_r > 0.0
        && holdout.trades >= super::MIN_CANDIDATE_HOLDOUT_TRADES
        && holdout.expectancy_r > 0.0;
    let reason = if publishes {
        format!(
            "Publishes: {:+.2}R a trade (95% from {:+.2}R) over {} trades; holdout {:+.2}R over {} trades",
            selection.expectancy_r,
            selection.expectancy_ci.0,
            selection.trades,
            holdout.expectancy_r,
            holdout.trades
        )
    } else if selection.trades < MIN_TRADES {
        format!(
            "Doesn't publish: only {} trades in the selection period, fewer than {}",
            selection.trades, MIN_TRADES
        )
    } else if selection.expectancy_ci.0 <= 0.0 {
        format!(
            "Doesn't publish: the lower bound is {:+.2}R over {} trades",
            selection.expectancy_ci.0, selection.trades
        )
    } else if corrected_lower <= 0.0 {
        format!(
            "Doesn't publish: +{:.2}R a trade over {} trades isn't clear of luck once 36 setups are tested together (corrected bound {:+.2}R)",
            selection.expectancy_r, selection.trades, corrected_lower
        )
    } else if holdout.trades < MIN_HOLDOUT_TRADES {
        format!(
            "Doesn't publish: the holdout has only {} trades",
            holdout.trades
        )
    } else {
        format!(
            "Doesn't publish: holdout {:+.2}R over {} trades",
            holdout.expectancy_r, holdout.trades
        )
    };
    let reason = if candidate {
        format!(
            "Candidate: {:+.2}R a trade over {} trades and {:+.2}R over {} in the last two years, after costs, but not yet clear of luck. {}",
            selection.expectancy_r, selection.trades, holdout.expectancy_r, holdout.trades, reason
        )
    } else {
        reason
    };
    Verdict {
        group: group.into(),
        selection,
        holdout,
        publishes,
        candidate,
        reason,
    }
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

fn sample_std(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = mean(values);
    let variance = values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / (values.len() - 1) as f64;
    variance.sqrt()
}

fn autocorrelation(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = mean(values);
    let denominator: f64 = values.iter().map(|value| (value - mean).powi(2)).sum();
    if denominator <= 0.0 {
        return 0.0;
    }
    let numerator: f64 = values
        .windows(2)
        .map(|pair| (pair[0] - mean) * (pair[1] - mean))
        .sum();
    (numerator / denominator).max(0.0)
}

fn wilson(successes: f64, count: f64) -> (f64, f64) {
    if count <= 0.0 {
        return (0.0, 0.0);
    }
    let proportion = successes / count;
    let squared = Z * Z;
    let denominator = 1.0 + squared / count;
    let centre = (proportion + squared / (2.0 * count)) / denominator;
    let margin = Z
        * (proportion * (1.0 - proportion) / count + squared / (4.0 * count * count)).sqrt()
        / denominator;
    ((centre - margin).max(0.0), (centre + margin).min(1.0))
}

fn max_drawdown(values: &[f64]) -> f64 {
    let mut cumulative = 0.0_f64;
    let mut peak = 0.0_f64;
    let mut worst = 0.0_f64;
    for value in values {
        cumulative += value;
        peak = peak.max(cumulative);
        worst = worst.max(peak - cumulative);
    }
    worst
}
