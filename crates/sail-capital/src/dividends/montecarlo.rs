//! Round 24: dividend income as a distribution, not three scenarios (B4, Claude).
//!
//! The deterministic projection ([`super::project`]) gives low, base and high lines that
//! are scenarios, not probabilities. This module runs many seeded paths month by month
//! and reports percentiles of net income per year, of the portfolio value, and the chance
//! that income covers a target. Every path uses the same rules as the projection
//! (withholding, the optional Portuguese reserve, reinvestment and the monthly allowance,
//! the high-yield rule) plus four sources of uncertainty:
//!
//! - **Prices:** monthly log returns with each holding's measured trend (as the
//!   projection uses it) and its own volatility. Shocks share one market factor with the
//!   average pairwise correlation `ρ̄` (the constant-correlation model the optimizer
//!   shrinks toward): `ε_i = √ρ̄·m + √(1 − ρ̄)·e_i`. The drift is corrected by `−σ²/2`, so
//!   the median path follows the trend.
//! - **Dividend growth:** at each 12-month step, the payout per share grows by
//!   `(1 + g)·exp(σ·z − σ²/2)` with the holding's own `g` and `σ` (the estimator's).
//! - **Cuts:** each year a paying holding halves its payout with the chance
//!   [`super::calibrate::cut_hazard`] gives for its yield at that time (measured rates
//!   blended with an a-priori table). Growth carries on from the lower level.
//! - **FX:** a driftless monthly random walk at the pair's volatility for holdings outside
//!   the account currency.
//!
//! High-yield holdings (trailing yield above [`super::HIGH_YIELD`]) pay a constant share
//! of their price, as in the projection, and are not cut separately (their price carries
//! the risk). A payout never exceeds `max(today's yield, 12%)` of the price.
//!
//! **Allocation.** The month's pool (reinvested net dividends plus the allowance) goes by
//! a [`Policy`]: one of the projection's [`super::Allocation`] modes, or fixed weights
//! (the maximizer's). Names that would break a [`Limits`] rule this month (largest
//! position, high-yield share, share outside the account currency) receive nothing and
//! their share goes to the others; with no eligible name the pool stays as cash. Cash
//! earns nothing.
//!
//! Pure and seeded: the same inputs and seed give the same numbers. Paths use a
//! SplitMix64 generator with Box–Muller normals, and path `p` is seeded from
//! `seed ^ p`, so two policies scored with one seed see the same shocks (common random
//! numbers).

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

use super::calibrate::{CalibrationReport, cut_hazard};
use super::safety::SafetyGrade;
use super::{Allocation, Growth, HIGH_YIELD, HIGH_YIELD_TOTAL_RETURN, MAX_YEARS, Payment};

/// Most paths in one run.
pub const MAX_PATHS: u32 = 20_000;

/// Default paths for a distribution.
pub const DEFAULT_PATHS: u32 = 2_000;

/// Price volatility assumed when a holding has too little price history (annual).
pub const DEFAULT_PRICE_VOL: f64 = 0.25;

/// One holding or candidate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McHolding {
    /// Ledger key, or `watch:<SYMBOL>` for a candidate.
    pub key: String,
    /// Symbol.
    pub symbol: String,
    /// Units held now (0 for a candidate).
    pub units: f64,
    /// Latest price in its trading currency.
    pub price: f64,
    /// Account currency per unit of the trading currency.
    pub fx: f64,
    /// Per-share payments.
    pub history: Vec<Payment>,
    /// Withholding on gross payments, 0..1.
    pub withholding: f64,
    /// Portuguese rate on the gross payment before crediting withholding, for the
    /// optional reserve (round 25 B7, [`super::tax::pt_rate`]).
    #[serde(default = "super::default_pt_rate")]
    pub pt_rate: f64,
    /// Annual price trend (shrunk and capped as in the projection), or `None` for the
    /// payout growth.
    pub price_growth: Option<f64>,
    /// Annual price volatility.
    pub price_vol: f64,
    /// Annual volatility of the account currency per unit of this one (0 when the same).
    pub fx_vol: f64,
    /// Trades in a currency other than the account's.
    pub foreign: bool,
    /// Not held: only receives money from fixed weights.
    pub candidate: bool,
    /// A fund (no company filings, so safety is unknown by nature).
    pub fund: bool,
    /// Dividend safety.
    pub safety: SafetyGrade,
}

/// Everything a run needs besides the rules.
#[derive(Clone, Debug, PartialEq)]
pub struct McInputs {
    /// Holdings, then candidates.
    pub holdings: Vec<McHolding>,
    /// Average pairwise correlation of monthly returns, clamped to `[0, 0.95]`.
    pub avg_correlation: f64,
    /// Measured cut rates, when the calibration has run.
    pub calibration: Option<CalibrationReport>,
    /// The growth shrinkage in use.
    pub shrink_k: f64,
    /// First projected month.
    pub start: NaiveDate,
}

/// Limits applied every month to where new money goes. Percent values.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    /// A name at or above this share of the portfolio receives nothing.
    pub max_position_pct: f64,
    /// High-yield names together at or above this share receive nothing.
    pub max_high_yield_pct: f64,
    /// Names outside the account currency together at or above this share receive
    /// nothing.
    pub max_foreign_pct: f64,
}

impl Default for Limits {
    /// No limits.
    fn default() -> Self {
        Limits {
            max_position_pct: 100.0,
            max_high_yield_pct: 100.0,
            max_foreign_pct: 100.0,
        }
    }
}

/// Stress switches for sensitivity runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stress {
    /// Every holding's dividend growth lowered by its `σ`.
    pub low_growth: bool,
    /// No cuts.
    pub no_cuts: bool,
    /// FX held at today's rates.
    pub fx_flat: bool,
}

/// The owner's rules for a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rules {
    /// Years simulated, 1..=[`MAX_YEARS`].
    pub years: u32,
    /// Share of each net payment reinvested, 0..=100.
    pub reinvest_pct: f64,
    /// Added from salary every month.
    pub monthly_contribution: f64,
    /// Set aside Portuguese tax before reinvesting.
    pub reserve_pt_tax: bool,
    /// Limits on new money.
    pub limits: Limits,
    /// Stress switches.
    #[serde(default)]
    pub stress: Stress,
}

/// Where the month's pool goes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Policy {
    /// One of the projection's modes over the held paying names (candidates excluded).
    Mode {
        /// The mode.
        allocation: Allocation,
        /// Names for the top-N modes.
        top_n: u32,
    },
    /// Fixed weights per holding index (same order as [`McInputs::holdings`]); negative or
    /// non-finite weights count as 0, and the rest are normalized.
    Weights {
        /// The weights.
        weights: Vec<f64>,
    },
}

/// Seed and size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McSettings {
    /// Paths, 1..=[`MAX_PATHS`].
    pub paths: u32,
    /// Seed.
    pub seed: u64,
}

/// Raw outcomes per path, for objectives.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcomes {
    /// Net income per path and year: `[path][year − 1]`.
    pub yearly_net: Vec<Vec<f64>>,
    /// The lowest month's net income per path and year.
    pub lowest_month: Vec<Vec<f64>>,
    /// Portfolio value (holdings plus cash) per path at each year end.
    pub value: Vec<Vec<f64>>,
}

/// The 10th, 50th and 90th percentiles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Quantiles {
    /// 10th percentile.
    pub p10: f64,
    /// Median.
    pub p50: f64,
    /// 90th percentile.
    pub p90: f64,
}

/// One year of the distribution.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct YearDistribution {
    /// 1, 2, …
    pub year: u32,
    /// Net income.
    pub net: Quantiles,
    /// Portfolio value at year end.
    pub value: Quantiles,
}

/// The distribution's result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IncomeDistribution {
    /// Paths run.
    pub paths: u32,
    /// Seed used.
    pub seed: u64,
    /// One row per year.
    pub years: Vec<YearDistribution>,
    /// The target, when asked: `(year, annual net income)`.
    pub target: Option<(u32, f64)>,
    /// Share of paths whose net income in the target year reaches the target.
    pub p_target: Option<f64>,
    /// Plain notes on the method.
    pub notes: Vec<String>,
}

/// SplitMix64 with Box–Muller normals.
struct Rng {
    state: u64,
    spare: Option<f64>,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Rng {
            state: seed,
            spare: None,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `(0, 1)`.
    fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    fn normal(&mut self) -> f64 {
        if let Some(value) = self.spare.take() {
            return value;
        }
        let (u, v) = (self.uniform(), self.uniform());
        let radius = (-2.0 * u.ln()).sqrt();
        let angle = std::f64::consts::TAU * v;
        self.spare = Some(radius * angle.sin());
        radius * angle.cos()
    }
}

/// Per-holding constants for a run.
struct Prepared {
    growth: Growth,
    g: f64,
    sigma: f64,
    high_yield: bool,
    paying: bool,
    yield_cap: f64,
    monthly_drift: f64,
    monthly_vol: f64,
    fx_monthly_vol: f64,
}

fn prepare(inputs: &McInputs, stress: Stress) -> Vec<Prepared> {
    inputs
        .holdings
        .iter()
        .map(|h| {
            let growth = super::growth_with(&h.history, inputs.start, inputs.shrink_k);
            let paying = growth.ttm_per_share > 0.0 && !growth.months.is_empty() && h.price > 0.0;
            let ttm_yield = if h.price > 0.0 {
                growth.ttm_per_share / h.price
            } else {
                0.0
            };
            let high_yield = paying && ttm_yield > HIGH_YIELD;
            let g = if stress.low_growth {
                growth.g - growth.sigma
            } else {
                growth.g
            };
            let mut trend = h.price_growth.unwrap_or(g);
            if high_yield {
                trend = trend.min(-(ttm_yield - HIGH_YIELD_TOTAL_RETURN)).max(-0.9);
            }
            let vol = if h.price_vol.is_finite() && h.price_vol > 0.0 {
                h.price_vol.min(2.0)
            } else {
                DEFAULT_PRICE_VOL
            };
            let monthly_vol = vol / 12f64.sqrt();
            Prepared {
                g,
                sigma: growth.sigma,
                high_yield,
                paying,
                yield_cap: ttm_yield.max(HIGH_YIELD),
                monthly_drift: (1.0 + trend).max(0.01).ln() / 12.0
                    - monthly_vol * monthly_vol / 2.0,
                monthly_vol,
                fx_monthly_vol: if stress.fx_flat || !h.foreign {
                    0.0
                } else {
                    h.fx_vol.clamp(0.0, 1.0) / 12f64.sqrt()
                },
                growth,
            }
        })
        .collect()
}

/// Pure, seeded: raw outcomes of `policy` under `rules` (module doc).
pub fn run(inputs: &McInputs, rules: &Rules, policy: &Policy, mc: McSettings) -> Outcomes {
    let years = rules.years.clamp(1, MAX_YEARS) as usize;
    let paths = mc.paths.clamp(1, MAX_PATHS) as usize;
    let prepared = prepare(inputs, rules.stress);
    let n = inputs.holdings.len();
    let rho = inputs.avg_correlation.clamp(0.0, 0.95);
    let (market_load, own_load) = (rho.sqrt(), (1.0 - rho).sqrt());
    let pct = rules.reinvest_pct.clamp(0.0, 100.0) / 100.0;
    let contribution = rules
        .monthly_contribution
        .clamp(0.0, super::MAX_MONTHLY_CONTRIBUTION);
    let start_month = inputs.start.month0();
    let fixed_weights: Option<Vec<f64>> = match policy {
        Policy::Weights { weights } => Some(
            (0..n)
                .map(|i| {
                    weights
                        .get(i)
                        .copied()
                        .filter(|w| w.is_finite() && *w > 0.0)
                        .unwrap_or(0.0)
                })
                .collect(),
        ),
        Policy::Mode { .. } => None,
    };

    let mut out = Outcomes {
        yearly_net: Vec::with_capacity(paths),
        lowest_month: Vec::with_capacity(paths),
        value: Vec::with_capacity(paths),
    };
    let mut units = vec![0.0f64; n];
    let mut log_price = vec![0.0f64; n];
    let mut log_fx = vec![0.0f64; n];
    let mut dps = vec![0.0f64; n];
    let mut prices = vec![0.0f64; n];
    let mut fxs = vec![0.0f64; n];
    let mut paid = vec![0.0; n];
    let mut amounts = vec![0.0; n];

    for path in 0..paths {
        let mut rng = Rng::new(mc.seed ^ (path as u64).wrapping_mul(0xD1B5_4A32_D192_ED03));
        for (i, h) in inputs.holdings.iter().enumerate() {
            units[i] = h.units.max(0.0);
            log_price[i] = 0.0;
            log_fx[i] = 0.0;
            dps[i] = prepared[i].growth.ttm_per_share;
        }
        let mut cash = 0.0;
        let mut yearly = Vec::with_capacity(years);
        let mut lowest = Vec::with_capacity(years);
        let mut values = Vec::with_capacity(years);
        let mut year_net = 0.0;
        let mut year_lowest = f64::INFINITY;

        for month in 0..years * 12 {
            // Yearly dividend growth and cuts, from the second year on.
            if month > 0 && month % 12 == 0 {
                for (i, p) in prepared.iter().enumerate() {
                    if !p.paying || p.high_yield {
                        continue;
                    }
                    let z = rng.normal();
                    dps[i] *= (1.0 + p.g).max(0.0) * (p.sigma * z - p.sigma * p.sigma / 2.0).exp();
                    let price_now = inputs.holdings[i].price * log_price[i].exp();
                    let trailing = if price_now > 0.0 {
                        dps[i] / price_now
                    } else {
                        0.0
                    };
                    let hazard = if rules.stress.no_cuts {
                        0.0
                    } else {
                        cut_hazard(inputs.calibration.as_ref(), trailing)
                    };
                    if rng.uniform() < hazard {
                        dps[i] *= 0.5;
                    }
                }
            }
            let market = rng.normal();
            for (i, p) in prepared.iter().enumerate() {
                let shock = market_load * market + own_load * rng.normal();
                log_price[i] += p.monthly_drift + p.monthly_vol * shock;
                if p.fx_monthly_vol > 0.0 {
                    log_fx[i] +=
                        p.fx_monthly_vol * rng.normal() - p.fx_monthly_vol * p.fx_monthly_vol / 2.0;
                }
                prices[i] = inputs.holdings[i].price * log_price[i].exp();
                fxs[i] = inputs.holdings[i].fx * log_fx[i].exp();
            }

            let month_of_year = (start_month + month as u32) % 12 + 1;
            let mut month_net = 0.0;
            let mut pool = 0.0;
            for (i, p) in prepared.iter().enumerate() {
                paid[i] = 0.0;
                if !p.paying || units[i] <= 0.0 || !p.growth.months.contains(&month_of_year) {
                    continue;
                }
                let h = &inputs.holdings[i];
                let payments = p.growth.months.len() as f64;
                let per_share = if p.high_yield {
                    p.growth.ttm_per_share / payments * prices[i] / h.price
                } else {
                    (dps[i] / payments).min(prices[i] * p.yield_cap / payments)
                };
                let gross = units[i] * per_share * fxs[i];
                let withheld = gross * h.withholding.clamp(0.0, 1.0);
                let pt = if rules.reserve_pt_tax {
                    super::tax::pt_reserve(gross, withheld, h.pt_rate)
                } else {
                    0.0
                };
                let net = gross - withheld - pt;
                month_net += net;
                paid[i] = net * pct;
                pool += net * pct;
                cash += net * (1.0 - pct);
            }
            let any_paying = prepared
                .iter()
                .zip(&units)
                .any(|(p, u)| p.paying && *u > 0.0);
            if any_paying || fixed_weights.is_some() {
                pool += contribution;
            }
            if pool > 0.0 {
                allocate(
                    inputs,
                    &prepared,
                    &units,
                    &prices,
                    &fxs,
                    &paid,
                    pool,
                    policy,
                    fixed_weights.as_deref(),
                    rules.limits,
                    &mut amounts,
                );
                let mut spent = 0.0;
                for i in 0..n {
                    let unit_price = prices[i] * fxs[i];
                    if amounts[i] > 0.0 && unit_price > 0.0 {
                        units[i] += amounts[i] / unit_price;
                        spent += amounts[i];
                    }
                }
                cash += (pool - spent).max(0.0);
            }
            year_net += month_net;
            year_lowest = year_lowest.min(month_net);
            if (month + 1) % 12 == 0 {
                let value: f64 = (0..n).map(|i| units[i] * prices[i] * fxs[i]).sum::<f64>() + cash;
                yearly.push(year_net);
                lowest.push(if year_lowest.is_finite() {
                    year_lowest
                } else {
                    0.0
                });
                values.push(value);
                year_net = 0.0;
                year_lowest = f64::INFINITY;
            }
        }
        out.yearly_net.push(yearly);
        out.lowest_month.push(lowest);
        out.value.push(values);
    }
    out
}

/// Splits `pool` into `amounts` by the policy and this month's limits.
#[allow(clippy::too_many_arguments)]
fn allocate(
    inputs: &McInputs,
    prepared: &[Prepared],
    units: &[f64],
    prices: &[f64],
    fxs: &[f64],
    paid: &[f64],
    pool: f64,
    policy: &Policy,
    fixed: Option<&[f64]>,
    limits: Limits,
    amounts: &mut [f64],
) {
    let n = units.len();
    for a in amounts.iter_mut() {
        *a = 0.0;
    }
    let values: Vec<f64> = (0..n).map(|i| units[i] * prices[i] * fxs[i]).collect();
    let total: f64 = values.iter().sum();
    let share = |sum: f64| {
        if total > 0.0 {
            sum / total * 100.0
        } else {
            0.0
        }
    };
    let high_yield_share = share(
        (0..n)
            .filter(|i| prepared[*i].high_yield)
            .map(|i| values[i])
            .sum(),
    );
    let foreign_share = share(
        (0..n)
            .filter(|i| inputs.holdings[*i].foreign)
            .map(|i| values[i])
            .sum(),
    );
    let blocked = |i: usize| {
        (total > 0.0 && share(values[i]) >= limits.max_position_pct)
            || (prepared[i].high_yield && high_yield_share >= limits.max_high_yield_pct)
            || (inputs.holdings[i].foreign && foreign_share >= limits.max_foreign_pct)
    };
    // Raw weights per name.
    let mut weights = vec![0.0; n];
    match (policy, fixed) {
        (_, Some(fixed)) => weights.copy_from_slice(fixed),
        (Policy::Mode { allocation, top_n }, None) => {
            let held: Vec<usize> = (0..n)
                .filter(|i| {
                    prepared[*i].paying && !inputs.holdings[*i].candidate && units[*i] > 0.0
                })
                .collect();
            if held.is_empty() {
                return;
            }
            let top_n = (*top_n).clamp(1, super::MAX_TOP_N) as usize;
            match allocation {
                Allocation::Proportional => {
                    for &i in &held {
                        weights[i] = values[i];
                    }
                }
                Allocation::SameHolding => {
                    // Dividends stay with their payer; the rest goes by value.
                    let dividends: f64 = paid.iter().sum();
                    let rest = (pool - dividends).max(0.0);
                    let held_value: f64 = held.iter().map(|i| values[*i]).sum();
                    for &i in &held {
                        let by_value = if held_value > 0.0 {
                            rest * values[i] / held_value
                        } else {
                            rest / held.len() as f64
                        };
                        weights[i] = (paid[i] + by_value) / pool;
                    }
                }
                Allocation::EqualWeight => {
                    for &i in &held {
                        weights[i] = 1.0;
                    }
                }
                Allocation::TopValue | Allocation::TopYield => {
                    let mut ranked: Vec<(usize, f64)> = held
                        .iter()
                        .map(|&i| {
                            let key = if *allocation == Allocation::TopValue {
                                values[i]
                            } else if prices[i] > 0.0 {
                                prepared[i].growth.ttm_per_share / prices[i]
                            } else {
                                0.0
                            };
                            (i, key)
                        })
                        .collect();
                    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
                    ranked.truncate(top_n);
                    for (i, _) in ranked {
                        weights[i] = values[i].max(f64::MIN_POSITIVE);
                    }
                }
                Allocation::Underweight => {
                    let held_value: f64 = held.iter().map(|i| values[*i]).sum();
                    let target = held_value / held.len() as f64;
                    let mut any = false;
                    for &i in &held {
                        weights[i] = (target - values[i]).max(0.0);
                        any |= weights[i] > 0.0;
                    }
                    if !any {
                        for &i in &held {
                            weights[i] = 1.0;
                        }
                    }
                }
            }
        }
        (Policy::Weights { .. }, None) => {}
    }
    for (i, w) in weights.iter_mut().enumerate() {
        if blocked(i) || !(w.is_finite() && *w > 0.0) {
            *w = 0.0;
        }
    }
    let sum: f64 = weights.iter().sum();
    if sum <= 0.0 {
        return;
    }
    for i in 0..n {
        amounts[i] = pool * weights[i] / sum;
    }
}

/// The value at percentile `q` (0..1) of `values` (nearest rank on the sorted values).
pub fn percentile(values: &[f64], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return 0.0;
    }
    sorted.sort_by(f64::total_cmp);
    let rank = (q.clamp(0.0, 1.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn quantiles(values: &[f64]) -> Quantiles {
    Quantiles {
        p10: percentile(values, 0.10),
        p50: percentile(values, 0.50),
        p90: percentile(values, 0.90),
    }
}

/// Pure, seeded: the distribution of `policy` under `rules`, with the chance of reaching
/// `target` = `(year, annual net income)` when given.
pub fn simulate(
    inputs: &McInputs,
    rules: &Rules,
    policy: &Policy,
    mc: McSettings,
    target: Option<(u32, f64)>,
) -> IncomeDistribution {
    let outcomes = run(inputs, rules, policy, mc);
    let years = rules.years.clamp(1, MAX_YEARS) as usize;
    let rows = (0..years)
        .map(|y| {
            let net: Vec<f64> = outcomes.yearly_net.iter().map(|p| p[y]).collect();
            let value: Vec<f64> = outcomes.value.iter().map(|p| p[y]).collect();
            YearDistribution {
                year: y as u32 + 1,
                net: quantiles(&net),
                value: quantiles(&value),
            }
        })
        .collect();
    let target = target.filter(|(year, amount)| {
        *year >= 1 && (*year as usize) <= years && amount.is_finite() && *amount > 0.0
    });
    let p_target = target.map(|(year, amount)| {
        let hits = outcomes
            .yearly_net
            .iter()
            .filter(|p| p[year as usize - 1] >= amount)
            .count();
        hits as f64 / outcomes.yearly_net.len().max(1) as f64
    });
    let mut notes = vec![
        format!(
            "{} simulated paths: prices share one market factor (average correlation {:.2}), dividends grow with each holding's own estimate and spread, and cuts follow measured rates by yield.",
            mc.paths.clamp(1, MAX_PATHS),
            inputs.avg_correlation.clamp(0.0, 0.95)
        ),
        "The 10th and 90th percentiles are the model's range, not a guarantee: it does not know about recessions longer than its price history or about changes in tax law.".to_string(),
    ];
    if inputs.calibration.is_none() {
        notes.push(
            "Cut rates are the a-priori table; the dividend backtest has not run yet.".to_string(),
        );
    }
    IncomeDistribution {
        paths: mc.paths.clamp(1, MAX_PATHS),
        seed: mc.seed,
        years: rows,
        target,
        p_target,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normals_have_unit_spread() {
        let mut rng = Rng::new(7);
        let draws: Vec<f64> = (0..20_000).map(|_| rng.normal()).collect();
        let mean = draws.iter().sum::<f64>() / draws.len() as f64;
        let var = draws.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / draws.len() as f64;
        assert!(mean.abs() < 0.03, "{mean}");
        assert!((var - 1.0).abs() < 0.05, "{var}");
    }
}
