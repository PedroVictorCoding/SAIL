//! Turning limits and signals into sized changes (round 11, CAP-18, Kilo's slice).
//!
//! One long-only problem over the owned instruments plus free funds, with weights as
//! shares of the account value. There are two modes.
//!
//! - **Policy.** The smallest change that satisfies the owner's limits:
//!   `minimize Σ (w_i − w0_i)²` over the constraints below. Signals play no part. If the
//!   portfolio is already within limits, this proposes nothing.
//! - **Ideas.** The best trade-off between expected return, risk and cost:
//!   `maximize α'w − (λ/2)·w'Σw − Σ c_i·|w_i − w0_i|` over the same constraints. Here
//!   `α` is from `signals` and `Σ` is the covariance at the same horizon. `λ` is the
//!   policy's `risk_aversion`, and `c_i` is the trade cost (plus the FX cost for
//!   instruments in another currency), as a fraction. Ideas run only when at least one
//!   signal survived validation, i.e. some `α_j ≠ 0`. Otherwise the plan is "do nothing"
//!   and says why. Risk aversion alone must never turn into trading advice.
//!
//! **Constraints**, from `policy::RiskLimits`:
//! - `w_i ≥ 0`;
//! - `w_i ≤ max_position` for tradable instruments;
//! - the sum of `w_i` over high-volatility instruments `≤ max_high_vol`;
//! - `cash_min ≤ cash ≤ cash_max`;
//! - `Σ w_i + cash = 1`;
//! - `½·Σ|w_i − w0_i| ≤ max_turnover`.
//!
//! **Locked** instruments (no prices, or CFDs, which are excluded upstream) keep their
//! weight. If the limits can't all be met, the plan does as much as it can, marks
//! `feasible: false` and lists what remains. Changes smaller than `min_trade` are
//! dropped, and the plan is re-checked after dropping them.
//!
//! The solver must be deterministic (same input, same output). It must be checked
//! against brute force on small problems. Projected gradient, an active-set QP, or
//! coordinate descent are all fine; no new dependency is needed.
use std::collections::BTreeMap;

use crate::forecast::CloseSeries;

use crate::policy::RiskLimits;
use chrono::Datelike;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

mod solver;

/// Slack used when comparing shares, far below anything a person would notice.
const EPSILON: f64 = 1e-9;

/// Weekly closes kept for the covariance.
const WEEKS: usize = 104;

/// Fewest common weekly returns a covariance pair needs.
const MIN_COMMON_WEEKS: usize = 26;

/// Which problem to solve.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Smallest change that meets the limits.
    Policy,
    /// Return, risk and cost trade-off within the limits.
    Ideas,
}

/// One owned instrument in the problem.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    /// Ledger instrument key.
    pub instrument_key: String,
    /// Broker symbol.
    pub symbol: String,
    /// Current share of the account value, 0..=1.
    pub weight: f64,
    /// `α_j` over the horizon (0 without a signal).
    pub alpha: f64,
    /// Annualized volatility, when known (decides `high_vol`).
    pub sigma_annual: Option<f64>,
    /// Trades in a currency other than the account's.
    pub foreign_currency: bool,
    /// Keeps its weight: no prices, or not tradable here.
    pub locked: bool,
}

/// The problem.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    /// Instruments, in a fixed order (the covariance uses the same order).
    pub assets: Vec<Asset>,
    /// Current free funds share, 0..=1. `Σ weight + cash = 1`.
    pub cash: f64,
    /// Covariance of horizon log returns, `assets.len()²`, row-major. Missing pairs are
    /// 0 off the diagonal and `σ²` on it.
    pub covariance: Vec<f64>,
    /// Account value in the account currency, for sizing.
    pub account_value: f64,
    /// Horizon in sessions the `alpha` and `covariance` are for.
    pub horizon_sessions: u32,
}

/// Which limit a breach or a change is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    /// One position above `max_position_pct`.
    MaxPosition,
    /// High-volatility instruments together above `max_high_vol_pct`.
    HighVolShare,
    /// Free funds below `cash_min_pct`.
    CashReserve,
    /// Free funds above `cash_max_pct`.
    IdleCash,
    /// A signal (ideas mode).
    Signal,
}

/// A limit the portfolio breaks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Breach {
    /// The rule.
    pub rule: Rule,
    /// The instrument, for `MaxPosition`.
    pub symbol: Option<String>,
    /// Current share, percent.
    pub actual_pct: f64,
    /// The limit, percent.
    pub limit_pct: f64,
}

/// One proposed change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// Ledger instrument key.
    pub instrument_key: String,
    /// Broker symbol.
    pub symbol: String,
    /// Share before, percent.
    pub from_pct: f64,
    /// Share after, percent.
    pub to_pct: f64,
    /// Signed amount in the account currency (positive adds), rounded to whole units.
    pub amount: f64,
    /// Why: the rules it serves.
    pub reasons: Vec<Rule>,
}

/// The result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// Mode solved.
    pub mode: Mode,
    /// Changes, largest absolute amount first. Empty means "do nothing".
    pub changes: Vec<Change>,
    /// Free funds before and after, percent.
    pub cash_from_pct: f64,
    /// Free funds after, percent.
    pub cash_to_pct: f64,
    /// Breaches before.
    pub breaches_before: Vec<Breach>,
    /// Breaches after (non-empty only when `feasible` is false).
    pub breaches_after: Vec<Breach>,
    /// Whether every limit is met after the changes.
    pub feasible: bool,
    /// Ideas mode: `α'w` gain over the horizon, in the account currency.
    pub expected_gain: f64,
    /// Ideas mode: change in `(λ/2)·w'Σw`, in the account currency (negative is less risk).
    pub risk_change: f64,
    /// Estimated one-off cost of the changes, in the account currency.
    pub cost: f64,
    /// Ideas mode: `expected_gain − risk_change − cost`. The plan proposes changes only
    /// when this is above 0. Otherwise `changes` is empty and `note` says that doing
    /// nothing is best.
    pub net_benefit: f64,
    /// One plain sentence: what the plan does, or why it does nothing.
    pub note: String,
}

/// The limits as fractions of the account value, with costs as fractions of a trade.
struct Fractions {
    max_position: f64,
    cash_min: f64,
    cash_max: f64,
    high_vol_threshold: f64,
    max_high_vol: f64,
    max_turnover: f64,
    min_trade: f64,
    trade_cost: f64,
    fx_cost: f64,
    risk_aversion: f64,
}

fn decimal_f64(value: Decimal) -> f64 {
    value
        .to_f64()
        .filter(|number| number.is_finite())
        .unwrap_or(0.0)
}

impl Fractions {
    fn new(limits: &RiskLimits) -> Self {
        let percent = |value: Decimal| decimal_f64(value).max(0.0) / 100.0;
        Self {
            max_position: percent(limits.max_position_pct),
            cash_min: percent(limits.cash_min_pct),
            cash_max: percent(limits.cash_max_pct),
            high_vol_threshold: percent(limits.high_vol_threshold_pct),
            max_high_vol: percent(limits.max_high_vol_pct),
            max_turnover: percent(limits.max_turnover_pct),
            min_trade: decimal_f64(limits.min_trade).max(0.0),
            trade_cost: decimal_f64(limits.trade_cost_bps).max(0.0) / 10_000.0,
            fx_cost: decimal_f64(limits.fx_cost_bps).max(0.0) / 10_000.0,
            risk_aversion: decimal_f64(limits.risk_aversion).max(0.0),
        }
    }
}

/// The limits the portfolio breaks now.
pub fn breaches(problem: &Problem, limits: &RiskLimits) -> Vec<Breach> {
    let fractions = Fractions::new(limits);
    let weights: Vec<f64> = problem.assets.iter().map(|asset| asset.weight).collect();
    let high_vol: Vec<bool> = problem
        .assets
        .iter()
        .map(|asset| is_high_vol(asset, &fractions))
        .collect();
    breach_list(
        &weights,
        problem.cash,
        &high_vol,
        &problem.assets,
        &fractions,
    )
}

fn is_high_vol(asset: &Asset, fractions: &Fractions) -> bool {
    asset
        .sigma_annual
        .is_some_and(|value| value.is_finite() && value > fractions.high_vol_threshold)
}

fn breach_list(
    weights: &[f64],
    cash: f64,
    high_vol: &[bool],
    assets: &[Asset],
    fractions: &Fractions,
) -> Vec<Breach> {
    let mut breaches = Vec::new();
    for (index, weight) in weights.iter().enumerate() {
        if *weight > fractions.max_position + EPSILON {
            breaches.push(Breach {
                rule: Rule::MaxPosition,
                symbol: Some(assets[index].symbol.clone()),
                actual_pct: weight * 100.0,
                limit_pct: fractions.max_position * 100.0,
            });
        }
    }
    let high_total: f64 = weights
        .iter()
        .zip(high_vol.iter())
        .filter(|(_, flag)| **flag)
        .map(|(weight, _)| *weight)
        .sum();
    if high_total > fractions.max_high_vol + EPSILON {
        breaches.push(Breach {
            rule: Rule::HighVolShare,
            symbol: None,
            actual_pct: high_total * 100.0,
            limit_pct: fractions.max_high_vol * 100.0,
        });
    }
    if cash < fractions.cash_min - EPSILON {
        breaches.push(Breach {
            rule: Rule::CashReserve,
            symbol: None,
            actual_pct: cash * 100.0,
            limit_pct: fractions.cash_min * 100.0,
        });
    } else if cash > fractions.cash_max + EPSILON {
        breaches.push(Breach {
            rule: Rule::IdleCash,
            symbol: None,
            actual_pct: cash * 100.0,
            limit_pct: fractions.cash_max * 100.0,
        });
    }
    breaches
}

/// Solves `problem` in `mode` under `limits`.
pub fn plan(problem: &Problem, limits: &RiskLimits, mode: Mode) -> Plan {
    let fractions = Fractions::new(limits);
    let setup = Setup::new(problem, fractions);
    let before = breaches(problem, limits);

    if !setup.structural {
        return unchanged(
            problem,
            mode,
            before,
            "These limits cannot all be met with the current holdings, because the locked positions alone break them.",
        );
    }

    match mode {
        Mode::Policy => {
            if before.is_empty() {
                return unchanged(
                    problem,
                    mode,
                    before,
                    "Every limit is met; no change is needed.",
                );
            }
            let point = setup.policy_point();
            let within = setup.feasible_set().holds(&point, 1e-6);
            let mut weights = setup.final_weights(&point);
            setup.apply_min_trade(&mut weights);
            if !setup.changed(&weights) {
                return unchanged(
                    problem,
                    mode,
                    before,
                    "No change large enough to matter would fix the limits, so nothing is proposed.",
                );
            }
            assemble(&setup, mode, weights, before, within)
        }
        Mode::Ideas => {
            if !setup.has_alpha() {
                return unchanged(
                    problem,
                    mode,
                    before,
                    "No signal passed validation, so doing nothing is best.",
                );
            }
            let point = setup.ideas_point();
            let within = setup.feasible_set().holds(&point, 1e-6);
            let mut weights = setup.final_weights(&point);
            setup.apply_min_trade(&mut weights);
            let (gain, risk_change, cost) = setup.metrics(&weights);
            let net = gain - risk_change - cost;
            if !setup.changed(&weights) || net.is_nan() || net <= 0.0 {
                return unchanged(
                    problem,
                    mode,
                    before,
                    "Doing nothing is best: the expected gain does not cover the extra risk and trading costs.",
                );
            }
            assemble(&setup, mode, weights, before, within)
        }
    }
}

fn unchanged(problem: &Problem, mode: Mode, before: Vec<Breach>, note: &str) -> Plan {
    let cash_pct = problem.cash * 100.0;
    let feasible = before.is_empty();
    Plan {
        mode,
        changes: Vec::new(),
        cash_from_pct: cash_pct,
        cash_to_pct: cash_pct,
        breaches_before: before.clone(),
        breaches_after: before,
        feasible,
        expected_gain: 0.0,
        risk_change: 0.0,
        cost: 0.0,
        net_benefit: 0.0,
        note: note.into(),
    }
}

fn assemble(
    setup: &Setup<'_>,
    mode: Mode,
    weights: Vec<f64>,
    before: Vec<Breach>,
    within: bool,
) -> Plan {
    let problem = setup.problem;
    let cash = setup.cash_after(&weights);
    let after = breach_list(
        &weights,
        cash,
        &setup.high_vol,
        &problem.assets,
        &setup.fractions,
    );
    let changes = setup.build_changes(&weights, &before, mode);
    let (gain, risk_change, cost) = setup.metrics(&weights);
    let net = gain - risk_change - cost;
    let feasible = after.is_empty() && within;
    let count = changes.len();
    let note = match mode {
        Mode::Policy if changes.is_empty() => format!(
            "No change is proposed; {} limit(s) still need attention.",
            after.len()
        ),
        Mode::Policy if feasible => {
            format!("{count} change(s) bring the portfolio within your limits.")
        }
        Mode::Policy => format!(
            "{count} change(s) reduce the breaches, but {} remain because locked positions or the turnover limit prevent a full fix.",
            after.len()
        ),
        Mode::Ideas => format!(
            "{count} change(s) have an expected net gain of {net:.0} after risk and trading costs; this is an idea, not an instruction."
        ),
    };
    let ideas = mode == Mode::Ideas;
    Plan {
        mode,
        changes,
        cash_from_pct: problem.cash * 100.0,
        cash_to_pct: cash * 100.0,
        breaches_before: before,
        breaches_after: after,
        feasible,
        expected_gain: if ideas { gain } else { 0.0 },
        risk_change: if ideas { risk_change } else { 0.0 },
        cost,
        net_benefit: if ideas { net } else { 0.0 },
        note,
    }
}

/// The problem prepared once: the tradable subset, the constraint numbers and the
/// covariance in the tradable order.
struct Setup<'a> {
    problem: &'a Problem,
    fractions: Fractions,
    high_vol: Vec<bool>,
    free: Vec<usize>,
    upper: Vec<f64>,
    free_high: Vec<bool>,
    center: Vec<f64>,
    sum_min: f64,
    sum_max: f64,
    high_cap: f64,
    turnover_radius: f64,
    structural: bool,
    covariance: Vec<f64>,
    free_covariance: Vec<f64>,
    alpha: Vec<f64>,
    cost: Vec<f64>,
    free_cost: Vec<f64>,
    cross: Vec<f64>,
}

impl<'a> Setup<'a> {
    fn new(problem: &'a Problem, fractions: Fractions) -> Self {
        let count = problem.assets.len();
        let high_vol: Vec<bool> = problem
            .assets
            .iter()
            .map(|asset| is_high_vol(asset, &fractions))
            .collect();
        let free: Vec<usize> = (0..count)
            .filter(|index| !problem.assets[*index].locked)
            .collect();
        let upper = vec![fractions.max_position; free.len()];
        let free_high: Vec<bool> = free.iter().map(|index| high_vol[*index]).collect();
        let center: Vec<f64> = free
            .iter()
            .map(|index| problem.assets[*index].weight)
            .collect();
        let locked_weight: f64 = problem
            .assets
            .iter()
            .filter(|asset| asset.locked)
            .map(|asset| asset.weight)
            .sum();
        let locked_high: f64 = problem
            .assets
            .iter()
            .zip(high_vol.iter())
            .filter(|(asset, flag)| asset.locked && **flag)
            .map(|(asset, _)| asset.weight)
            .sum();
        let reachable: f64 = upper.iter().sum();
        let sum_min = (1.0 - fractions.cash_max - locked_weight).max(0.0);
        let sum_max = (1.0 - fractions.cash_min - locked_weight).min(reachable);
        let high_cap = fractions.max_high_vol - locked_high;
        let structural =
            sum_min <= sum_max + EPSILON && high_cap >= -EPSILON && sum_max >= -EPSILON;
        let turnover_radius = 2.0 * fractions.max_turnover;

        let mut covariance = vec![0.0; count * count];
        for row in 0..count {
            for column in 0..count {
                covariance[row * count + column] = problem
                    .covariance
                    .get(row * count + column)
                    .copied()
                    .filter(|value| value.is_finite())
                    .unwrap_or(0.0);
            }
        }
        let tradable = free.len();
        let mut free_covariance = vec![0.0; tradable * tradable];
        for (row, left) in free.iter().enumerate() {
            for (column, right) in free.iter().enumerate() {
                free_covariance[row * tradable + column] = covariance[left * count + right];
            }
        }
        let alpha: Vec<f64> = free
            .iter()
            .map(|index| {
                let value = problem.assets[*index].alpha;
                if value.is_finite() { value } else { 0.0 }
            })
            .collect();
        let mut cost = vec![0.0; count];
        for (index, asset) in problem.assets.iter().enumerate() {
            if !asset.locked {
                let mut value = fractions.trade_cost;
                if asset.foreign_currency {
                    value += fractions.fx_cost;
                }
                cost[index] = value;
            }
        }
        let free_cost: Vec<f64> = free.iter().map(|index| cost[*index]).collect();
        let cross: Vec<f64> = free
            .iter()
            .map(|index| {
                problem
                    .assets
                    .iter()
                    .enumerate()
                    .filter(|(_, asset)| asset.locked)
                    .map(|(other, asset)| covariance[index * count + other] * asset.weight)
                    .sum()
            })
            .collect();

        Self {
            problem,
            fractions,
            high_vol,
            free,
            upper,
            free_high,
            center,
            sum_min,
            sum_max,
            high_cap,
            turnover_radius,
            structural,
            covariance,
            free_covariance,
            alpha,
            cost,
            free_cost,
            cross,
        }
    }

    fn feasible_set(&self) -> solver::FeasibleSet<'_> {
        solver::FeasibleSet {
            upper: &self.upper,
            high_vol: &self.free_high,
            sum_min: self.sum_min,
            sum_max: self.sum_max,
            high_cap: self.high_cap,
            center: &self.center,
            turnover_radius: self.turnover_radius,
        }
    }

    fn policy_point(&self) -> Vec<f64> {
        self.feasible_set().project(&self.center)
    }

    fn ideas_point(&self) -> Vec<f64> {
        let lambda = self.fractions.risk_aversion;
        let lipschitz = lambda * solver::largest_eigenvalue(&self.free_covariance, self.free.len());
        let problem = solver::IdeasProblem {
            set: self.feasible_set(),
            center: &self.center,
            alpha: &self.alpha,
            covariance: &self.free_covariance,
            cross: &self.cross,
            cost: &self.free_cost,
            lambda,
        };
        problem.solve(&self.center, lipschitz)
    }

    fn has_alpha(&self) -> bool {
        self.free.iter().any(|index| {
            let alpha = self.problem.assets[*index].alpha;
            alpha.is_finite() && alpha != 0.0
        })
    }

    fn final_weights(&self, point: &[f64]) -> Vec<f64> {
        let mut weights: Vec<f64> = self
            .problem
            .assets
            .iter()
            .map(|asset| asset.weight)
            .collect();
        for (position, index) in self.free.iter().enumerate() {
            if let Some(value) = point.get(position).filter(|value| value.is_finite()) {
                weights[*index] = *value;
            }
        }
        weights
    }

    fn cash_after(&self, weights: &[f64]) -> f64 {
        1.0 - weights.iter().sum::<f64>()
    }

    fn apply_min_trade(&self, weights: &mut [f64]) {
        for index in &self.free {
            let delta = weights[*index] - self.problem.assets[*index].weight;
            let amount = (delta * self.problem.account_value).round();
            if amount == 0.0 || amount.abs() < self.fractions.min_trade {
                weights[*index] = self.problem.assets[*index].weight;
            }
        }
    }

    fn changed(&self, weights: &[f64]) -> bool {
        self.free
            .iter()
            .any(|index| (weights[*index] - self.problem.assets[*index].weight).abs() > EPSILON)
    }

    fn build_changes(&self, weights: &[f64], before: &[Breach], mode: Mode) -> Vec<Change> {
        let cash_delta = self.cash_after(weights) - self.problem.cash;
        let mut changes = Vec::new();
        for (index, asset) in self.problem.assets.iter().enumerate() {
            if asset.locked {
                continue;
            }
            let delta = weights[index] - asset.weight;
            if delta.abs() <= EPSILON {
                continue;
            }
            let amount = (delta * self.problem.account_value).round();
            if amount == 0.0 || amount.abs() < self.fractions.min_trade {
                continue;
            }
            let mut reasons = Vec::new();
            for breach in before {
                if let Some(rule) = self.reason_for(breach, index, delta, cash_delta)
                    && !reasons.contains(&rule)
                {
                    reasons.push(rule);
                }
            }
            if mode == Mode::Ideas && !reasons.contains(&Rule::Signal) {
                reasons.push(Rule::Signal);
            }
            if reasons.is_empty()
                && let Some(breach) = before.first()
            {
                reasons.push(breach.rule);
            }
            changes.push(Change {
                instrument_key: asset.instrument_key.clone(),
                symbol: asset.symbol.clone(),
                from_pct: asset.weight * 100.0,
                to_pct: weights[index] * 100.0,
                amount,
                reasons,
            });
        }
        changes.sort_by(|left, right| {
            right
                .amount
                .abs()
                .total_cmp(&left.amount.abs())
                .then_with(|| left.symbol.cmp(&right.symbol))
        });
        changes
    }

    fn reason_for(
        &self,
        breach: &Breach,
        index: usize,
        delta: f64,
        cash_delta: f64,
    ) -> Option<Rule> {
        match breach.rule {
            Rule::MaxPosition => (breach.symbol.as_deref()
                == Some(self.problem.assets[index].symbol.as_str())
                && delta < 0.0)
                .then_some(Rule::MaxPosition),
            Rule::HighVolShare => ((self.high_vol[index] && delta < 0.0)
                || (!self.high_vol[index] && delta > 0.0))
                .then_some(Rule::HighVolShare),
            Rule::CashReserve => (cash_delta > 0.0).then_some(Rule::CashReserve),
            Rule::IdleCash => (cash_delta < 0.0).then_some(Rule::IdleCash),
            Rule::Signal => None,
        }
    }

    fn metrics(&self, weights: &[f64]) -> (f64, f64, f64) {
        let count = self.problem.assets.len();
        let account = if self.problem.account_value.is_finite() {
            self.problem.account_value
        } else {
            0.0
        };
        let current: Vec<f64> = self
            .problem
            .assets
            .iter()
            .map(|asset| asset.weight)
            .collect();
        let mut gain = 0.0;
        for (index, asset) in self.problem.assets.iter().enumerate() {
            let alpha = if asset.alpha.is_finite() {
                asset.alpha
            } else {
                0.0
            };
            gain += alpha * (weights[index] - asset.weight);
        }
        let risk_before = quadratic(&self.covariance, &current, count);
        let risk_after = quadratic(&self.covariance, weights, count);
        let risk_change =
            account * (self.fractions.risk_aversion / 2.0) * (risk_after - risk_before);
        let cost: f64 = (0..count)
            .map(|index| self.cost[index] * (weights[index] - current[index]).abs())
            .sum();
        (account * gain, risk_change, account * cost)
    }
}

fn quadratic(matrix: &[f64], weights: &[f64], count: usize) -> f64 {
    let mut total = 0.0;
    for row in 0..count {
        let mut sum = 0.0;
        for column in 0..count {
            sum += matrix[row * count + column] * weights[column];
        }
        total += weights[row] * sum;
    }
    total
}

/// Covariance of `horizon`-session log returns for `series`, in the given order.
/// Instruments trade on different calendars, so it uses **weekly** log returns: the last
/// close of each ISO week, over the latest 104 weeks. Pairs need at least 26 common
/// weeks; otherwise they are 0 off the diagonal. It shrinks toward constant correlation
/// (Ledoit–Wolf), then scales by `horizon / 5`. Returns row-major `n²`.
pub fn covariance(series: &[CloseSeries<'_>], horizon_sessions: u32) -> Vec<f64> {
    let count = series.len();
    if count == 0 {
        return Vec::new();
    }
    let weekly: Vec<WeeklyReturns> = series.iter().map(weekly_returns).collect();
    let mut variances = vec![0.0; count];
    let mut covariance = vec![0.0; count * count];
    let mut correlation = vec![0.0; count * count];
    for index in 0..count {
        correlation[index * count + index] = 1.0;
        variances[index] = variance(&weekly[index].returns);
        covariance[index * count + index] = variances[index];
    }
    for left in 0..count {
        for right in (left + 1)..count {
            let pairs = paired_returns(&weekly[left], &weekly[right]);
            if pairs.len() < MIN_COMMON_WEEKS {
                continue;
            }
            let value = covariance_of(&pairs);
            covariance[left * count + right] = value;
            covariance[right * count + left] = value;
            let scale = (variances[left] * variances[right]).sqrt();
            if scale > 0.0 && scale.is_finite() {
                let correlation_value = (value / scale).clamp(-1.0, 1.0);
                correlation[left * count + right] = correlation_value;
                correlation[right * count + left] = correlation_value;
            }
        }
    }
    let average = average_correlation(&correlation, count);
    let intensity = shrink_intensity(&weekly, count, average);
    let mut shrunk = vec![0.0; count * count];
    for left in 0..count {
        for right in 0..count {
            let target = if left == right { 1.0 } else { average };
            shrunk[left * count + right] =
                (1.0 - intensity) * correlation[left * count + right] + intensity * target;
        }
    }
    let mut result = vec![0.0; count * count];
    for left in 0..count {
        for right in 0..count {
            result[left * count + right] =
                shrunk[left * count + right] * (variances[left] * variances[right]).sqrt();
        }
    }
    ensure_positive_semidefinite(&mut result, count);
    let scale = horizon_sessions as f64 / 5.0;
    for value in &mut result {
        *value *= scale;
    }
    result
}

/// Weekly log returns of one series, labelled by the ISO week of the closing price.
struct WeeklyReturns {
    returns: Vec<((i32, u32), f64)>,
}

fn weekly_returns(series: &CloseSeries<'_>) -> WeeklyReturns {
    let mut by_week: BTreeMap<(i32, u32), f64> = BTreeMap::new();
    for (date, close) in series.closes {
        let Some(value) = close.to_f64() else {
            continue;
        };
        if !value.is_finite() || value <= 0.0 {
            continue;
        }
        let week = date.iso_week();
        by_week.insert((week.year(), week.week()), value);
    }
    let latest: Vec<((i32, u32), f64)> = by_week
        .into_iter()
        .rev()
        .take(WEEKS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let mut returns = Vec::with_capacity(latest.len().saturating_sub(1));
    for pair in latest.windows(2) {
        let value = (pair[1].1 / pair[0].1).ln();
        if value.is_finite() {
            returns.push((pair[1].0, value));
        }
    }
    WeeklyReturns { returns }
}

fn variance(returns: &[((i32, u32), f64)]) -> f64 {
    if returns.len() < 2 {
        return 0.0;
    }
    let values_mean = returns.iter().map(|(_, value)| *value).sum::<f64>() / returns.len() as f64;
    returns
        .iter()
        .map(|(_, value)| (value - values_mean) * (value - values_mean))
        .sum::<f64>()
        / (returns.len() - 1) as f64
}

fn paired_returns(left: &WeeklyReturns, right: &WeeklyReturns) -> Vec<(f64, f64)> {
    let mut pairs = Vec::new();
    let mut left_index = 0;
    let mut right_index = 0;
    while left_index < left.returns.len() && right_index < right.returns.len() {
        match left.returns[left_index]
            .0
            .cmp(&right.returns[right_index].0)
        {
            std::cmp::Ordering::Less => left_index += 1,
            std::cmp::Ordering::Greater => right_index += 1,
            std::cmp::Ordering::Equal => {
                pairs.push((left.returns[left_index].1, right.returns[right_index].1));
                left_index += 1;
                right_index += 1;
            }
        }
    }
    pairs
}

fn covariance_of(pairs: &[(f64, f64)]) -> f64 {
    if pairs.len() < 2 {
        return 0.0;
    }
    let count = pairs.len() as f64;
    let left_mean = pairs.iter().map(|(left, _)| *left).sum::<f64>() / count;
    let right_mean = pairs.iter().map(|(_, right)| *right).sum::<f64>() / count;
    pairs
        .iter()
        .map(|(left, right)| (left - left_mean) * (right - right_mean))
        .sum::<f64>()
        / (count - 1.0)
}

fn average_correlation(correlation: &[f64], count: usize) -> f64 {
    if count < 2 {
        return 0.0;
    }
    let mut sum = 0.0;
    let mut pairs = 0usize;
    for left in 0..count {
        for right in (left + 1)..count {
            sum += correlation[left * count + right];
            pairs += 1;
        }
    }
    let average = sum / pairs as f64;
    let floor = -1.0 / (count as f64 - 1.0) + 1e-6;
    average.max(floor).clamp(-1.0, 1.0)
}

/// Rows of weekly returns where every instrument has one, for the shrink intensity.
fn complete_case(weekly: &[WeeklyReturns], count: usize) -> Vec<Vec<f64>> {
    let mut by_week: BTreeMap<(i32, u32), Vec<Option<f64>>> = BTreeMap::new();
    for (index, instrument) in weekly.iter().enumerate() {
        for (week, value) in &instrument.returns {
            let row = by_week.entry(*week).or_insert_with(|| vec![None; count]);
            row[index] = Some(*value);
        }
    }
    by_week
        .into_values()
        .filter(|row| row.iter().all(Option::is_some))
        .map(|row| row.into_iter().map(|value| value.unwrap_or(0.0)).collect())
        .collect()
}

/// Ledoit–Wolf shrinkage intensity toward constant correlation, from the complete rows.
fn shrink_intensity(weekly: &[WeeklyReturns], count: usize, average: f64) -> f64 {
    if count < 2 {
        return 0.0;
    }
    let rows = complete_case(weekly, count);
    let sessions = rows.len();
    if sessions < 2 {
        return 0.0;
    }
    let sessions_f = sessions as f64;
    let means: Vec<f64> = (0..count)
        .map(|column| rows.iter().map(|row| row[column]).sum::<f64>() / sessions_f)
        .collect();
    let mut sample = vec![0.0; count * count];
    for left in 0..count {
        for right in left..count {
            let mut sum = 0.0;
            for row in &rows {
                sum += (row[left] - means[left]) * (row[right] - means[right]);
            }
            let value = sum / (sessions_f - 1.0);
            sample[left * count + right] = value;
            sample[right * count + left] = value;
        }
    }
    let mut pi = 0.0;
    let mut theta = vec![0.0; count];
    for left in 0..count {
        for right in 0..count {
            let target = sample[left * count + right];
            let mut sum = 0.0;
            for row in &rows {
                let product = (row[left] - means[left]) * (row[right] - means[right]);
                sum += (product - target) * (product - target);
            }
            let term = sum / sessions_f;
            pi += term;
            if left == right {
                theta[left] = term.max(0.0);
            }
        }
    }
    let mut rho = 0.0;
    for left in 0..count {
        rho += theta[left];
        for right in 0..count {
            if left == right {
                continue;
            }
            let variance_left = sample[left * count + left];
            let variance_right = sample[right * count + right];
            if variance_left <= 0.0 || variance_right <= 0.0 {
                continue;
            }
            rho += 0.5
                * average
                * ((variance_right / variance_left).sqrt() * theta[left]
                    + (variance_left / variance_right).sqrt() * theta[right]);
        }
    }
    let mut gamma = 0.0;
    for left in 0..count {
        for right in 0..count {
            let target = if left == right {
                sample[left * count + left]
            } else {
                average * (sample[left * count + left] * sample[right * count + right]).sqrt()
            };
            let difference = target - sample[left * count + right];
            gamma += difference * difference;
        }
    }
    if !gamma.is_finite() || gamma <= 0.0 {
        return 0.0;
    }
    ((pi - rho) / gamma / sessions_f).clamp(0.0, 1.0)
}

/// Shifts the diagonal just enough for every Gershgorin disc to stay non-negative, which
/// makes the matrix positive semidefinite.
fn ensure_positive_semidefinite(matrix: &mut [f64], count: usize) {
    let mut lower = f64::INFINITY;
    for row in 0..count {
        let mut off_diagonal = 0.0;
        for column in 0..count {
            if column != row {
                off_diagonal += matrix[row * count + column].abs();
            }
        }
        lower = lower.min(matrix[row * count + row] - off_diagonal);
    }
    if !lower.is_finite() || lower >= 0.0 {
        return;
    }
    let total: f64 = matrix.iter().map(|value| value.abs()).sum();
    let ridge = -lower + 1e-12 * total.max(1.0);
    for index in 0..count {
        matrix[index * count + index] += ridge;
    }
}
