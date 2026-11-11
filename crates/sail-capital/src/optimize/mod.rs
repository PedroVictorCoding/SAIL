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
use crate::forecast::CloseSeries;
use crate::policy::RiskLimits;
use serde::{Deserialize, Serialize};

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

/// The limits the portfolio breaks now.
pub fn breaches(problem: &Problem, limits: &RiskLimits) -> Vec<Breach> {
    // Kilo implements this; the stub keeps the crate building.
    let _ = (problem, limits);
    Vec::new()
}

/// Solves `problem` in `mode` under `limits`.
pub fn plan(problem: &Problem, limits: &RiskLimits, mode: Mode) -> Plan {
    // Kilo implements this; the stub keeps the crate building.
    let _ = limits;
    Plan {
        mode,
        changes: Vec::new(),
        cash_from_pct: problem.cash * 100.0,
        cash_to_pct: problem.cash * 100.0,
        breaches_before: Vec::new(),
        breaches_after: Vec::new(),
        feasible: true,
        expected_gain: 0.0,
        risk_change: 0.0,
        cost: 0.0,
        net_benefit: 0.0,
        note: "not implemented".into(),
    }
}

/// Covariance of `horizon`-session log returns for `series`, in the given order.
/// Instruments trade on different calendars, so it uses **weekly** log returns: the last
/// close of each ISO week, over the latest 104 weeks. Pairs need at least 26 common
/// weeks; otherwise they are 0 off the diagonal. It shrinks toward constant correlation
/// (Ledoit–Wolf), then scales by `horizon / 5`. Returns row-major `n²`.
pub fn covariance(series: &[CloseSeries<'_>], horizon_sessions: u32) -> Vec<f64> {
    // Kilo implements this; the stub keeps the crate building.
    let n = series.len();
    let _ = horizon_sessions;
    vec![0.0; n * n]
}
