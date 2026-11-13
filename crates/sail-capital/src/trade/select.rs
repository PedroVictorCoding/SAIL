//! Round 19: how setups earn the right to publish (Kilo's slice). Replaces the round 13
//! rule (Bonferroni-corrected 95% bound plus one fixed two-year holdout) in
//! [`super::signal_day_with`]. **This module doc is the spec.**
//!
//! # Inputs
//! Trades pooled per group (`"fx"`, `"stocks"`, `"indices"`, as [`super::group_of`]),
//! the live graded cards ([`LiveTrade`]) and the evaluation date `as_of`. **No look-ahead:**
//! every step uses only trades with `exit_session <= as_of` (and, inside the walk-forward,
//! only trades that exited before the fold start minus [`EMBARGO_DAYS`]).
//!
//! # Steps, per group, for each of the 12 setup and side cells
//! 1. **Evidence:** [`super::evidence`] over the cell's trades with
//!    `exit_session <= as_of`. This is `Verdict::selection`. `Verdict::holdout` stays the
//!    evidence over the last 24 months (exits in `(as_of − 24 months, as_of]`), for display
//!    only: it no longer gates anything.
//! 2. **p-value:** [`p_value`]: one-sided, H0 "mean R ≤ 0", from the normal approximation
//!    with the autocorrelation-inflated standard error that `Evidence::expectancy_ci`
//!    already uses: `se = (ci.1 − ci.0) / (2 · 1.959964)`, `z = mean / se`,
//!    `p = 1 − Φ(z)`. Fewer than 2 trades, or `se` not finite or ≤ 0: `p = 1`.
//! 3. **False discovery rate:** [`benjamini_hochberg`] over **all 36** p-values at once
//!    (3 groups × 6 setups × 2 sides) at `q = ` [`FDR_Q`]. Step-up: sort ascending,
//!    find the largest `k` with `p_(k) <= k / m · q`, reject `p_(1) … p_(k)`. Ties keep
//!    input order; NaN p-values never reject. `m` is the number of p-values passed.
//! 4. **Partial pooling:** [`pool`] over the group's 12 cells (normal–normal empirical
//!    Bayes, DerSimonian–Laird):
//!    - per cell `y_i = expectancy_r`, `v_i = se_i²`; cells with fewer than
//!      [`MIN_POOL_TRADES`] trades or a non-finite/zero `v_i` don't inform the prior and
//!      get `Pooled::default()` with `shrinkage = 1` and `posterior_mean = prior_mean`;
//!    - weights `w_i = 1/v_i`, `ȳ = Σw y / Σw`, `Q = Σ w (y − ȳ)²`,
//!      `τ² = max(0, (Q − (k − 1)) / (Σw − Σw² / Σw))` with `k` informing cells
//!      (`k < 2`: `τ² = 0`, prior mean = `ȳ`, or 0 when `k = 0`);
//!    - `prior_mean = Σ w* y / Σ w*` with `w* = 1/(v_i + τ²)`; `prior_sd = τ`;
//!    - `B_i = v_i / (v_i + τ²)` (1 when `τ² = 0`), `posterior_mean = B·prior + (1−B)·y`,
//!      `posterior_sd = sqrt((1 − B) · v_i)` (when `B = 1`, use `sqrt(v_i·τ²/(v_i+τ²))`
//!      which is 0 for `τ² = 0`; then the cell is exactly the prior mean).
//! 5. **Live update:** the cell's live closed cards ([`LiveTrade`], either tier, exit ≤
//!    `as_of`) update the pooled posterior as further normal observations with the
//!    cell's backtest per-trade variance `s² = v_i · n_i` (n_i = selection trades):
//!    `precision = 1/posterior_sd² + n_live/s²`,
//!    `mean = (posterior_mean/posterior_sd² + Σr_live/s²) / precision`. If
//!    `posterior_sd = 0`, the live trades don't move it. Result in [`LiveEvidence`] and
//!    `Judgement::posterior_mean`/`posterior_sd`;
//!    `posterior_lower = mean − 1.959964 · sd`.
//! 6. **Walk-forward:** [`walk_forward`]. [`WF_FOLDS`] consecutive folds of
//!    [`WF_FOLD_MONTHS`] months ending at `as_of`. For each fold `[start, end)`: rerun
//!    steps 1–4 (not 5) on trades with `exit_session < start − EMBARGO_DAYS` (purged: a
//!    trade that is still open at the fold start is excluded from the fit), and mark the
//!    cell `selected` when it would have been **verified** by steps 3–4 alone
//!    (BH-rejected and pooled posterior lower bound > 0). Then record the fold's
//!    out-of-sample trades: the cell's trades with `start <= signal_session < end` and
//!    `exit_session <= as_of`. `WalkForward::oos_*` pool the OOS trades of **selected**
//!    folds only. Fold boundaries use `NaiveDate::checked_sub_months`.
//!
//! # Verdicts
//! - **Verified** (`publishes`): BH-rejected (step 3) **and** `posterior_lower > 0`
//!   (step 5) **and** walk-forward `oos_trades >= ` [`MIN_OOS_TRADES`] with
//!   `oos_mean_r > 0`.
//! - **Candidate**: not verified, `selection.trades >= super::MIN_CANDIDATE_TRADES`,
//!   `posterior_mean > 0`, and over **all** walk-forward folds' OOS trades (selected or
//!   not) at least [`MIN_CANDIDATE_OOS_TRADES`] with a positive mean.
//! - `reason`: one sentence in the round 13 style, starting `"Publishes: "`,
//!   `"Candidate: "` or `"Doesn't publish: "`, naming the first failed condition in the
//!   order above, with its number (e.g. `"Doesn't publish: out of sample +0.00R over 12
//!   trades, fewer than 20"`). Never "will", "guaranteed" or "certain".
//! - `Verdict::judgement` is always `Some` from [`judge`].
//!
//! # Output
//! [`judge`] returns 36 verdicts in the order `["fx", "stocks", "indices"]` ×
//! [`super::SetupId::ALL`] × `[Long, Short]`, the order the scoreboard has today.
//! It is deterministic (same inputs, same output, bit for bit) and pure: no I/O, no
//! clock, no randomness.

use std::collections::BTreeMap;

use chrono::{Days, Months, NaiveDate};
use serde::{Deserialize, Serialize};

use super::{Evidence, SetupId, Side, Tier, Trade, Verdict};

/// False discovery rate across the 36 cells.
pub const FDR_Q: f64 = 0.10;
/// Fewest trades for a cell to inform the pooled prior.
pub const MIN_POOL_TRADES: u32 = 10;
/// Walk-forward folds.
pub const WF_FOLDS: u32 = 4;
/// Months per walk-forward fold.
pub const WF_FOLD_MONTHS: u32 = 12;
/// Calendar days between the fit data and a fold start (on top of purging open trades).
pub const EMBARGO_DAYS: u64 = 10;
/// Fewest out-of-sample trades (selected folds) for a verified signal.
pub const MIN_OOS_TRADES: u32 = 20;
/// Fewest out-of-sample trades (all folds) for a candidate.
pub const MIN_CANDIDATE_OOS_TRADES: u32 = 10;

/// One closed live card, as graded by the service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveTrade {
    /// Asset group of the card's instrument.
    pub group: String,
    /// Setup.
    pub setup: SetupId,
    /// Side.
    pub side: Side,
    /// Tier the card was published at.
    pub tier: Tier,
    /// Exit session.
    pub exit_session: NaiveDate,
    /// Result in R after costs.
    pub r_multiple: f64,
}

/// Step 4: one cell's pooled estimate.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pooled {
    /// The group's prior mean R.
    pub prior_mean: f64,
    /// The spread of true cell means (τ).
    pub prior_sd: f64,
    /// Weight on the prior, 0..=1.
    pub shrinkage: f64,
    /// Pooled mean R.
    pub posterior_mean: f64,
    /// Its standard deviation.
    pub posterior_sd: f64,
}

/// Step 5: the live record's part.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveEvidence {
    /// Closed live cards of this cell (both tiers).
    pub trades: u32,
    /// Their mean R (0 when none).
    pub mean_r: f64,
}

/// One walk-forward fold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fold {
    /// First day of the fold.
    pub start: NaiveDate,
    /// Day after the fold (exclusive).
    pub end: NaiveDate,
    /// Would have been verified at `start` (steps 3–4 on earlier data).
    pub selected: bool,
    /// Out-of-sample trades in the fold.
    pub trades: u32,
    /// Their mean R (0 when none).
    pub mean_r: f64,
}

/// Step 6.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WalkForward {
    /// Oldest first.
    pub folds: Vec<Fold>,
    /// Out-of-sample trades over selected folds.
    pub oos_trades: u32,
    /// Their mean R (0 when none).
    pub oos_mean_r: f64,
    /// 95% interval on that mean, as `Evidence::expectancy_ci` computes it.
    pub oos_ci: (f64, f64),
    /// Out-of-sample trades over all folds.
    pub all_trades: u32,
    /// Their mean R (0 when none).
    pub all_mean_r: f64,
}

/// Everything behind a verdict, for the page and the voice tool.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Judgement {
    /// Step 2.
    pub p_value: f64,
    /// Step 3.
    pub fdr_pass: bool,
    /// Step 4.
    pub pooled: Pooled,
    /// Step 5.
    pub live: LiveEvidence,
    /// Posterior after the live update.
    pub posterior_mean: f64,
    /// Its standard deviation.
    pub posterior_sd: f64,
    /// `posterior_mean − 1.959964 · posterior_sd`.
    pub posterior_lower: f64,
    /// Step 6.
    pub walk_forward: WalkForward,
}

/// Step 2. Kilo, round 19.
pub fn p_value(evidence: &Evidence) -> f64 {
    if evidence.trades < 2 {
        return 1.0;
    }
    let se = (evidence.expectancy_ci.1 - evidence.expectancy_ci.0) / (2.0 * Z95);
    if !se.is_finite() || se <= 0.0 {
        return 1.0;
    }
    let z = evidence.expectancy_r / se;
    if z.is_nan() {
        return f64::NAN;
    }
    normal_survival(z)
}

/// Step 3: which p-values are rejected at `q`. Kilo, round 19.
pub fn benjamini_hochberg(p_values: &[f64], q: f64) -> Vec<bool> {
    let mut rejected = vec![false; p_values.len()];
    if p_values.is_empty() {
        return rejected;
    }
    let m = p_values.len() as f64;
    let mut ordered: Vec<(usize, f64)> = p_values
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, p)| p.is_finite())
        .collect();
    ordered.sort_by(|left, right| left.1.total_cmp(&right.1));
    let mut last = 0usize;
    for (rank, (_, p)) in ordered.iter().enumerate() {
        let rank = (rank + 1) as f64;
        if *p <= rank / m * q {
            last = rank as usize;
        }
    }
    for (index, _) in ordered.into_iter().take(last) {
        rejected[index] = true;
    }
    rejected
}

/// Step 4 over one group's cells (any order; output in the same order). Kilo, round 19.
pub fn pool(cells: &[Evidence]) -> Vec<Pooled> {
    let mut out = vec![Pooled::default(); cells.len()];
    let mut informing: Vec<(usize, f64, f64)> = Vec::new();
    for (index, cell) in cells.iter().enumerate() {
        if cell.trades < MIN_POOL_TRADES {
            continue;
        }
        let se = (cell.expectancy_ci.1 - cell.expectancy_ci.0) / (2.0 * Z95);
        let variance = se * se;
        if !variance.is_finite() || variance <= 0.0 {
            continue;
        }
        informing.push((index, cell.expectancy_r, variance));
    }
    let k = informing.len();
    if k == 0 {
        for cell in &mut out {
            cell.shrinkage = 1.0;
        }
        return out;
    }
    if k == 1 {
        let prior = informing[0].1;
        for cell in &mut out {
            cell.prior_mean = prior;
            cell.shrinkage = 1.0;
            cell.posterior_mean = prior;
        }
        return out;
    }
    let sum_w: f64 = informing.iter().map(|(_, _, v)| 1.0 / v).sum();
    let sum_w2: f64 = informing.iter().map(|(_, _, v)| (1.0 / v).powi(2)).sum();
    let y_bar: f64 = informing.iter().map(|(_, y, v)| y / v).sum::<f64>() / sum_w;
    let q: f64 = informing
        .iter()
        .map(|(_, y, v)| (y - y_bar).powi(2) / v)
        .sum();
    let denominator = sum_w - sum_w2 / sum_w;
    let tau2 = if denominator > 0.0 {
        ((q - (k as f64 - 1.0)) / denominator).max(0.0)
    } else {
        0.0
    };
    let sum_star: f64 = informing.iter().map(|(_, _, v)| 1.0 / (v + tau2)).sum();
    let prior_mean: f64 = informing
        .iter()
        .map(|(_, y, v)| y / (v + tau2))
        .sum::<f64>()
        / sum_star;
    let prior_sd = tau2.sqrt();
    for (index, cell) in out.iter_mut().enumerate() {
        cell.prior_mean = prior_mean;
        cell.prior_sd = prior_sd;
        let Some((_, y, variance)) = informing.iter().find(|(at, _, _)| *at == index) else {
            cell.shrinkage = 1.0;
            cell.posterior_mean = prior_mean;
            continue;
        };
        let b = variance / (variance + tau2);
        cell.shrinkage = b;
        cell.posterior_mean = b * prior_mean + (1.0 - b) * y;
        cell.posterior_sd = if b >= 1.0 {
            (variance * tau2 / (variance + tau2)).sqrt()
        } else {
            ((1.0 - b) * variance).sqrt()
        };
    }
    out
}

/// Step 6 for every cell of every group, keyed by `(group, setup, side)`. Kilo, round 19.
pub fn walk_forward(
    trades_by_group: &BTreeMap<String, Vec<Trade>>,
    as_of: NaiveDate,
) -> BTreeMap<(String, SetupId, Side), WalkForward> {
    let cells = cells_in_order();
    let mut folds: BTreeMap<(String, SetupId, Side), Vec<Fold>> = cells
        .iter()
        .map(|key| (key.clone(), Vec::with_capacity(WF_FOLDS as usize)))
        .collect();
    let mut selected_trades: Vec<Vec<Trade>> = vec![Vec::new(); cells.len()];
    let mut all_trades: Vec<Vec<Trade>> = vec![Vec::new(); cells.len()];

    for (start, end) in fold_bounds(as_of) {
        let cutoff = start
            .checked_sub_days(Days::new(EMBARGO_DAYS))
            .unwrap_or(NaiveDate::MIN);
        let fit_groups: Vec<Vec<Trade>> = GROUPS
            .iter()
            .map(|group| {
                trades_by_group
                    .get(*group)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                    .iter()
                    .filter(|trade| trade.exit_session < cutoff)
                    .cloned()
                    .collect()
            })
            .collect();
        let fit_evidence: Vec<Evidence> = cells
            .iter()
            .enumerate()
            .map(|(index, (_, setup, side))| {
                super::stats::evidence(*setup, *side, &fit_groups[group_index(index)])
            })
            .collect();
        let p_values: Vec<f64> = fit_evidence.iter().map(p_value).collect();
        let rejected = benjamini_hochberg(&p_values, FDR_Q);
        let pooled: Vec<Vec<Pooled>> = (0..GROUPS.len())
            .map(|group| {
                let from = group * SETUPS_PER_GROUP;
                pool(&fit_evidence[from..from + SETUPS_PER_GROUP])
            })
            .collect();
        let selected: Vec<bool> = cells
            .iter()
            .enumerate()
            .map(|(index, _)| {
                let group = group_index(index);
                let estimate = &pooled[group][index - group * SETUPS_PER_GROUP];
                rejected[index] && estimate.posterior_mean - Z95 * estimate.posterior_sd > 0.0
            })
            .collect();

        for (index, (group, setup, side)) in cells.iter().enumerate() {
            let cell_trades: Vec<&Trade> = trades_by_group
                .get(group)
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .filter(|trade| {
                    trade.setup == *setup
                        && trade.side == *side
                        && trade.signal_session >= start
                        && trade.signal_session < end
                        && trade.exit_session <= as_of
                })
                .collect();
            let count = cell_trades.len() as u32;
            let mean_r = mean_of(cell_trades.iter().map(|trade| trade.r_multiple));
            if let Some(fold) = folds.get_mut(&(group.clone(), *setup, *side)) {
                fold.push(Fold {
                    start,
                    end,
                    selected: selected[index],
                    trades: count,
                    mean_r,
                });
            }
            all_trades[index].extend(cell_trades.iter().map(|trade| (*trade).clone()));
            if selected[index] {
                selected_trades[index].extend(cell_trades.into_iter().cloned());
            }
        }
    }

    let mut out = BTreeMap::new();
    for (index, (group, setup, side)) in cells.into_iter().enumerate() {
        let oos = &selected_trades[index];
        let all = &all_trades[index];
        let walk_forward = WalkForward {
            folds: folds
                .remove(&(group.clone(), setup, side))
                .unwrap_or_default(),
            oos_trades: oos.len() as u32,
            oos_mean_r: mean_of(oos.iter().map(|trade| trade.r_multiple)),
            oos_ci: super::stats::evidence(setup, side, oos).expectancy_ci,
            all_trades: all.len() as u32,
            all_mean_r: mean_of(all.iter().map(|trade| trade.r_multiple)),
        };
        out.insert((group, setup, side), walk_forward);
    }
    out
}

/// Steps 1–6 and the verdicts, in scoreboard order. Kilo, round 19: until then this keeps
/// the round 13 rule so the build and the page work.
pub fn judge(
    trades_by_group: &BTreeMap<String, Vec<Trade>>,
    live: &[LiveTrade],
    as_of: NaiveDate,
) -> Vec<Verdict> {
    let cells = cells_in_order();
    let cutoff = as_of
        .checked_sub_months(Months::new(HOLDOUT_MONTHS))
        .unwrap_or(NaiveDate::MIN);
    let selection_groups: Vec<Vec<Trade>> = GROUPS
        .iter()
        .map(|group| {
            trades_by_group
                .get(*group)
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .filter(|trade| trade.exit_session <= as_of)
                .cloned()
                .collect()
        })
        .collect();
    let selection: Vec<Evidence> = cells
        .iter()
        .enumerate()
        .map(|(index, (_, setup, side))| {
            super::stats::evidence(*setup, *side, &selection_groups[group_index(index)])
        })
        .collect();
    let holdout: Vec<Evidence> = cells
        .iter()
        .enumerate()
        .map(|(index, (_, setup, side))| {
            let trades: Vec<Trade> = selection_groups[group_index(index)]
                .iter()
                .filter(|trade| trade.exit_session > cutoff)
                .cloned()
                .collect();
            super::stats::evidence(*setup, *side, &trades)
        })
        .collect();
    let p_values: Vec<f64> = selection.iter().map(p_value).collect();
    let fdr = benjamini_hochberg(&p_values, FDR_Q);
    let pooled: Vec<Vec<Pooled>> = (0..GROUPS.len())
        .map(|group| {
            let from = group * SETUPS_PER_GROUP;
            pool(&selection[from..from + SETUPS_PER_GROUP])
        })
        .collect();
    let walk = walk_forward(trades_by_group, as_of);

    let mut out = Vec::with_capacity(cells.len());
    for (index, (group, setup, side)) in cells.into_iter().enumerate() {
        let group_at = group_index(index);
        let estimate = &pooled[group_at][index - group_at * SETUPS_PER_GROUP];
        let cell_live: Vec<&LiveTrade> = live
            .iter()
            .filter(|card| {
                card.group == group
                    && card.setup == setup
                    && card.side == side
                    && card.exit_session <= as_of
            })
            .collect();
        let live_evidence = LiveEvidence {
            trades: cell_live.len() as u32,
            mean_r: mean_of(cell_live.iter().map(|card| card.r_multiple)),
        };
        let (posterior_mean, posterior_sd) =
            live_update(estimate, &selection[index], &cell_live, &live_evidence);
        let posterior_lower = posterior_mean - Z95 * posterior_sd;
        let walk_forward = walk
            .get(&(group.clone(), setup, side))
            .cloned()
            .unwrap_or_default();
        let publishes = fdr[index]
            && posterior_lower > 0.0
            && walk_forward.oos_trades >= MIN_OOS_TRADES
            && walk_forward.oos_mean_r > 0.0;
        let candidate = !publishes
            && selection[index].trades >= super::MIN_CANDIDATE_TRADES
            && posterior_mean > 0.0
            && walk_forward.all_trades >= MIN_CANDIDATE_OOS_TRADES
            && walk_forward.all_mean_r > 0.0;
        let reason = verdict_reason(
            publishes,
            candidate,
            &group,
            &selection[index],
            &live_evidence,
            p_values[index],
            fdr[index],
            posterior_mean,
            posterior_lower,
            &walk_forward,
        );
        out.push(Verdict {
            group: group.clone(),
            selection: selection[index].clone(),
            holdout: holdout[index].clone(),
            publishes,
            candidate,
            reason,
            judgement: Some(Judgement {
                p_value: p_values[index],
                fdr_pass: fdr[index],
                pooled: estimate.clone(),
                live: live_evidence,
                posterior_mean,
                posterior_sd,
                posterior_lower,
                walk_forward,
            }),
        });
    }
    out
}

const Z95: f64 = 1.959_964;
const HOLDOUT_MONTHS: u32 = 24;
const GROUPS: [&str; 3] = ["fx", "stocks", "indices"];
const SETUPS_PER_GROUP: usize = 12;

fn cells_in_order() -> Vec<(String, SetupId, Side)> {
    let mut cells = Vec::with_capacity(GROUPS.len() * SETUPS_PER_GROUP);
    for group in GROUPS {
        for setup in SetupId::ALL {
            for side in [Side::Long, Side::Short] {
                cells.push((group.to_string(), setup, side));
            }
        }
    }
    cells
}

fn group_index(cell: usize) -> usize {
    cell / SETUPS_PER_GROUP
}

fn fold_bounds(as_of: NaiveDate) -> Vec<(NaiveDate, NaiveDate)> {
    let mut bounds = Vec::with_capacity(WF_FOLDS as usize + 1);
    for step in (0..=WF_FOLDS).rev() {
        let months = step * WF_FOLD_MONTHS;
        bounds.push(
            as_of
                .checked_sub_months(Months::new(months))
                .unwrap_or(NaiveDate::MIN),
        );
    }
    bounds.windows(2).map(|pair| (pair[0], pair[1])).collect()
}

fn live_update(
    pooled: &Pooled,
    selection: &Evidence,
    live: &[&LiveTrade],
    live_evidence: &LiveEvidence,
) -> (f64, f64) {
    if live.is_empty() || pooled.posterior_sd == 0.0 {
        return (pooled.posterior_mean, pooled.posterior_sd);
    }
    let se = (selection.expectancy_ci.1 - selection.expectancy_ci.0) / (2.0 * Z95);
    let backtest_variance = se * se * selection.trades as f64;
    if !backtest_variance.is_finite() || backtest_variance <= 0.0 {
        return (pooled.posterior_mean, pooled.posterior_sd);
    }
    let prior_precision = 1.0 / pooled.posterior_sd.powi(2);
    let precision = prior_precision + live_evidence.trades as f64 / backtest_variance;
    if !precision.is_finite() || precision <= 0.0 {
        return (pooled.posterior_mean, pooled.posterior_sd);
    }
    let sum: f64 = live.iter().map(|card| card.r_multiple).sum();
    let mean = (pooled.posterior_mean * prior_precision + sum / backtest_variance) / precision;
    (mean, (1.0 / precision).sqrt())
}

#[allow(clippy::too_many_arguments)]
fn verdict_reason(
    publishes: bool,
    candidate: bool,
    _group: &str,
    selection: &Evidence,
    live: &LiveEvidence,
    p_value: f64,
    fdr_pass: bool,
    posterior_mean: f64,
    posterior_lower: f64,
    walk: &WalkForward,
) -> String {
    if publishes {
        return format!(
            "Publishes: {:+.2}R a trade over {} trades, pooled lower bound {:+.2}R, out of sample {:+.2}R over {} trades",
            selection.expectancy_r,
            selection.trades,
            posterior_lower,
            walk.oos_mean_r,
            walk.oos_trades
        );
    }
    let failed = if !fdr_pass {
        format!("the false-discovery-rate test did not reject this cell (p = {p_value:.3})")
    } else if posterior_lower <= 0.0 {
        format!("the pooled lower bound is {posterior_lower:+.2}R")
    } else if walk.oos_trades < MIN_OOS_TRADES {
        format!(
            "out of sample {:+.2}R over {} trades, fewer than {}",
            walk.oos_mean_r, walk.oos_trades, MIN_OOS_TRADES
        )
    } else if walk.oos_mean_r <= 0.0 {
        format!(
            "out of sample {:+.2}R over {} trades",
            walk.oos_mean_r, walk.oos_trades
        )
    } else if selection.trades < super::MIN_CANDIDATE_TRADES {
        format!(
            "only {} trades in the selection period, fewer than {}",
            selection.trades,
            super::MIN_CANDIDATE_TRADES
        )
    } else if posterior_mean <= 0.0 {
        format!("the pooled mean is {posterior_mean:+.2}R")
    } else if walk.all_trades < MIN_CANDIDATE_OOS_TRADES {
        format!(
            "out of sample {:+.2}R over {} trades over all folds, fewer than {}",
            walk.all_mean_r, walk.all_trades, MIN_CANDIDATE_OOS_TRADES
        )
    } else {
        format!(
            "out of sample {:+.2}R over {} trades over all folds",
            walk.all_mean_r, walk.all_trades
        )
    };
    if candidate {
        format!(
            "Candidate: {failed}; {:+.2}R a trade over {} trades with {} live, not yet clear of luck",
            selection.expectancy_r, selection.trades, live.trades
        )
    } else {
        format!("Doesn't publish: {failed}")
    }
}

fn mean_of(values: impl Iterator<Item = f64>) -> f64 {
    let mut count = 0u32;
    let mut sum = 0.0;
    for value in values {
        sum += value;
        count += 1;
    }
    if count == 0 { 0.0 } else { sum / count as f64 }
}

fn normal_survival(z: f64) -> f64 {
    0.5 * erfc(z / std::f64::consts::SQRT_2)
}

fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let answer = t
        * (-z * z - 1.265_512_23
            + t * (1.000_023_68
                + t * (0.374_091_96
                    + t * (0.096_784_18
                        + t * (-0.186_288_06
                            + t * (0.278_868_07
                                + t * (-1.135_203_98
                                    + t * (1.488_515_87
                                        + t * (-0.822_152_23 + t * 0.170_872_77)))))))))
            .exp();
    if x >= 0.0 { answer } else { 2.0 - answer }
}
