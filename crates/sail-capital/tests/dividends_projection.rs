//! Dividend projection (Kilo's slice, round 14): synthetic holdings and histories only,
//! no files, no clock and no network.

use chrono::NaiveDate;
use sail_capital::dividends::{
    Allocation, DividendOutlook, Holding, Payment, Projection, Scenario, Settings, project,
};

const TOLERANCE: f64 = 1e-9;

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
}

fn settings(
    years: u32,
    reinvest_pct: f64,
    allocation: Allocation,
    reserve_pt_tax: bool,
) -> Settings {
    Settings {
        years,
        reinvest_pct,
        allocation,
        reserve_pt_tax,
        start: date(2027, 1, 1),
    }
}

/// One payment a year in June, amounts growing by `rate` from 2021 through 2026, which is
/// the last complete year before the projection starts.
fn steady_history(rate: f64) -> Vec<Payment> {
    (0..6)
        .map(|index| Payment {
            ex_date: date(2021 + index, 6, 15),
            per_share: (1.0 + rate).powi(index),
        })
        .collect()
}

fn holding(symbol: &str, units: f64, price: f64, history: Vec<Payment>) -> Holding {
    Holding {
        key: symbol.to_ascii_lowercase(),
        symbol: symbol.to_string(),
        units,
        price,
        fx: 1.0,
        history,
        withholding: 0.0,
        withholding_source: "default".to_string(),
    }
}

/// A holding that pays `ttm` once, in June 2026.
fn annual_payer(symbol: &str, units: f64, price: f64, ttm: f64) -> Holding {
    holding(
        symbol,
        units,
        price,
        vec![Payment {
            ex_date: date(2026, 6, 15),
            per_share: ttm,
        }],
    )
}

fn scenario(outlook: &DividendOutlook, scenario: Scenario) -> &Projection {
    outlook
        .scenarios
        .iter()
        .find(|projection| projection.scenario == scenario)
        .expect("every scenario is present")
}

fn holding_row<'a>(
    outlook: &'a DividendOutlook,
    symbol: &str,
) -> &'a sail_capital::dividends::HoldingRow {
    outlook
        .holdings
        .iter()
        .find(|row| row.symbol == symbol)
        .expect("the paying holding has a row")
}

fn assert_close(actual: f64, expected: f64, label: &str) {
    assert!(
        (actual - expected).abs() <= TOLERANCE,
        "{label}: {actual} differs from {expected}"
    );
}

#[test]
fn same_holding_reinvestment_compounds_to_the_closed_form() {
    let outlook = project(
        &[annual_payer("AAA", 100.0, 10.0, 1.0)],
        &settings(10, 100.0, Allocation::SameHolding, false),
        None,
    );
    let base = scenario(&outlook, Scenario::Base);
    assert_eq!(base.years.len(), 10);
    assert_eq!(base.monthly_net.len(), 120);
    for (index, row) in base.years.iter().enumerate() {
        assert_close(
            row.net,
            100.0 * 1.1f64.powi(index as i32),
            "annual net income",
        );
    }

    let row = holding_row(&outlook, "AAA");
    assert_close(row.end_units, 100.0 * 1.1f64.powi(10), "end units");
    assert_close(
        base.years[9].value,
        100.0 * 1.1f64.powi(10) * 10.0,
        "year-end value",
    );
}

#[test]
fn no_reinvestment_keeps_units_and_income_flat_without_growth() {
    let outlook = project(
        &[annual_payer("AAA", 100.0, 10.0, 1.0)],
        &settings(5, 0.0, Allocation::SameHolding, false),
        None,
    );
    let base = scenario(&outlook, Scenario::Base);

    for row in &base.years {
        assert_close(row.net, 100.0, "flat net income");
        assert_close(row.value, 1000.0, "flat value");
        assert_close(row.reinvested, 0.0, "nothing reinvested");
        assert_close(row.cash, 100.0, "everything kept as cash");
    }
    assert!(base.doubling_year.is_none());
    assert_close(holding_row(&outlook, "AAA").end_units, 100.0, "end units");
}

#[test]
fn proportional_reinvestment_keeps_value_shares_at_equal_growth() {
    // Both start at 1,000 in value but pay different yields (10% and 5%).
    let outlook = project(
        &[
            annual_payer("AAA", 100.0, 10.0, 1.0),
            annual_payer("BBB", 50.0, 20.0, 1.0),
        ],
        &settings(10, 100.0, Allocation::Proportional, false),
        None,
    );

    let growth = 1.075f64.powi(10);
    let aaa = holding_row(&outlook, "AAA");
    let bbb = holding_row(&outlook, "BBB");
    assert_close(aaa.end_units, 100.0 * growth, "AAA end units");
    assert_close(bbb.end_units, 50.0 * growth, "BBB end units");
    assert_close(aaa.end_units / bbb.end_units, 2.0, "value share ratio");
    assert_close(aaa.first_year_net, 100.0, "AAA year-1 net");
    assert_close(bbb.first_year_net, 50.0, "BBB year-1 net");
}

#[test]
fn withholding_and_the_portuguese_reserve_are_subtracted_in_order() {
    let mut payer = annual_payer("AAA", 100.0, 10.0, 1.0);
    payer.withholding = 0.15;
    payer.withholding_source = "observed".to_string();

    let reserved = project(
        std::slice::from_ref(&payer),
        &settings(1, 0.0, Allocation::SameHolding, true),
        None,
    );
    let row = &scenario(&reserved, Scenario::Base).years[0];
    assert_close(row.gross, 100.0, "gross");
    assert_close(row.withheld, 15.0, "withheld");
    assert_close(row.pt_tax, 13.0, "Portuguese reserve");
    assert_close(row.net, 72.0, "net after both taxes");
    assert!(
        reserved
            .notes
            .iter()
            .any(|note| note.contains("Portuguese tax"))
    );

    let unreserved = project(
        &[payer],
        &settings(1, 0.0, Allocation::SameHolding, false),
        None,
    );
    let row = &scenario(&unreserved, Scenario::Base).years[0];
    assert_close(row.withheld, 15.0, "withheld");
    assert_close(row.pt_tax, 0.0, "no reserve");
    assert_close(row.net, 85.0, "net after withholding only");
}

#[test]
fn low_base_and_high_income_are_ordered_in_every_year() {
    let payer = holding("AAA", 100.0, 25.0, steady_history(0.10));
    let outlook = project(
        &[payer],
        &settings(10, 100.0, Allocation::Proportional, false),
        Some(2500.0),
    );
    let low = scenario(&outlook, Scenario::Low);
    let base = scenario(&outlook, Scenario::Base);
    let high = scenario(&outlook, Scenario::High);

    for year in 0..10 {
        assert!(low.years[year].net <= base.years[year].net + TOLERANCE);
        assert!(base.years[year].net <= high.years[year].net + TOLERANCE);
    }
    let yield_on_cost = base.years[0].yield_on_cost.expect("cost is known");
    assert_close(yield_on_cost, base.years[0].net / 2500.0, "yield on cost");
}

#[test]
fn doubling_year_is_the_first_year_at_twice_year_one() {
    let outlook = project(
        &[annual_payer("AAA", 100.0, 10.0, 1.0)],
        &settings(12, 100.0, Allocation::SameHolding, false),
        None,
    );
    let base = scenario(&outlook, Scenario::Base);
    assert_eq!(base.doubling_year, Some(9));

    let flat = project(
        &[annual_payer("AAA", 100.0, 10.0, 1.0)],
        &settings(12, 0.0, Allocation::SameHolding, false),
        None,
    );
    assert!(flat.without_reinvestment.doubling_year.is_none());
}

#[test]
fn the_projection_is_deterministic() {
    let holdings = [
        annual_payer("AAA", 100.0, 10.0, 1.0),
        holding("BBB", 20.0, 30.0, steady_history(0.05)),
    ];
    let settings = settings(8, 60.0, Allocation::Proportional, true);
    let first = project(&holdings, &settings, Some(1600.0));
    let second = project(&holdings, &settings, Some(1600.0));
    assert_eq!(first, second);
}

#[test]
fn holdings_without_a_payment_are_listed_and_earn_nothing() {
    let outlook = project(
        &[holding("FUND", 10.0, 5.0, Vec::new())],
        &settings(5, 100.0, Allocation::Proportional, false),
        None,
    );

    assert!(outlook.holdings.is_empty());
    assert_eq!(outlook.non_paying, vec!["FUND".to_string()]);
    for projection in &outlook.scenarios {
        assert_eq!(projection.years.len(), 5);
        assert_eq!(projection.monthly_net.len(), 60);
        assert!(projection.years.iter().all(|row| row.net == 0.0));
        assert!(projection.years.iter().all(|row| row.value == 0.0));
        assert!(projection.doubling_year.is_none());
    }
    assert!(
        outlook
            .notes
            .iter()
            .any(|note| note.contains("no dividend income"))
    );
}

#[test]
fn holdings_rows_report_the_inputs_used() {
    let mut payer = annual_payer("AAA", 100.0, 10.0, 2.5);
    payer.withholding = 0.15;
    payer.withholding_source = "observed".to_string();
    let outlook = project(
        &[payer, holding("FUND", 10.0, 5.0, Vec::new())],
        &settings(1, 100.0, Allocation::SameHolding, false),
        None,
    );

    assert_eq!(outlook.holdings.len(), 1);
    assert_eq!(outlook.non_paying, vec!["FUND".to_string()]);
    let row = holding_row(&outlook, "AAA");
    assert_close(row.growth.ttm_per_share, 2.5, "TTM per share");
    assert_close(row.growth.g, 0.0, "growth");
    assert_close(row.withholding.0, 0.15, "withholding");
    assert_eq!(row.withholding.1, "observed");
}

#[test]
fn one_and_thirty_years_both_project_the_right_length() {
    let payer = annual_payer("AAA", 100.0, 10.0, 1.0);
    let one = project(
        std::slice::from_ref(&payer),
        &settings(1, 100.0, Allocation::SameHolding, false),
        None,
    );
    assert_eq!(scenario(&one, Scenario::Base).years.len(), 1);
    assert_eq!(scenario(&one, Scenario::Base).monthly_net.len(), 12);

    let thirty = project(
        &[payer],
        &settings(30, 100.0, Allocation::SameHolding, false),
        None,
    );
    assert_eq!(scenario(&thirty, Scenario::Base).years.len(), 30);
    assert_eq!(scenario(&thirty, Scenario::Base).monthly_net.len(), 360);
}

#[test]
fn notes_state_the_assumptions_and_any_cut() {
    let outlook = project(
        &[holding("AAA", 100.0, 10.0, steady_history(0.10))],
        &settings(3, 100.0, Allocation::Proportional, true),
        None,
    );
    let text = outlook.notes.join(" ");
    assert!(text.contains("Projection from past dividend payouts"));
    assert!(text.contains("not probabilities"));
    assert!(text.contains("FX"));
    assert!(text.contains("Portuguese tax"));

    let cut = project(
        &[holding(
            "CUT",
            100.0,
            10.0,
            vec![
                Payment {
                    ex_date: date(2023, 6, 15),
                    per_share: 1.0,
                },
                Payment {
                    ex_date: date(2024, 6, 15),
                    per_share: 1.0,
                },
            ],
        )],
        &settings(3, 100.0, Allocation::SameHolding, false),
        None,
    );
    assert!(
        cut.notes
            .iter()
            .any(|note| note.contains("no payment after a paying year"))
    );
}
