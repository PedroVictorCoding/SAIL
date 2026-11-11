//! Policy-mode and ideas-mode plans against hand-built problems (Kilo's slice, round 11).

use sail_capital::optimize::{Asset, Mode, Plan, Problem, Rule, breaches, plan};
use sail_capital::policy::RiskLimits;

fn asset(symbol: &str, weight: f64) -> Asset {
    Asset {
        instrument_key: format!("key-{symbol}"),
        symbol: symbol.into(),
        weight,
        alpha: 0.0,
        sigma_annual: None,
        foreign_currency: false,
        locked: false,
    }
}

fn problem(assets: Vec<Asset>, cash: f64, account_value: f64) -> Problem {
    let count = assets.len();
    Problem {
        assets,
        cash,
        covariance: vec![0.0; count * count],
        account_value,
        horizon_sessions: 60,
    }
}

fn limits() -> RiskLimits {
    RiskLimits::draft()
}

fn changed_symbols(plan: &Plan) -> Vec<String> {
    plan.changes
        .iter()
        .map(|change| change.symbol.clone())
        .collect()
}

fn rules_for(plan: &Plan, symbol: &str) -> Vec<Rule> {
    plan.changes
        .iter()
        .find(|change| change.symbol == symbol)
        .map(|change| change.reasons.clone())
        .unwrap_or_default()
}

#[test]
fn a_portfolio_within_limits_is_left_alone() {
    let assets = (0..10)
        .map(|index| asset(&format!("S{index}"), 0.09))
        .collect();
    let problem = problem(assets, 0.10, 100_000.0);
    assert!(breaches(&problem, &limits()).is_empty());

    let policy = plan(&problem, &limits(), Mode::Policy);
    assert!(policy.changes.is_empty(), "{policy:?}");
    assert!(policy.feasible);
    assert!(policy.breaches_after.is_empty());
    assert!(
        policy.note.contains("Every limit is met"),
        "{}",
        policy.note
    );

    let ideas = plan(&problem, &limits(), Mode::Ideas);
    assert!(ideas.changes.is_empty(), "{ideas:?}");
    assert!(
        ideas.note.contains("No signal passed validation"),
        "{}",
        ideas.note
    );
}

#[test]
fn an_overweight_position_is_trimmed_to_the_cap() {
    let mut assets = vec![asset("BIG", 0.20)];
    assets.extend((0..9).map(|index| asset(&format!("S{index}"), 70.0 / 9.0 / 100.0)));
    let problem = problem(assets, 0.10, 100_000.0);
    let before = breaches(&problem, &limits());
    assert_eq!(before.len(), 1, "{before:?}");
    assert_eq!(before[0].rule, Rule::MaxPosition);
    assert_eq!(before[0].symbol.as_deref(), Some("BIG"));

    let plan = plan(&problem, &limits(), Mode::Policy);
    assert!(plan.feasible, "{plan:?}");
    assert!(plan.breaches_after.is_empty(), "{plan:?}");
    let big = plan
        .changes
        .iter()
        .find(|change| change.symbol == "BIG")
        .expect("a change for BIG");
    assert!((big.to_pct - 10.0).abs() < 1e-6, "{big:?}");
    assert!(big.amount < 0.0);
    assert!(rules_for(&plan, "BIG").contains(&Rule::MaxPosition));
    // The proceeds raise free funds to 20%; the excess over 15% is one free-funds line,
    // not a purchase spread over every holding.
    assert!((plan.cash_to_pct - 15.0).abs() < 1e-6, "{plan:?}");
    assert_eq!(changed_symbols(&plan), vec!["BIG", "Free funds"]);
    let cash = &plan.changes[1];
    assert_eq!(cash.instrument_key, sail_capital::optimize::CASH_KEY);
    assert_eq!(cash.amount, -5_000.0);
    assert_eq!(cash.reasons, vec![Rule::IdleCash]);
    assert!(
        plan.note.contains("invest the excess free funds"),
        "{}",
        plan.note
    );
}

#[test]
fn a_high_volatility_breach_is_brought_back_to_its_cap() {
    let mut assets = vec![asset("HOT1", 0.10), asset("HOT2", 0.10)];
    for hot in &mut assets {
        hot.sigma_annual = Some(0.80);
    }
    assets.extend((0..8).map(|index| asset(&format!("CALM{index}"), 0.09)));
    let problem = problem(assets, 0.08, 100_000.0);
    let before = breaches(&problem, &limits());
    assert_eq!(before.len(), 1, "{before:?}");
    assert_eq!(before[0].rule, Rule::HighVolShare);
    assert!((before[0].actual_pct - 20.0).abs() < 1e-9);

    let plan = plan(&problem, &limits(), Mode::Policy);
    assert!(plan.feasible, "{plan:?}");
    assert!(plan.breaches_after.is_empty(), "{plan:?}");
    let hot_total: f64 = plan
        .changes
        .iter()
        .filter(|change| change.symbol.starts_with("HOT"))
        .map(|change| change.to_pct / 100.0)
        .sum();
    assert!(hot_total <= 0.15 + 1e-9, "{hot_total}");
    assert_eq!(
        plan.changes
            .iter()
            .filter(|change| change.symbol.starts_with("HOT"))
            .count(),
        2
    );
    for hot in ["HOT1", "HOT2"] {
        assert!(rules_for(&plan, hot).contains(&Rule::HighVolShare), "{hot}");
    }
    assert!((plan.cash_to_pct - 13.0).abs() < 1e-6, "{plan:?}");
}

#[test]
fn low_free_funds_are_raised_to_the_reserve() {
    let assets = (0..12)
        .map(|index| asset(&format!("S{index}"), 98.0 / 12.0 / 100.0))
        .collect();
    let problem = problem(assets, 0.02, 100_000.0);
    let before = breaches(&problem, &limits());
    assert_eq!(before.len(), 1, "{before:?}");
    assert_eq!(before[0].rule, Rule::CashReserve);

    let plan = plan(&problem, &limits(), Mode::Policy);
    assert!(plan.feasible, "{plan:?}");
    assert!(plan.breaches_after.is_empty(), "{plan:?}");
    assert!((plan.cash_to_pct - 5.0).abs() < 1e-6, "{plan:?}");
    // One free-funds line: raise 3,000 by selling holdings of the owner's choice.
    assert_eq!(changed_symbols(&plan), vec!["Free funds"]);
    assert_eq!(plan.changes[0].amount, 3_000.0);
    assert_eq!(plan.changes[0].reasons, vec![Rule::CashReserve]);
    assert!(plan.note.contains("raise free funds"), "{}", plan.note);
}

#[test]
fn idle_free_funds_are_invested_up_to_the_limit() {
    let assets = (0..10)
        .map(|index| asset(&format!("S{index}"), 0.05))
        .collect();
    let problem = problem(assets, 0.50, 100_000.0);
    let before = breaches(&problem, &limits());
    assert_eq!(before.len(), 1, "{before:?}");
    assert_eq!(before[0].rule, Rule::IdleCash);

    let plan = plan(&problem, &limits(), Mode::Policy);
    assert!(plan.feasible, "{plan:?}");
    assert!(plan.breaches_after.is_empty(), "{plan:?}");
    assert!((plan.cash_to_pct - 15.0).abs() < 1e-6, "{plan:?}");
    // One free-funds line: invest 35,000; no instrument is chosen without evidence.
    assert_eq!(changed_symbols(&plan), vec!["Free funds"]);
    assert_eq!(plan.changes[0].amount, -35_000.0);
    assert_eq!(plan.changes[0].reasons, vec![Rule::IdleCash]);
}

#[test]
fn locked_positions_keep_their_weight_and_stay_listed() {
    let mut locked = asset("LOCKED", 0.20);
    locked.locked = true;
    let mut assets = vec![locked];
    assets.extend((0..8).map(|index| asset(&format!("S{index}"), 0.09)));
    let problem = problem(assets, 0.08, 100_000.0);
    let before = breaches(&problem, &limits());
    assert_eq!(before.len(), 1, "{before:?}");
    assert_eq!(before[0].symbol.as_deref(), Some("LOCKED"));

    let plan = plan(&problem, &limits(), Mode::Policy);
    assert!(!plan.feasible, "{plan:?}");
    assert!(changed_symbols(&plan).is_empty(), "{plan:?}");
    assert_eq!(plan.breaches_after, before);
    assert!(
        plan.note.contains("No change large enough"),
        "{}",
        plan.note
    );
}

#[test]
fn changes_below_the_smallest_trade_are_dropped() {
    let assets = (0..12)
        .map(|index| asset(&format!("S{index}"), 98.0 / 12.0 / 100.0))
        .collect();
    let problem = problem(assets, 0.02, 10_000.0);
    // Raising 3% of 10,000 is 300, below a 500 smallest trade.
    let mut small = limits();
    small.min_trade = rust_decimal::Decimal::from(500);
    let plan = plan(&problem, &small, Mode::Policy);
    assert!(plan.changes.is_empty(), "{plan:?}");
    assert!(!plan.feasible);
    assert!(
        plan.breaches_after
            .iter()
            .any(|breach| breach.rule == Rule::CashReserve)
    );
}

#[test]
fn impossible_limits_are_reported_not_hidden() {
    let mut locked = asset("LOCKED", 0.98);
    locked.locked = true;
    let assets = vec![locked, asset("SMALL", 0.02)];
    let problem = problem(assets, 0.0, 100_000.0);
    let before = breaches(&problem, &limits());
    assert!(!before.is_empty());
    let plan = plan(&problem, &limits(), Mode::Policy);
    assert!(!plan.feasible);
    // The locked position can't be trimmed, but the cash reserve can still be raised.
    assert_eq!(changed_symbols(&plan), vec!["Free funds"], "{plan:?}");
    assert_eq!(
        plan.breaches_after,
        before
            .iter()
            .filter(|b| b.rule == Rule::MaxPosition)
            .cloned()
            .collect::<Vec<_>>()
    );
    assert!(plan.note.contains("cannot all be met"), "{}", plan.note);
}

#[test]
fn the_turnover_limit_caps_what_one_plan_may_move() {
    let mut assets = vec![asset("BIG", 0.20)];
    assets.extend((0..9).map(|index| asset(&format!("S{index}"), 70.0 / 9.0 / 100.0)));
    let problem = problem(assets, 0.10, 100_000.0);
    let mut limits = limits();
    limits.max_turnover_pct = rust_decimal::Decimal::ONE;
    let plan = plan(&problem, &limits, Mode::Policy);
    assert!(!plan.feasible, "{plan:?}");
    let big = plan
        .changes
        .iter()
        .find(|change| change.symbol == "BIG")
        .expect("a change for BIG");
    assert!(big.to_pct < 20.0 && big.to_pct > 10.0, "{big:?}");
    let moved: f64 = plan.changes.iter().map(|change| change.amount.abs()).sum();
    assert!(moved <= 0.02 * 100_000.0 + 1.0, "{moved}");
    assert!(
        plan.breaches_after
            .iter()
            .any(|breach| breach.rule == Rule::MaxPosition),
        "{plan:?}"
    );
}

#[test]
fn the_same_problem_gives_the_same_plan() {
    let mut assets = vec![asset("HOT1", 0.10), asset("HOT2", 0.10)];
    for hot in &mut assets {
        hot.sigma_annual = Some(0.80);
    }
    assets.extend((0..8).map(|index| asset(&format!("CALM{index}"), 0.09)));
    let problem = problem(assets, 0.08, 100_000.0);
    let first = plan(&problem, &limits(), Mode::Policy);
    let second = plan(&problem, &limits(), Mode::Policy);
    assert_eq!(first, second);

    let mut tweaked = problem.clone();
    for asset in &mut tweaked.assets {
        if !asset.locked {
            asset.alpha = if asset.sigma_annual.is_some() {
                -0.02
            } else {
                0.01
            };
        }
    }
    assert_eq!(
        plan(&tweaked, &limits(), Mode::Ideas),
        plan(&tweaked, &limits(), Mode::Ideas)
    );
}
