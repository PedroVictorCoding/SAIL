//! Ideas-mode plans: the return/risk/cost trade-off, against brute force (Kilo's slice,
//! round 11).

use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use sail_capital::optimize::{Asset, Mode, Problem, plan};
use sail_capital::policy::RiskLimits;

fn decimal(value: f64) -> Decimal {
    Decimal::from_f64(value).expect("finite decimal")
}

fn asset(symbol: &str, weight: f64, alpha: f64) -> Asset {
    Asset {
        instrument_key: format!("key-{symbol}"),
        symbol: symbol.into(),
        weight,
        alpha,
        sigma_annual: None,
        foreign_currency: false,
        locked: false,
    }
}

fn permissive_limits() -> RiskLimits {
    RiskLimits {
        max_position_pct: decimal(60.0),
        cash_min_pct: decimal(0.0),
        cash_max_pct: decimal(100.0),
        high_vol_threshold_pct: decimal(500.0),
        max_high_vol_pct: decimal(100.0),
        max_turnover_pct: decimal(100.0),
        min_trade: decimal(0.0),
        trade_cost_bps: decimal(0.0),
        fx_cost_bps: decimal(0.0),
        risk_aversion: decimal(4.0),
        risk_per_trade_pct: decimal(0.5),
        max_heat_pct: Decimal::from(3),
        max_cluster_heat_pct: Decimal::new(15, 1),
    }
}

fn three_asset_problem() -> Problem {
    Problem {
        assets: vec![
            asset("A", 0.20, 0.050),
            asset("B", 0.20, 0.020),
            asset("C", 0.20, 0.000),
        ],
        cash: 0.40,
        covariance: vec![0.04, 0.0, 0.0, 0.0, 0.01, 0.0, 0.0, 0.0, 0.0025],
        account_value: 1_000.0,
        horizon_sessions: 60,
    }
}

fn weights_of(problem: &Problem, plan: &sail_capital::optimize::Plan) -> Vec<f64> {
    problem
        .assets
        .iter()
        .map(|asset| {
            plan.changes
                .iter()
                .find(|change| change.symbol == asset.symbol)
                .map(|change| change.to_pct / 100.0)
                .unwrap_or(asset.weight)
        })
        .collect()
}

fn objective(problem: &Problem, weights: &[f64], risk_aversion: f64) -> f64 {
    let mut gain = 0.0;
    let mut risk_after = 0.0;
    let mut risk_before = 0.0;
    for (index, asset) in problem.assets.iter().enumerate() {
        gain += asset.alpha * (weights[index] - asset.weight);
        for (other, other_asset) in problem.assets.iter().enumerate() {
            let entry = problem.covariance[index * problem.assets.len() + other];
            risk_after += 0.5 * risk_aversion * weights[index] * entry * weights[other];
            risk_before += 0.5 * risk_aversion * asset.weight * entry * other_asset.weight;
        }
    }
    problem.account_value * (gain - (risk_after - risk_before))
}

#[test]
fn ideas_match_brute_force_on_a_three_asset_grid() {
    let problem = three_asset_problem();
    let limits = permissive_limits();
    let plan = plan(&problem, &limits, Mode::Ideas);
    assert!(plan.feasible, "{plan:?}");
    assert!(!plan.changes.is_empty(), "{plan:?}");
    assert!(plan.net_benefit > 0.0, "{plan:?}");

    let mut best = f64::NEG_INFINITY;
    let mut best_weights = [0.0f64; 3];
    let step = 0.005;
    let steps = 120;
    for first in 0..=steps {
        for second in 0..=steps {
            for third in 0..=steps {
                let weights = [
                    first as f64 * step,
                    second as f64 * step,
                    third as f64 * step,
                ];
                if weights.iter().sum::<f64>() > 1.0 + 1e-12 {
                    continue;
                }
                let value = objective(&problem, &weights, 4.0);
                if value > best {
                    best = value;
                    best_weights = weights;
                }
            }
        }
    }

    assert!(
        plan.net_benefit >= best - 1e-3,
        "plan {} is worse than the grid best {} at {:?}",
        plan.net_benefit,
        best,
        best_weights
    );
    let weights = weights_of(&problem, &plan);
    let analytic = [0.3125, 0.5, 0.0];
    for (index, expected) in analytic.iter().enumerate() {
        assert!(
            (weights[index] - expected).abs() < 0.02,
            "weight {} is {} not {} ({weights:?})",
            index,
            weights[index],
            expected
        );
    }
    assert!(
        (weights.iter().sum::<f64>() - 0.8125).abs() < 0.02,
        "{weights:?}"
    );
}

#[test]
fn a_signal_that_cannot_cover_costs_is_ignored() {
    let mut problem = three_asset_problem();
    problem.assets[0].alpha = 0.0004;
    problem.assets[1].alpha = 0.0;
    let mut limits = permissive_limits();
    limits.trade_cost_bps = decimal(500.0);
    let plan = plan(&problem, &limits, Mode::Ideas);
    assert!(plan.changes.is_empty(), "{plan:?}");
    assert!(plan.note.contains("Doing nothing is best"), "{}", plan.note);
    assert_eq!(plan.net_benefit, 0.0);
}

#[test]
fn ideas_never_move_a_locked_position() {
    let mut problem = three_asset_problem();
    problem.assets[2].locked = true;
    problem.assets[2].alpha = 0.5;
    problem.assets[0].alpha = 0.02;
    problem.assets[1].alpha = 0.01;
    let limits = permissive_limits();
    let plan = plan(&problem, &limits, Mode::Ideas);
    assert!(
        !plan.changes.iter().any(|change| change.symbol == "C"),
        "{plan:?}"
    );
    assert!(plan.feasible, "{plan:?}");
}

#[test]
fn ideas_stay_inside_the_policy_limits() {
    let mut limits = permissive_limits();
    limits.max_position_pct = decimal(30.0);
    limits.cash_min_pct = decimal(10.0);
    limits.cash_max_pct = decimal(20.0);
    let problem = three_asset_problem();
    let plan = plan(&problem, &limits, Mode::Ideas);
    assert!(plan.feasible, "{plan:?}");
    for change in &plan.changes {
        assert!(change.to_pct <= 30.0 + 1e-6, "{change:?}");
    }
    assert!(plan.cash_to_pct >= 10.0 - 1e-6, "{plan:?}");
    assert!(plan.cash_to_pct <= 20.0 + 1e-6, "{plan:?}");
}
