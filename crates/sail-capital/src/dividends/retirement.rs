//! Early-retirement plan from the dividend projection: when income (or capital) reaches
//! what the owner needs to stop working.
//!
//! The plan is derived from the same monthly simulation as the projection (`super::project`),
//! so changing reinvestment, allocation, the allowance or the tax setting changes the
//! retirement date in a way the owner can see and explain. Two independent tests decide
//! whether a scenario is funded at the retirement age:
//!
//! - **Income**: the projection's net dividends in that year already cover the target,
//!   adjusted for inflation. This is the dividend-investor route: principal stays invested.
//! - **Capital**: the paying portfolio is worth at least the target annual spending divided
//!   by the owner's withdrawal rate. That is the sell-down route; sequence-of-returns risk
//!   is not simulated here.
//!
//! Everything is pure: no I/O and no clock. Money is `f64` in the account currency.
use serde::{Deserialize, Serialize};

use super::{
    Holding, MAX_MONTHLY_CONTRIBUTION, MAX_YEARS, PT_TAX, Projection, Scenario, Settings, growth,
    project,
};

/// Youngest age accepted in the assumptions.
pub const MIN_AGE: u32 = 16;
/// Oldest age accepted in the assumptions.
pub const MAX_AGE: u32 = 95;
/// Largest monthly net income target accepted, in the account currency (today's money).
pub const MAX_TARGET_MONTHLY: f64 = 100_000.0;
/// Largest annual inflation accepted.
pub const MAX_INFLATION_PCT: f64 = 10.0;
/// Smallest withdrawal rate accepted.
pub const MIN_WITHDRAWAL_PCT: f64 = 1.0;
/// Largest withdrawal rate accepted.
pub const MAX_WITHDRAWAL_PCT: f64 = 10.0;

/// What the owner expects to need, in today's money.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Assumptions {
    /// Age today.
    pub current_age: u32,
    /// Age at which the owner wants income to cover spending.
    pub retire_age: u32,
    /// Monthly net spending target, in today's money.
    pub target_monthly_net: f64,
    /// Annual inflation applied to the target, percent.
    pub inflation_pct: f64,
    /// Share of the portfolio the owner is willing to withdraw in the first year,
    /// percent. The capital target is the annual spending divided by this.
    pub withdrawal_rate_pct: f64,
}

impl Default for Assumptions {
    fn default() -> Self {
        Self {
            current_age: 35,
            retire_age: 55,
            target_monthly_net: 2_500.0,
            inflation_pct: 2.0,
            withdrawal_rate_pct: 4.0,
        }
    }
}

impl Assumptions {
    /// Reject values the plan cannot work with. Messages are owner-readable.
    pub fn validate(&self) -> Result<(), String> {
        if !(MIN_AGE..=MAX_AGE).contains(&self.current_age) {
            return Err(format!(
                "The current age must be between {MIN_AGE} and {MAX_AGE}."
            ));
        }
        if !(MIN_AGE..=MAX_AGE).contains(&self.retire_age) {
            return Err(format!(
                "The retirement age must be between {MIN_AGE} and {MAX_AGE}."
            ));
        }
        if self.retire_age <= self.current_age {
            return Err("The retirement age must be later than the current age.".into());
        }
        let years = self.years_to_retirement();
        if years > MAX_YEARS {
            return Err(format!(
                "The retirement age is {years} years away; the projection looks at most {MAX_YEARS} years ahead."
            ));
        }
        if !self.target_monthly_net.is_finite()
            || !(0.0..=MAX_TARGET_MONTHLY).contains(&self.target_monthly_net)
            || self.target_monthly_net <= 0.0
        {
            return Err(format!(
                "The monthly target must be between 0 and {MAX_TARGET_MONTHLY:.0}."
            ));
        }
        if !self.inflation_pct.is_finite()
            || !(0.0..=MAX_INFLATION_PCT).contains(&self.inflation_pct)
        {
            return Err(format!(
                "Inflation must be between 0 and {MAX_INFLATION_PCT:.0}%."
            ));
        }
        if !self.withdrawal_rate_pct.is_finite()
            || !(MIN_WITHDRAWAL_PCT..=MAX_WITHDRAWAL_PCT).contains(&self.withdrawal_rate_pct)
        {
            return Err(format!(
                "The withdrawal rate must be between {MIN_WITHDRAWAL_PCT:.0} and {MAX_WITHDRAWAL_PCT:.0}%."
            ));
        }
        Ok(())
    }

    /// Completed years between today and the retirement age.
    pub fn years_to_retirement(&self) -> u32 {
        self.retire_age.saturating_sub(self.current_age)
    }
}

/// One scenario's progress toward the target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScenarioPlan {
    /// Which scenario.
    pub scenario: Scenario,
    /// Projected monthly net income in the retirement year, in today's money.
    pub monthly_net_today: f64,
    /// Projected monthly net income in the retirement year, nominal.
    pub monthly_net_at_retirement: f64,
    /// Projected annual net dividend income in the retirement year, nominal.
    pub annual_net_at_retirement: f64,
    /// Paying portfolio value at the end of the retirement year, nominal.
    pub value_at_retirement: f64,
    /// First projection year after which income stays at or above the inflation-adjusted
    /// target until the retirement year; `None` if it never does.
    pub income_reached_year: Option<u32>,
    /// The age at [`Self::income_reached_year`].
    pub income_reached_age: Option<u32>,
    /// Whether income alone covers the target in the retirement year.
    pub funded_by_income: bool,
    /// Whether the portfolio lasts at the owner's withdrawal rate in the retirement year.
    pub funded_by_capital: bool,
}

/// The plan the API and the page show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// Inputs used.
    pub assumptions: Assumptions,
    /// Years between today and the retirement age.
    pub years_to_retirement: u32,
    /// The projection year the retirement age falls in (1-based).
    pub retirement_year: u32,
    /// Monthly target at the retirement age, nominal.
    pub target_monthly_at_retirement: f64,
    /// Annual target at the retirement age, nominal.
    pub target_annual_at_retirement: f64,
    /// Paying-portfolio value the withdrawal rate needs at the retirement age, nominal.
    pub required_capital: f64,
    /// Base-scenario year-1 net income as a share of the annual target today, percent
    /// (capped at 999 for display).
    pub progress_pct: f64,
    /// Base-scenario monthly net income in year 1, in today's money.
    pub base_monthly_net_today: f64,
    /// Monthly allowance already set on the projection.
    pub current_monthly_contribution: f64,
    /// Smallest monthly allowance (rounded up to 10) that makes income cover the target
    /// in the base scenario by the retirement age; `None` when even the maximum does not.
    pub required_monthly_contribution: Option<f64>,
    /// Low, base and high.
    pub scenarios: Vec<ScenarioPlan>,
    /// Plain notes on method and limits.
    pub notes: Vec<String>,
}

/// Builds the early-retirement plan from the same inputs as the projection.
pub fn plan(
    holdings: &[Holding],
    settings: &Settings,
    original_cost: Option<f64>,
    assumptions: &Assumptions,
) -> Result<Plan, String> {
    assumptions.validate()?;
    let retirement_year = assumptions.years_to_retirement();
    if retirement_year == 0 {
        return Err("The retirement age must be later than the current age.".into());
    }
    let mut plan_settings = settings.clone();
    plan_settings.years = plan_settings.years.max(retirement_year).min(MAX_YEARS);
    let outlook = project(holdings, &plan_settings, original_cost);

    let inflation = 1.0 + assumptions.inflation_pct / 100.0;
    let factor = inflation.powi(retirement_year as i32);
    let target_annual_today = assumptions.target_monthly_net * 12.0;
    let target_annual_nominal = target_annual_today * factor;
    let required_capital = target_annual_nominal / (assumptions.withdrawal_rate_pct / 100.0);

    let scenarios: Vec<ScenarioPlan> = outlook
        .scenarios
        .iter()
        .map(|projection| {
            scenario_plan(
                projection,
                assumptions,
                retirement_year,
                target_annual_nominal,
                required_capital,
                factor,
            )
        })
        .collect();
    let current_net = current_annual_net(holdings, settings);
    let progress_pct = if target_annual_today > 0.0 {
        (current_net / target_annual_today * 100.0).clamp(0.0, 999.0)
    } else {
        0.0
    };

    let required_monthly_contribution = required_contribution(
        holdings,
        &plan_settings,
        original_cost,
        target_annual_nominal,
        retirement_year,
    );

    let mut notes = vec![
        "The income test counts the projection's net dividends in the retirement year; the capital test divides that year's spending target by your withdrawal rate and compares it with the paying portfolio's value.".to_string(),
        "Both tests use the same scenarios as the projection. They are scenarios, not probabilities, and neither simulates a market crash in the order it happens (sequence risk).".to_string(),
        "The target grows with the inflation you set; the projection's income is in the account currency. Taxes already in the projection are the ones the page shows.".to_string(),
        "A dividend cut, a flat market or a lower reinvestment rate can move the date later. This is a planning estimate, not financial advice.".to_string(),
    ];
    match required_monthly_contribution {
        Some(0.0) => notes.insert(
            0,
            "The base scenario already covers the target by the retirement age without a monthly allowance.".to_string(),
        ),
        Some(needed) => notes.insert(
            0,
            format!(
                "In the base scenario, a monthly allowance of about {needed:.0} brings income up to the target by age {} (the projection currently uses {:.0}).",
                assumptions.retire_age, settings.monthly_contribution
            ),
        ),
        None => notes.insert(
            0,
            format!(
                "Even the maximum monthly allowance ({MAX_MONTHLY_CONTRIBUTION:.0}) does not bring income up to the target by age {} in the base scenario; a later age, a lower target or a different allocation closes the gap.",
                assumptions.retire_age
            ),
        ),
    }
    if !holdings.iter().any(|holding| {
        !holding.history.is_empty() && holding.units > 0.0 && holding.price.is_finite()
    }) {
        notes.insert(
            0,
            "There are no dividend-paying holdings with payout history yet, so projected income is zero.".to_string(),
        );
    }

    Ok(Plan {
        assumptions: *assumptions,
        years_to_retirement: retirement_year,
        retirement_year,
        target_monthly_at_retirement: target_annual_nominal / 12.0,
        target_annual_at_retirement: target_annual_nominal,
        required_capital,
        progress_pct,
        base_monthly_net_today: current_net / 12.0,
        current_monthly_contribution: settings.monthly_contribution,
        required_monthly_contribution,
        scenarios,
        notes,
    })
}

/// The income the holdings pay today, before any reinvestment: trailing twelve months per
/// share, converted at today's rate, less withholding and the Portuguese reserve.
fn current_annual_net(holdings: &[Holding], settings: &Settings) -> f64 {
    holdings
        .iter()
        .map(|holding| {
            let growth = growth(&holding.history, settings.start);
            let gross = holding.units.max(0.0) * growth.ttm_per_share * holding.fx.max(0.0);
            if !gross.is_finite() {
                return 0.0;
            }
            let withheld = gross * holding.withholding.clamp(0.0, 1.0);
            let pt_tax = if settings.reserve_pt_tax {
                (gross * PT_TAX - withheld).max(0.0)
            } else {
                0.0
            };
            (gross - withheld - pt_tax).max(0.0)
        })
        .sum()
}

fn scenario_plan(
    projection: &Projection,
    assumptions: &Assumptions,
    retirement_year: u32,
    target_annual_nominal: f64,
    required_capital: f64,
    factor: f64,
) -> ScenarioPlan {
    let row = projection
        .years
        .get((retirement_year.saturating_sub(1)) as usize);
    let net = row.map(|row| row.net).unwrap_or(0.0);
    let value = row.map(|row| row.value).unwrap_or(0.0);
    let reached = first_sustained_year(
        projection,
        assumptions.target_monthly_net * 12.0,
        factor,
        retirement_year,
    );
    ScenarioPlan {
        scenario: projection.scenario,
        monthly_net_today: if factor > 0.0 {
            net / 12.0 / factor
        } else {
            0.0
        },
        monthly_net_at_retirement: net / 12.0,
        annual_net_at_retirement: net,
        value_at_retirement: value,
        income_reached_year: reached,
        income_reached_age: reached.map(|year| assumptions.current_age + year),
        funded_by_income: net + f64::EPSILON >= target_annual_nominal,
        funded_by_capital: value + f64::EPSILON >= required_capital,
    }
}

/// The first year after which net income stays at or above its inflation-adjusted target
/// through `last_year`, or `None` if that never happens in the window.
fn first_sustained_year(
    projection: &Projection,
    target_annual_today: f64,
    factor: f64,
    last_year: u32,
) -> Option<u32> {
    if target_annual_today <= 0.0 || !factor.is_finite() || factor <= 0.0 {
        return None;
    }
    for year in (1..=last_year).rev() {
        let target = target_annual_today * factor.powi(year as i32);
        let net = projection
            .years
            .get((year - 1) as usize)
            .map(|row| row.net)
            .unwrap_or(0.0);
        if net + f64::EPSILON < target {
            let reached = year + 1;
            return (reached <= last_year).then_some(reached);
        }
    }
    Some(1)
}

/// Smallest monthly allowance that makes base-scenario income cover the target by
/// `retirement_year`, bisected over the projection and rounded up to the next 10.
fn required_contribution(
    holdings: &[Holding],
    settings: &Settings,
    original_cost: Option<f64>,
    target_annual_nominal: f64,
    retirement_year: u32,
) -> Option<f64> {
    let covers = |contribution: f64| {
        let mut candidate = settings.clone();
        candidate.years = retirement_year;
        candidate.monthly_contribution = contribution;
        project(holdings, &candidate, original_cost)
            .scenarios
            .iter()
            .find(|projection| projection.scenario == Scenario::Base)
            .and_then(|projection| projection.years.get((retirement_year - 1) as usize))
            .is_some_and(|row| row.net + f64::EPSILON >= target_annual_nominal)
    };
    if covers(0.0) {
        return Some(0.0);
    }
    if !covers(MAX_MONTHLY_CONTRIBUTION) {
        return None;
    }
    let mut low = 0.0;
    let mut high = MAX_MONTHLY_CONTRIBUTION;
    for _ in 0..24 {
        let middle = (low + high) / 2.0;
        if covers(middle) {
            high = middle;
        } else {
            low = middle;
        }
    }
    let rounded = (high / 10.0).ceil() * 10.0;
    Some(rounded.min(MAX_MONTHLY_CONTRIBUTION))
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;
    use crate::dividends::{Allocation, Payment};

    fn holding(symbol: &str, units: f64, price: f64, monthly: f64) -> Holding {
        let mut history = Vec::new();
        for year in 2021..=2025 {
            for month in 1..=12 {
                history.push(Payment {
                    ex_date: NaiveDate::from_ymd_opt(year, month, 15).expect("date"),
                    per_share: monthly,
                });
            }
        }
        Holding {
            key: format!("{symbol}-key"),
            symbol: symbol.to_string(),
            units,
            price,
            fx: 1.0,
            history,
            withholding: 0.0,
            withholding_source: "observed".into(),
            price_growth: None,
        }
    }

    fn settings(monthly_contribution: f64) -> Settings {
        Settings {
            years: 30,
            reinvest_pct: 100.0,
            allocation: Allocation::Proportional,
            top_n: 5,
            monthly_contribution,
            reserve_pt_tax: false,
            start: NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        }
    }

    fn assumptions() -> Assumptions {
        Assumptions {
            current_age: 40,
            retire_age: 50,
            target_monthly_net: 100.0,
            inflation_pct: 0.0,
            withdrawal_rate_pct: 4.0,
        }
    }

    #[test]
    fn a_covered_target_reports_progress_and_funding() {
        // 1,000 units paying 1.2 per share a year net 1,200, i.e. 100 a month.
        let holdings = vec![holding("PAY", 1_000.0, 20.0, 0.1)];
        let plan = plan(&holdings, &settings(0.0), Some(20_000.0), &assumptions()).expect("plan");
        assert_eq!(plan.years_to_retirement, 10);
        assert_eq!(plan.retirement_year, 10);
        assert_eq!(plan.target_annual_at_retirement, 1_200.0);
        assert_eq!(plan.required_capital, 30_000.0);
        assert!((plan.progress_pct - 100.0).abs() < 0.01, "{plan:?}");
        let base = plan
            .scenarios
            .iter()
            .find(|scenario| scenario.scenario == Scenario::Base)
            .expect("base");
        assert!(base.funded_by_income, "{base:?}");
        assert_eq!(base.income_reached_year, Some(1));
        assert_eq!(base.income_reached_age, Some(41));
        assert_eq!(plan.required_monthly_contribution, Some(0.0));
    }

    #[test]
    fn inflation_raises_the_target_and_the_capital_needed() {
        let holdings = vec![holding("PAY", 1_000.0, 20.0, 0.1)];
        let mut inflated = assumptions();
        inflated.inflation_pct = 3.0;
        let flat = plan(&holdings, &settings(0.0), None, &assumptions()).expect("flat");
        let rising = plan(&holdings, &settings(0.0), None, &inflated).expect("rising");
        assert!(rising.target_monthly_at_retirement > flat.target_monthly_at_retirement);
        assert!(rising.required_capital > flat.required_capital);
        let factor = 1.03_f64.powi(10);
        assert!(
            (rising.required_capital - flat.required_capital * factor).abs() < 0.01,
            "{} vs {}",
            rising.required_capital,
            flat.required_capital * factor
        );
    }

    #[test]
    fn a_lower_withdrawal_rate_needs_more_capital() {
        let holdings = vec![holding("PAY", 1_000.0, 20.0, 0.1)];
        let mut cautious = assumptions();
        cautious.withdrawal_rate_pct = 3.0;
        let four = plan(&holdings, &settings(0.0), None, &assumptions()).expect("4%");
        let three = plan(&holdings, &settings(0.0), None, &cautious).expect("3%");
        assert!(three.required_capital > four.required_capital);
    }

    #[test]
    fn an_allowance_closes_an_income_gap_and_is_monotone_in_the_retirement_age() {
        let holdings = vec![holding("PAY", 1_000.0, 20.0, 0.02)];
        let mut early = assumptions();
        early.current_age = 40;
        early.retire_age = 45;
        let mut late = assumptions();
        late.current_age = 40;
        late.retire_age = 60;
        let early_plan = plan(&holdings, &settings(0.0), None, &early).expect("early");
        let late_plan = plan(&holdings, &settings(0.0), None, &late).expect("late");
        let early_needed = early_plan.required_monthly_contribution.expect("early gap");
        let late_needed = late_plan.required_monthly_contribution.expect("late gap");
        assert!(early_needed > 0.0);
        assert!(
            early_needed >= late_needed,
            "a longer runway needs no more than a short one: {early_needed} vs {late_needed}"
        );
        assert!(late_plan.progress_pct <= 100.0);
    }

    #[test]
    fn the_plan_is_pure_and_repeatable() {
        let holdings = vec![holding("PAY", 1_000.0, 20.0, 0.05)];
        let first = plan(&holdings, &settings(50.0), None, &assumptions()).expect("first");
        let second = plan(&holdings, &settings(50.0), None, &assumptions()).expect("second");
        assert_eq!(first, second);
    }

    #[test]
    fn assumptions_are_validated() {
        let holdings = vec![holding("PAY", 1.0, 1.0, 0.01)];
        let mut bad = assumptions();
        bad.current_age = 60;
        bad.retire_age = 50;
        assert!(plan(&holdings, &settings(0.0), None, &bad).is_err());
        let mut bad = assumptions();
        bad.retire_age = 90;
        bad.current_age = 20;
        assert!(plan(&holdings, &settings(0.0), None, &bad).is_err());
        let mut bad = assumptions();
        bad.target_monthly_net = 0.0;
        assert!(plan(&holdings, &settings(0.0), None, &bad).is_err());
        let mut bad = assumptions();
        bad.withdrawal_rate_pct = 0.5;
        assert!(plan(&holdings, &settings(0.0), None, &bad).is_err());
        let mut bad = assumptions();
        bad.inflation_pct = f64::NAN;
        assert!(plan(&holdings, &settings(0.0), None, &bad).is_err());
    }

    #[test]
    fn no_paying_holdings_is_an_honest_zero() {
        let holdings: Vec<Holding> = Vec::new();
        let plan = plan(&holdings, &settings(0.0), None, &assumptions()).expect("plan");
        assert_eq!(plan.progress_pct, 0.0);
        assert_eq!(plan.base_monthly_net_today, 0.0);
        assert!(
            plan.scenarios
                .iter()
                .all(|scenario| !scenario.funded_by_income)
        );
        assert!(
            plan.notes
                .iter()
                .any(|note| note.contains("no dividend-paying"))
        );
    }
}
