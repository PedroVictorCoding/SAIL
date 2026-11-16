//! Round 24: the dividend income maximizer (B5/B6, Claude).
//!
//! **The decision.** Fixed weights for where each month's pool (reinvested dividends and
//! the salary allowance) goes, over the paying holdings and any dividend candidates the
//! owner marked. SAIL never places orders; the plan reads "this month, buy about X of A".
//!
//! **Objectives** (fixed a priori, the owner picks one), each scored on the Monte Carlo
//! ([`super::montecarlo`]):
//! - [`Objective::RobustIncome`]: the 10th percentile of net income in `year` (a bad
//!   but not extreme outcome), the default at the retirement year;
//! - [`Objective::IncomeWithWealthFloor`]: the median net income in `year`, while the
//!   median portfolio value that year stays at least `floor_pct`% of what the
//!   proportional mode gives (otherwise the plan scores −∞);
//! - [`Objective::SmoothMonths`]: the 10th percentile of the lowest month's net income in
//!   `year` (fills the empty months).
//!
//! **Constraints.** New money never goes to a name at or above the largest-position limit,
//! to high-yield names once they hold [`Constraints::max_high_yield_pct`] together, or
//! outside the account currency once that holds [`Constraints::max_foreign_pct`]. Names
//! below [`Constraints::min_safety`] get nothing (Unknown is allowed only for funds). At
//! most [`Constraints::top_n`] names receive money.
//!
//! **Search.** Projected coordinate ascent on the simplex: from three starts (current
//! value, equal, highest yield), repeatedly move a step of weight toward or away from one
//! name (taking it from or giving it to the others in proportion), keeping the move that
//! improves the objective most, with steps 0.2, 0.1, 0.05 and 0.02. Every candidate is
//! scored on the same `search` paths (common random numbers).
//!
//! **Honesty.** The winner is re-scored on fresh `score` paths (a different seed), beside
//! the six fixed modes. It is recommended only if it beats the best mode by more than two
//! standard errors of the paired difference (20 batches of the score paths). Otherwise
//! the plan says no allocation beat that mode and recommends the mode. Three stress runs
//! (low growth, no cuts, flat FX) show how much the answer leans on the estimates.
//!
//! Pure and seeded.

use serde::{Deserialize, Serialize};

use super::montecarlo::{
    self, Limits, McInputs, McSettings, Outcomes, Policy, Rules, Stress, percentile,
};
use super::safety::SafetyGrade;
use super::{Allocation, DEFAULT_TOP_N, HIGH_YIELD};

/// Batches for the standard error of a difference.
const BATCHES: usize = 20;

/// Steps of weight tried, largest first.
const STEPS: [f64; 4] = [0.2, 0.1, 0.05, 0.02];

/// Most improving moves per step size.
const MAX_MOVES_PER_STEP: usize = 25;

/// The six fixed modes, in the projection's order.
pub const MODES: [Allocation; 6] = [
    Allocation::Proportional,
    Allocation::SameHolding,
    Allocation::TopValue,
    Allocation::EqualWeight,
    Allocation::TopYield,
    Allocation::Underweight,
];

/// What to maximize. JSON: `{"kind": "robust_income", "year": N}`,
/// `{"kind": "income_with_wealth_floor", "year": N, "floor_pct": P}` or
/// `{"kind": "smooth_months", "year": N}`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Objective {
    /// 10th percentile of net income in `year`.
    RobustIncome {
        /// Projection year, 1..=30.
        year: u32,
    },
    /// Median net income in `year` with the median value at least `floor_pct`% of the
    /// proportional mode's.
    IncomeWithWealthFloor {
        /// Projection year.
        year: u32,
        /// Percent, 0..=100.
        floor_pct: f64,
    },
    /// 10th percentile of the lowest month's net income in `year`.
    SmoothMonths {
        /// Projection year.
        year: u32,
    },
}

/// The wealth floor the "all objectives" run uses: the plan's median portfolio value
/// must stay at least 90% of the proportional mode's (fixed a priori: income may cost
/// at most a tenth of the wealth the default rule keeps).
pub const DEFAULT_FLOOR_PCT: f64 = 90.0;

impl Objective {
    /// Round 25: the three objectives, in the order the "all objectives" run scores
    /// them, for `year` (the wealth floor at [`DEFAULT_FLOOR_PCT`] unless given).
    pub fn all(year: u32, floor_pct: Option<f64>) -> [Objective; 3] {
        [
            Objective::RobustIncome { year },
            Objective::IncomeWithWealthFloor {
                year,
                floor_pct: floor_pct.unwrap_or(DEFAULT_FLOOR_PCT),
            },
            Objective::SmoothMonths { year },
        ]
    }

    /// A short name for the owner.
    pub fn label(&self) -> &'static str {
        match self {
            Objective::RobustIncome { .. } => "Most income in a bad year",
            Objective::IncomeWithWealthFloor { .. } => "Most typical income, keeping wealth",
            Objective::SmoothMonths { .. } => "Steadiest monthly income",
        }
    }

    /// One plain sentence on what it maximizes.
    pub fn explainer(&self) -> String {
        match *self {
            Objective::RobustIncome { year } => format!(
                "Maximizes the net income you'd still get in year {year} if things go badly \
                 (the 10th percentile across simulated futures)."
            ),
            Objective::IncomeWithWealthFloor { year, floor_pct } => format!(
                "Maximizes the typical (median) net income in year {year}, as long as the \
                 typical portfolio value stays at least {floor_pct:.0}% of what the \
                 proportional rule would leave."
            ),
            Objective::SmoothMonths { year } => format!(
                "Maximizes the weakest month's net income in year {year} in a bad case (the \
                 10th percentile), so income arrives evenly through the year."
            ),
        }
    }

    /// The projection year it scores.
    pub fn year(&self) -> u32 {
        match *self {
            Objective::RobustIncome { year }
            | Objective::IncomeWithWealthFloor { year, .. }
            | Objective::SmoothMonths { year } => year,
        }
    }

    fn with_year(self, year: u32) -> Self {
        match self {
            Objective::RobustIncome { .. } => Objective::RobustIncome { year },
            Objective::IncomeWithWealthFloor { floor_pct, .. } => {
                Objective::IncomeWithWealthFloor { year, floor_pct }
            }
            Objective::SmoothMonths { .. } => Objective::SmoothMonths { year },
        }
    }
}

/// Limits on where money goes. Percent values.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Constraints {
    /// Largest position (from the confirmed limits).
    pub max_position_pct: f64,
    /// Largest combined share of high-yield names.
    pub max_high_yield_pct: f64,
    /// Largest share outside the account currency.
    pub max_foreign_pct: f64,
    /// Lowest safety grade that may receive money.
    pub min_safety: SafetyGrade,
    /// Most names receiving money.
    pub top_n: u32,
}

impl Default for Constraints {
    /// 10% largest position, 20% high yield, 60% foreign, grade C, five names.
    fn default() -> Self {
        Constraints {
            max_position_pct: 10.0,
            max_high_yield_pct: 20.0,
            max_foreign_pct: 60.0,
            min_safety: SafetyGrade::C,
            top_n: DEFAULT_TOP_N,
        }
    }
}

impl Constraints {
    fn limits(&self) -> Limits {
        Limits {
            max_position_pct: self.max_position_pct.clamp(0.0, 100.0),
            max_high_yield_pct: self.max_high_yield_pct.clamp(0.0, 100.0),
            max_foreign_pct: self.max_foreign_pct.clamp(0.0, 100.0),
        }
    }
}

/// One name's weight in the plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanWeight {
    /// Key.
    pub key: String,
    /// Symbol.
    pub symbol: String,
    /// Share of each month's pool, 0..1.
    pub weight: f64,
    /// Not held yet.
    pub candidate: bool,
}

/// One buy for this month.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanBuy {
    /// Key.
    pub key: String,
    /// Symbol.
    pub symbol: String,
    /// Amount of the monthly allowance, in the account currency.
    pub amount: f64,
    /// Not held yet ("consider buying").
    pub candidate: bool,
}

/// How a policy scored on the score paths.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolicyScore {
    /// `"plan"` or the mode's snake-case name.
    pub policy: String,
    /// The objective's value (−∞ is reported as `None`).
    pub objective: Option<f64>,
    /// 10th percentile of net income in the objective's year.
    pub p10: f64,
    /// Median net income in that year.
    pub p50: f64,
    /// Median portfolio value at the end of that year.
    pub value_p50: f64,
}

/// The plan's objective under each stress.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sensitivity {
    /// Every dividend growing one σ slower.
    pub low_growth: Option<f64>,
    /// No cuts.
    pub no_cuts: Option<f64>,
    /// FX held at today's rates.
    pub fx_flat: Option<f64>,
}

/// What the owner should do. JSON: `{"kind": "plan"}` or `{"kind": "mode", "mode": …}`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Recommendation {
    /// Follow the plan's weights.
    Plan,
    /// Keep (or switch to) this fixed mode: no search beat it clearly.
    Mode {
        /// The mode.
        mode: Allocation,
    },
}

/// The maximizer's answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IncomePlan {
    /// The objective, with its year clamped to the projection.
    pub objective: Objective,
    /// The constraints used.
    pub constraints: Constraints,
    /// The weights found, largest first (zero weights left out).
    pub weights: Vec<PlanWeight>,
    /// The monthly allowance split by the weights.
    pub this_month: Vec<PlanBuy>,
    /// The plan and the six modes on the score paths, plan first.
    pub scores: Vec<PolicyScore>,
    /// The best fixed mode.
    pub best_mode: Allocation,
    /// Plan minus best mode, mean over batches.
    pub margin: Option<f64>,
    /// Its standard error.
    pub margin_se: Option<f64>,
    /// What to do.
    pub recommendation: Recommendation,
    /// The plan's objective under stress.
    pub sensitivity: Sensitivity,
    /// Names left out by the safety floor, by symbol.
    pub excluded: Vec<String>,
    /// Plain notes.
    pub notes: Vec<String>,
    /// Paths used to search and to score.
    pub paths: (u32, u32),
    /// Round 25: [`Objective::label`].
    #[serde(default)]
    pub objective_label: String,
    /// Round 25: [`Objective::explainer`].
    #[serde(default)]
    pub objective_explainer: String,
}

/// The objective over a subset of paths. `floor` is the proportional mode's median value
/// in the objective's year, for the wealth floor.
fn objective_value(
    outcomes: &Outcomes,
    objective: Objective,
    floor: Option<f64>,
    paths: std::ops::Range<usize>,
) -> f64 {
    let y = objective.year() as usize - 1;
    let net = |p: &Vec<f64>| p.get(y).copied().unwrap_or(0.0);
    let slice = &outcomes.yearly_net[paths.clone()];
    match objective {
        Objective::RobustIncome { .. } => {
            percentile(&slice.iter().map(net).collect::<Vec<_>>(), 0.10)
        }
        Objective::SmoothMonths { .. } => percentile(
            &outcomes.lowest_month[paths]
                .iter()
                .map(|p| p.get(y).copied().unwrap_or(0.0))
                .collect::<Vec<_>>(),
            0.10,
        ),
        Objective::IncomeWithWealthFloor { floor_pct, .. } => {
            let value = percentile(
                &outcomes.value[paths]
                    .iter()
                    .map(|p| p.get(y).copied().unwrap_or(0.0))
                    .collect::<Vec<_>>(),
                0.5,
            );
            if let Some(floor) = floor
                && value < floor * floor_pct.clamp(0.0, 100.0) / 100.0
            {
                return f64::NEG_INFINITY;
            }
            percentile(&slice.iter().map(net).collect::<Vec<_>>(), 0.5)
        }
    }
}

fn score(outcomes: &Outcomes, objective: Objective, floor: Option<f64>, name: &str) -> PolicyScore {
    let y = objective.year() as usize - 1;
    let net: Vec<f64> = outcomes
        .yearly_net
        .iter()
        .map(|p| p.get(y).copied().unwrap_or(0.0))
        .collect();
    let value: Vec<f64> = outcomes
        .value
        .iter()
        .map(|p| p.get(y).copied().unwrap_or(0.0))
        .collect();
    let objective = objective_value(outcomes, objective, floor, 0..outcomes.yearly_net.len());
    PolicyScore {
        policy: name.to_string(),
        objective: objective.is_finite().then_some(objective),
        p10: percentile(&net, 0.10),
        p50: percentile(&net, 0.5),
        value_p50: percentile(&value, 0.5),
    }
}

fn mode_name(mode: Allocation) -> &'static str {
    match mode {
        Allocation::Proportional => "proportional",
        Allocation::SameHolding => "same_holding",
        Allocation::TopValue => "top_value",
        Allocation::EqualWeight => "equal_weight",
        Allocation::TopYield => "top_yield",
        Allocation::Underweight => "underweight",
    }
}

/// Keeps the `top_n` largest weights over `eligible`, zero elsewhere, normalized.
fn project(weights: &mut [f64], eligible: &[bool], top_n: usize) {
    for (w, ok) in weights.iter_mut().zip(eligible) {
        if !*ok || !w.is_finite() || *w < 0.0 {
            *w = 0.0;
        }
    }
    let mut order: Vec<usize> = (0..weights.len()).filter(|i| weights[*i] > 0.0).collect();
    order.sort_by(|a, b| weights[*b].total_cmp(&weights[*a]).then(a.cmp(b)));
    for &i in order.iter().skip(top_n.max(1)) {
        weights[i] = 0.0;
    }
    let sum: f64 = weights.iter().sum();
    if sum > 0.0 {
        for w in weights.iter_mut() {
            *w /= sum;
        }
    }
}

/// Moves `step` of weight onto `to` (taken from the others in proportion), or off it when
/// `step` is negative.
fn shift(weights: &[f64], to: usize, step: f64) -> Option<Vec<f64>> {
    let current = weights[to];
    let target = (current + step).clamp(0.0, 1.0);
    if (target - current).abs() < 1e-12 {
        return None;
    }
    let others: f64 = 1.0 - current;
    let mut next = weights.to_vec();
    next[to] = target;
    let remaining = 1.0 - target;
    if others <= 1e-12 {
        if remaining > 1e-12 {
            return None;
        }
    } else {
        for (i, w) in next.iter_mut().enumerate() {
            if i != to {
                *w = weights[i] / others * remaining;
            }
        }
    }
    Some(next)
}

/// Pure, seeded: the plan (module doc). `search` and `score` should use different seeds.
pub fn solve(
    inputs: &McInputs,
    rules: &Rules,
    objective: Objective,
    constraints: Constraints,
    search: McSettings,
    score_mc: McSettings,
) -> IncomePlan {
    let years = rules.years.clamp(1, super::MAX_YEARS);
    let objective = objective.with_year(objective.year().clamp(1, years));
    let mut rules = rules.clone();
    rules.limits = constraints.limits();
    // Only simulate up to the objective's year.
    rules.years = objective.year();
    let n = inputs.holdings.len();
    let top_n = constraints.top_n.clamp(1, super::MAX_TOP_N) as usize;

    let growths: Vec<super::Growth> = inputs
        .holdings
        .iter()
        .map(|h| super::growth_with(&h.history, inputs.start, inputs.shrink_k))
        .collect();
    let paying: Vec<bool> = inputs
        .holdings
        .iter()
        .zip(&growths)
        .map(|(h, g)| h.price > 0.0 && g.ttm_per_share > 0.0 && !g.months.is_empty())
        .collect();
    let safe = |i: usize| {
        let h = &inputs.holdings[i];
        if h.safety == SafetyGrade::Unknown {
            h.fund
        } else {
            h.safety.at_least(constraints.min_safety)
        }
    };
    let eligible: Vec<bool> = (0..n).map(|i| paying[i] && safe(i)).collect();
    let excluded: Vec<String> = (0..n)
        .filter(|i| paying[*i] && !safe(*i))
        .map(|i| inputs.holdings[i].symbol.clone())
        .collect();

    let proportional = Policy::Mode {
        allocation: Allocation::Proportional,
        top_n: constraints.top_n,
    };
    let floor_value = |mc: McSettings| -> Option<f64> {
        match objective {
            Objective::IncomeWithWealthFloor { .. } => {
                let base = montecarlo::run(inputs, &rules, &proportional, mc);
                let y = objective.year() as usize - 1;
                Some(percentile(
                    &base
                        .value
                        .iter()
                        .map(|p| p.get(y).copied().unwrap_or(0.0))
                        .collect::<Vec<_>>(),
                    0.5,
                ))
            }
            _ => None,
        }
    };
    let search_floor = floor_value(search);
    let evaluate = |weights: &[f64]| -> f64 {
        let outcomes = montecarlo::run(
            inputs,
            &rules,
            &Policy::Weights {
                weights: weights.to_vec(),
            },
            search,
        );
        objective_value(
            &outcomes,
            objective,
            search_floor,
            0..outcomes.yearly_net.len(),
        )
    };

    // Starts: current value, equal, highest yield.
    let mut starts: Vec<Vec<f64>> = Vec::new();
    let value: Vec<f64> = inputs
        .holdings
        .iter()
        .map(|h| (h.units * h.price * h.fx).max(0.0))
        .collect();
    let mut by_value = value.clone();
    if by_value
        .iter()
        .zip(&eligible)
        .all(|(v, ok)| !*ok || *v <= 0.0)
    {
        by_value = vec![1.0; n];
    }
    starts.push(by_value);
    starts.push(vec![1.0; n]);
    let mut by_yield: Vec<f64> = (0..n)
        .map(|i| {
            if inputs.holdings[i].price > 0.0 {
                let y = growths[i].ttm_per_share / inputs.holdings[i].price;
                y.min(HIGH_YIELD)
            } else {
                0.0
            }
        })
        .collect();
    if by_yield.iter().all(|v| *v <= 0.0) {
        by_yield = vec![1.0; n];
    }
    starts.push(by_yield);

    let mut best: Option<(Vec<f64>, f64)> = None;
    if eligible.iter().any(|ok| *ok) {
        for mut weights in starts {
            project(&mut weights, &eligible, top_n);
            if weights.iter().sum::<f64>() <= 0.0 {
                continue;
            }
            let mut current = evaluate(&weights);
            for step in STEPS {
                for _ in 0..MAX_MOVES_PER_STEP {
                    let mut improved: Option<(Vec<f64>, f64)> = None;
                    for i in (0..n).filter(|i| eligible[*i]) {
                        for signed in [step, -step] {
                            let Some(mut next) = shift(&weights, i, signed) else {
                                continue;
                            };
                            project(&mut next, &eligible, top_n);
                            if next.iter().sum::<f64>() <= 0.0 {
                                continue;
                            }
                            let value = evaluate(&next);
                            let threshold = improved.as_ref().map_or(current, |(_, v)| *v);
                            if value > threshold + 1e-9 {
                                improved = Some((next, value));
                            }
                        }
                    }
                    match improved {
                        Some((next, value)) => {
                            weights = next;
                            current = value;
                        }
                        None => break,
                    }
                }
            }
            if best.as_ref().is_none_or(|(_, v)| current > *v) {
                best = Some((weights, current));
            }
        }
    }

    // Re-score on fresh paths beside the modes.
    let score_floor = floor_value(score_mc);
    let mut scores = Vec::new();
    let plan_outcomes = best.as_ref().map(|(weights, _)| {
        montecarlo::run(
            inputs,
            &rules,
            &Policy::Weights {
                weights: weights.clone(),
            },
            score_mc,
        )
    });
    if let Some(outcomes) = &plan_outcomes {
        scores.push(score(outcomes, objective, score_floor, "plan"));
    }
    let mut mode_outcomes: Vec<(Allocation, Outcomes)> = Vec::new();
    for mode in MODES {
        let outcomes = montecarlo::run(
            inputs,
            &rules,
            &Policy::Mode {
                allocation: mode,
                top_n: constraints.top_n,
            },
            score_mc,
        );
        scores.push(score(&outcomes, objective, score_floor, mode_name(mode)));
        mode_outcomes.push((mode, outcomes));
    }
    let (best_mode, best_mode_outcomes) = mode_outcomes
        .iter()
        .max_by(|a, b| {
            let va = objective_value(&a.1, objective, score_floor, 0..a.1.yearly_net.len());
            let vb = objective_value(&b.1, objective, score_floor, 0..b.1.yearly_net.len());
            va.total_cmp(&vb)
        })
        .map(|(m, o)| (*m, o))
        .unwrap_or((Allocation::Proportional, &mode_outcomes[0].1));

    let paths = score_mc.paths as usize;
    let (margin, margin_se) = match &plan_outcomes {
        Some(plan) if paths >= BATCHES * 5 => {
            let size = paths / BATCHES;
            let diffs: Vec<f64> = (0..BATCHES)
                .map(|b| {
                    let range = b * size..(b + 1) * size;
                    objective_value(plan, objective, score_floor, range.clone())
                        - objective_value(best_mode_outcomes, objective, score_floor, range)
                })
                .filter(|d| d.is_finite())
                .collect();
            if diffs.len() < 2 {
                (None, None)
            } else {
                let mean = diffs.iter().sum::<f64>() / diffs.len() as f64;
                let var = diffs.iter().map(|d| (d - mean).powi(2)).sum::<f64>()
                    / (diffs.len() - 1) as f64;
                (Some(mean), Some((var / diffs.len() as f64).sqrt()))
            }
        }
        _ => (None, None),
    };
    let beats = matches!((margin, margin_se), (Some(m), Some(se)) if m > 2.0 * se && m > 0.0);
    let recommendation = if beats {
        Recommendation::Plan
    } else {
        Recommendation::Mode { mode: best_mode }
    };

    let stressed = |stress: Stress| -> Option<f64> {
        let (weights, _) = best.as_ref()?;
        let mut stressed_rules = rules.clone();
        stressed_rules.stress = stress;
        let outcomes = montecarlo::run(
            inputs,
            &stressed_rules,
            &Policy::Weights {
                weights: weights.clone(),
            },
            score_mc,
        );
        let v = objective_value(
            &outcomes,
            objective,
            score_floor,
            0..outcomes.yearly_net.len(),
        );
        v.is_finite().then_some(v)
    };
    let sensitivity = Sensitivity {
        low_growth: stressed(Stress {
            low_growth: true,
            ..Stress::default()
        }),
        no_cuts: stressed(Stress {
            no_cuts: true,
            ..Stress::default()
        }),
        fx_flat: stressed(Stress {
            fx_flat: true,
            ..Stress::default()
        }),
    };

    let weights_list: Vec<PlanWeight> = match &best {
        Some((weights, _)) => {
            let mut list: Vec<PlanWeight> = weights
                .iter()
                .enumerate()
                .filter(|(_, w)| **w > 1e-6)
                .map(|(i, w)| PlanWeight {
                    key: inputs.holdings[i].key.clone(),
                    symbol: inputs.holdings[i].symbol.clone(),
                    weight: *w,
                    candidate: inputs.holdings[i].candidate,
                })
                .collect();
            list.sort_by(|a, b| b.weight.total_cmp(&a.weight).then(a.key.cmp(&b.key)));
            list
        }
        None => Vec::new(),
    };
    let contribution = rules
        .monthly_contribution
        .clamp(0.0, super::MAX_MONTHLY_CONTRIBUTION);
    let this_month: Vec<PlanBuy> = if beats {
        weights_list
            .iter()
            .map(|w| PlanBuy {
                key: w.key.clone(),
                symbol: w.symbol.clone(),
                amount: (contribution * w.weight * 100.0).round() / 100.0,
                candidate: w.candidate,
            })
            .filter(|b| b.amount > 0.0)
            .collect()
    } else {
        Vec::new()
    };

    let mut notes = Vec::new();
    match recommendation {
        Recommendation::Plan => notes.push(format!(
            "The plan beat the best fixed choice ({}) by {:.0} (± {:.0}) on paths it was not searched on.",
            mode_name(best_mode).replace('_', " "),
            margin.unwrap_or(0.0),
            margin_se.unwrap_or(0.0)
        )),
        Recommendation::Mode { mode } => notes.push(format!(
            "No allocation beat “{}” by more than its simulation noise, so SAIL recommends keeping that choice.",
            mode_name(mode).replace('_', " ")
        )),
    }
    if !excluded.is_empty() {
        notes.push(format!(
            "Left out by the safety floor: {}.",
            excluded.join(", ")
        ));
    }
    if best.is_none() {
        notes.push(
            "No holding or candidate pays dividends with enough history to search over."
                .to_string(),
        );
    }
    notes.push(
        "The plan only says where new money goes; it never sells. SAIL does not place orders."
            .to_string(),
    );
    notes.push("Scores come from simulated paths built on past payouts and prices; a regime the history did not contain can make any plan wrong.".to_string());

    IncomePlan {
        objective,
        constraints,
        weights: weights_list,
        this_month,
        scores,
        best_mode,
        margin,
        margin_se,
        recommendation,
        sensitivity,
        excluded,
        notes,
        paths: (search.paths, score_mc.paths),
        objective_label: objective.label().to_string(),
        objective_explainer: objective.explainer(),
    }
}
