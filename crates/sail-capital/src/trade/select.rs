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

use chrono::NaiveDate;
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
    let _ = evidence;
    1.0
}

/// Step 3: which p-values are rejected at `q`. Kilo, round 19.
pub fn benjamini_hochberg(p_values: &[f64], q: f64) -> Vec<bool> {
    let _ = q;
    vec![false; p_values.len()]
}

/// Step 4 over one group's cells (any order; output in the same order). Kilo, round 19.
pub fn pool(cells: &[Evidence]) -> Vec<Pooled> {
    vec![Pooled::default(); cells.len()]
}

/// Step 6 for every cell of every group, keyed by `(group, setup, side)`. Kilo, round 19.
pub fn walk_forward(
    trades_by_group: &BTreeMap<String, Vec<Trade>>,
    as_of: NaiveDate,
) -> BTreeMap<(String, SetupId, Side), WalkForward> {
    let _ = (trades_by_group, as_of);
    BTreeMap::new()
}

/// Steps 1–6 and the verdicts, in scoreboard order. Kilo, round 19: until then this keeps
/// the round 13 rule so the build and the page work.
pub fn judge(
    trades_by_group: &BTreeMap<String, Vec<Trade>>,
    live: &[LiveTrade],
    as_of: NaiveDate,
) -> Vec<Verdict> {
    let _ = live;
    let mut out = Vec::with_capacity(36);
    for group in ["fx", "stocks", "indices"] {
        let trades = trades_by_group.get(group).map(Vec::as_slice).unwrap_or(&[]);
        for setup in SetupId::ALL {
            for side in [Side::Long, Side::Short] {
                out.push(super::stats::verdict(group, setup, side, trades, as_of));
            }
        }
    }
    out
}
