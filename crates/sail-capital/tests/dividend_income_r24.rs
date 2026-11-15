//! Round 24 (Claude): special dividends, the growth calibration backtest, the seeded
//! Monte Carlo and the income maximizer. Synthetic histories only; no files, clock or
//! network.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use sail_capital::dividends::calibrate::{
    CalibrationInput, PRIOR_CUT_RATE, backtest, bucket_of, cut_hazard,
};
use sail_capital::dividends::maximize::{Constraints, Objective, Recommendation, solve};
use sail_capital::dividends::montecarlo::{
    Limits, McHolding, McInputs, McSettings, Policy, Rules, run, simulate,
};
use sail_capital::dividends::safety::SafetyGrade;
use sail_capital::dividends::{
    Allocation, DEFAULT_SHRINK_K, Payment, PaymentKind, classify, growth, growth_with, specials,
};

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
}

/// Quarterly payments from `first` to `last` year, growing `g` a year from `start`.
fn quarterly(first: i32, last: i32, start: f64, g: f64) -> Vec<Payment> {
    let mut out = Vec::new();
    for year in first..=last {
        let amount = start * (1.0 + g).powi(year - first);
        for month in [3, 6, 9, 12] {
            out.push(Payment {
                ex_date: date(year, month, 15),
                per_share: amount / 4.0,
            });
        }
    }
    out
}

// ---- B1 specials -------------------------------------------------------------------

#[test]
fn a_one_off_payment_is_special_and_leaves_growth_alone() {
    let mut history = quarterly(2018, 2025, 4.0, 0.05);
    history.push(Payment {
        ex_date: date(2024, 7, 1),
        per_share: 10.0,
    });
    let kinds = classify(&history);
    let special: Vec<_> = kinds
        .iter()
        .filter(|(_, k)| *k == PaymentKind::Special)
        .collect();
    assert_eq!(special.len(), 1);
    assert_eq!(special[0].0.ex_date, date(2024, 7, 1));

    let with = growth(&history, date(2026, 1, 1));
    let without = growth(&quarterly(2018, 2025, 4.0, 0.05), date(2026, 1, 1));
    assert_eq!(with, without, "the special changed the estimate");
    assert_eq!(specials(&history, date(2026, 1, 1)).len(), 1);
    assert!(specials(&history, date(2024, 6, 30)).is_empty());
}

#[test]
fn a_switch_to_annual_payments_is_not_special() {
    // Quarterly 1.0 until 2022, then one annual 4.2 a year.
    let mut history: Vec<Payment> = quarterly(2018, 2022, 4.0, 0.0);
    for year in 2023..=2025 {
        history.push(Payment {
            ex_date: date(year, 6, 1),
            per_share: 4.2,
        });
    }
    assert!(
        classify(&history)
            .iter()
            .all(|(_, k)| *k == PaymentKind::Regular),
        "a frequency change was read as specials"
    );
}

#[test]
fn growth_with_the_default_k_is_growth() {
    let history = quarterly(2015, 2025, 2.0, 0.08);
    let as_of = date(2026, 1, 1);
    assert_eq!(
        growth(&history, as_of),
        growth_with(&history, as_of, DEFAULT_SHRINK_K)
    );
    // Less shrinkage keeps more of the measured 8%.
    assert!(growth_with(&history, as_of, 1.0).g > growth_with(&history, as_of, 10.0).g);
}

#[test]
fn growth_ignores_payments_after_as_of() {
    let history = quarterly(2015, 2025, 2.0, 0.08);
    let mut later = history.clone();
    later.push(Payment {
        ex_date: date(2026, 3, 15),
        per_share: 100.0,
    });
    let as_of = date(2026, 1, 1);
    assert_eq!(growth(&history, as_of), growth(&later, as_of));
}

// ---- B2 calibration ----------------------------------------------------------------

fn steady(symbol: &str, g: f64) -> CalibrationInput {
    let history = quarterly(2004, 2025, 1.0, g);
    let year_end_prices: BTreeMap<i32, f64> = (2004..=2025)
        .map(|y| (y, 40.0 * (1.0 + g).powi(y - 2004)))
        .collect();
    CalibrationInput {
        symbol: symbol.into(),
        history,
        year_end_prices,
    }
}

#[test]
fn steady_growers_are_scored_and_lean_toward_less_shrinkage() {
    let inputs: Vec<_> = (0..10)
        .map(|i| steady(&format!("S{i}"), 0.06 + 0.002 * i as f64))
        .collect();
    let report = backtest(&inputs, 2024);
    assert_eq!(report.symbols, 10);
    let by_k = |k: f64| report.shrink.iter().find(|r| r.k == k).expect("grid row");
    assert!(by_k(3.0).n_5y > 0 && by_k(3.0).n_1y > by_k(3.0).n_5y);
    // Shrinking steady growth toward zero under-forecasts it.
    assert!(by_k(10.0).bias_5y < by_k(1.0).bias_5y);
    assert!(by_k(1.0).mae_5y < by_k(10.0).mae_5y);
    assert_eq!(report.best_k, 1.0);
    // Ten identical-shape payers: the paired difference has no noise, so k=1 is adopted.
    assert_eq!(report.adopted_k, 1.0);
    assert!(report.cuts.iter().all(|c| c.cuts == 0));
    assert!(report.cuts[bucket_of(0.025)].years > 0);
}

#[test]
fn the_backtest_is_point_in_time() {
    let base = steady("S", 0.05);
    let mut changed = base.clone();
    // Change only 2025's payments: 2018's origin (outcome years ≤ 2023) must not move.
    for p in changed
        .history
        .iter_mut()
        .filter(|p| p.ex_date.year() == 2025)
    {
        p.per_share *= 3.0;
    }
    let a = backtest(&[base], 2018);
    let b = backtest(&[changed], 2018);
    assert_eq!(a, b);
}

#[test]
fn cuts_are_counted_by_yield_and_blend_with_the_prior() {
    let mut cutter = steady("CUT", 0.0);
    // Halve the payout from 2015 on: 2014 is a cut year at a 2.5% yield.
    for p in cutter
        .history
        .iter_mut()
        .filter(|p| p.ex_date.year() >= 2015)
    {
        p.per_share *= 0.5;
    }
    let report = backtest(&[cutter], 2024);
    let row = &report.cuts[bucket_of(1.0 / 40.0)];
    assert_eq!(row.cuts, 1);
    let blended = cut_hazard(Some(&report), 1.0 / 40.0);
    let prior = PRIOR_CUT_RATE[bucket_of(1.0 / 40.0)];
    assert!(blended != prior);
    assert_eq!(cut_hazard(None, 0.09), PRIOR_CUT_RATE[4]);
    assert_eq!(backtest(&[], 2024).adopted_k, DEFAULT_SHRINK_K);
}

// ---- B4 Monte Carlo ----------------------------------------------------------------

fn holding(key: &str, units: f64, price: f64, yield_: f64, g: f64) -> McHolding {
    McHolding {
        key: key.into(),
        symbol: key.into(),
        units,
        price,
        fx: 1.0,
        history: quarterly(2016, 2026, price * yield_ / (1.0 + g).powi(10), g),
        withholding: 0.15,
        price_growth: Some(0.04),
        price_vol: 0.2,
        fx_vol: 0.0,
        foreign: false,
        candidate: false,
        fund: false,
        safety: SafetyGrade::B,
    }
}

fn inputs(holdings: Vec<McHolding>) -> McInputs {
    McInputs {
        holdings,
        avg_correlation: 0.4,
        calibration: None,
        shrink_k: DEFAULT_SHRINK_K,
        start: date(2027, 1, 1),
    }
}

fn rules(years: u32, contribution: f64) -> Rules {
    Rules {
        years,
        reinvest_pct: 100.0,
        monthly_contribution: contribution,
        reserve_pt_tax: false,
        limits: Limits::default(),
        stress: Default::default(),
    }
}

const PROPORTIONAL: Policy = Policy::Mode {
    allocation: Allocation::Proportional,
    top_n: 5,
};

#[test]
fn the_monte_carlo_is_seeded_and_pure() {
    let inputs = inputs(vec![
        holding("A", 100.0, 50.0, 0.03, 0.05),
        holding("B", 50.0, 80.0, 0.05, 0.02),
    ]);
    let mc = McSettings {
        paths: 300,
        seed: 42,
    };
    let a = simulate(
        &inputs,
        &rules(10, 200.0),
        &PROPORTIONAL,
        mc,
        Some((10, 2_000.0)),
    );
    let b = simulate(
        &inputs,
        &rules(10, 200.0),
        &PROPORTIONAL,
        mc,
        Some((10, 2_000.0)),
    );
    assert_eq!(a, b);
    let c = simulate(
        &inputs,
        &rules(10, 200.0),
        &PROPORTIONAL,
        McSettings { seed: 43, ..mc },
        Some((10, 2_000.0)),
    );
    assert_ne!(a, c);
    assert_eq!(a.years.len(), 10);
    for row in &a.years {
        assert!(row.net.p10 <= row.net.p50 && row.net.p50 <= row.net.p90);
    }
    let p = a.p_target.expect("a target chance");
    assert!((0.0..=1.0).contains(&p));
}

#[test]
fn the_median_path_tracks_the_deterministic_first_year() {
    // No noise in growth (σ floor aside), no contribution: year-1 median income is close
    // to units × trailing payout × (1 − withholding).
    let h = holding("A", 100.0, 50.0, 0.04, 0.0);
    let ttm = growth(&h.history, date(2027, 1, 1)).ttm_per_share;
    let inputs = inputs(vec![h]);
    let out = simulate(
        &inputs,
        &Rules {
            reinvest_pct: 0.0,
            ..rules(1, 0.0)
        },
        &PROPORTIONAL,
        McSettings {
            paths: 500,
            seed: 1,
        },
        None,
    );
    let expected = 100.0 * ttm * 0.85;
    assert!(
        (out.years[0].net.p50 - expected).abs() / expected < 0.02,
        "{} vs {expected}",
        out.years[0].net.p50
    );
}

#[test]
fn more_allowance_means_more_income_on_the_same_paths() {
    let inputs = inputs(vec![holding("A", 100.0, 50.0, 0.04, 0.03)]);
    let mc = McSettings {
        paths: 200,
        seed: 9,
    };
    let low = run(&inputs, &rules(15, 0.0), &PROPORTIONAL, mc);
    let high = run(&inputs, &rules(15, 500.0), &PROPORTIONAL, mc);
    for (a, b) in low.yearly_net.iter().zip(&high.yearly_net) {
        assert!(b[14] >= a[14]);
    }
}

#[test]
fn a_capped_position_stops_receiving_money() {
    let inputs = inputs(vec![
        holding("BIG", 1_000.0, 50.0, 0.04, 0.03),
        holding("SMALL", 10.0, 50.0, 0.04, 0.03),
    ]);
    let mc = McSettings { paths: 50, seed: 3 };
    let weights = Policy::Weights {
        weights: vec![1.0, 0.0],
    };
    let capped = Rules {
        limits: Limits {
            max_position_pct: 50.0,
            ..Limits::default()
        },
        ..rules(5, 1_000.0)
    };
    // All weight on BIG, which is already 99% of the portfolio: with the cap, nothing
    // can be bought, so the money stays as cash and income matches no allowance at all.
    let with_cap = run(&inputs, &capped, &weights, mc);
    let no_money = run(
        &inputs,
        &Rules {
            reinvest_pct: 0.0,
            ..rules(5, 0.0)
        },
        &weights,
        mc,
    );
    assert_eq!(with_cap.yearly_net, no_money.yearly_net);
}

// ---- B5 maximizer ------------------------------------------------------------------

fn search() -> McSettings {
    McSettings {
        paths: 120,
        seed: 11,
    }
}

fn score() -> McSettings {
    McSettings {
        paths: 400,
        seed: 12_345,
    }
}

#[test]
fn the_maximizer_is_pure_and_respects_the_safety_floor() {
    let mut risky = holding("RISKY", 10.0, 20.0, 0.09, 0.0);
    risky.safety = SafetyGrade::E;
    let inputs = inputs(vec![holding("A", 100.0, 50.0, 0.03, 0.06), risky]);
    let objective = Objective::RobustIncome { year: 8 };
    let a = solve(
        &inputs,
        &rules(10, 300.0),
        objective,
        Constraints::default(),
        search(),
        score(),
    );
    let b = solve(
        &inputs,
        &rules(10, 300.0),
        objective,
        Constraints::default(),
        search(),
        score(),
    );
    assert_eq!(a, b);
    assert_eq!(a.excluded, vec!["RISKY".to_string()]);
    assert!(a.weights.iter().all(|w| w.key != "RISKY"));
    assert_eq!(a.scores.len(), 7, "the plan and the six modes");
    assert_eq!(a.objective.year(), 8);
}

#[test]
fn a_good_candidate_can_beat_every_fixed_mode() {
    // One held low-yield name and a much higher-yielding, safe candidate the fixed modes
    // never buy (they only spread over holdings).
    let mut candidate = holding("watch:CAND", 0.0, 40.0, 0.07, 0.03);
    candidate.candidate = true;
    candidate.safety = SafetyGrade::A;
    let inputs = inputs(vec![holding("A", 100.0, 50.0, 0.01, 0.02), candidate]);
    let plan = solve(
        &inputs,
        &rules(15, 1_000.0),
        Objective::RobustIncome { year: 15 },
        Constraints {
            max_position_pct: 100.0,
            ..Constraints::default()
        },
        search(),
        score(),
    );
    assert_eq!(
        plan.recommendation,
        Recommendation::Plan,
        "{:?}",
        plan.notes
    );
    assert_eq!(plan.weights[0].key, "watch:CAND");
    assert!(
        plan.this_month
            .iter()
            .any(|b| b.candidate && b.amount > 0.0)
    );
    let total: f64 = plan.this_month.iter().map(|b| b.amount).sum();
    assert!((total - 1_000.0).abs() < 1.0);
}

#[test]
fn with_nothing_to_gain_it_recommends_a_fixed_mode() {
    // Two identical holdings: no weighting can clearly beat a mode.
    let inputs = inputs(vec![
        holding("A", 100.0, 50.0, 0.03, 0.04),
        holding("B", 100.0, 50.0, 0.03, 0.04),
    ]);
    let plan = solve(
        &inputs,
        &rules(10, 200.0),
        Objective::RobustIncome { year: 10 },
        Constraints {
            max_position_pct: 100.0,
            ..Constraints::default()
        },
        search(),
        score(),
    );
    assert!(matches!(plan.recommendation, Recommendation::Mode { .. }));
    assert!(plan.this_month.is_empty());
}

#[test]
fn the_wealth_floor_rejects_plans_that_give_up_too_much_value() {
    let inputs = inputs(vec![
        holding("GROW", 100.0, 50.0, 0.005, 0.10),
        holding("YIELD", 100.0, 50.0, 0.11, -0.02),
    ]);
    let plan = solve(
        &inputs,
        &rules(15, 500.0),
        Objective::IncomeWithWealthFloor {
            year: 15,
            floor_pct: 100.0,
        },
        Constraints {
            max_position_pct: 100.0,
            ..Constraints::default()
        },
        search(),
        score(),
    );
    let proportional = plan
        .scores
        .iter()
        .find(|s| s.policy == "proportional")
        .expect("proportional score");
    if let Some(plan_score) = plan.scores.iter().find(|s| s.policy == "plan")
        && plan_score.objective.is_some()
    {
        assert!(plan_score.value_p50 >= proportional.value_p50 * 0.97);
    }
}
