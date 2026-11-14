//! Dividend projection (Kilo's slice, round 14): synthetic holdings and histories only,
//! no files, no clock and no network.

use chrono::NaiveDate;
use sail_capital::dividends::{
    Allocation, DEFAULT_TOP_N, DividendOutlook, Holding, Payment, Projection, Scenario, Settings,
    project,
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
    settings_top(
        years,
        reinvest_pct,
        allocation,
        DEFAULT_TOP_N,
        reserve_pt_tax,
    )
}

fn settings_top(
    years: u32,
    reinvest_pct: f64,
    allocation: Allocation,
    top_n: u32,
    reserve_pt_tax: bool,
) -> Settings {
    Settings {
        years,
        reinvest_pct,
        allocation,
        top_n,
        monthly_contribution: 0.0,
        reserve_pt_tax,
        start: date(2027, 1, 1),
    }
}

fn settings_with_contribution(
    years: u32,
    reinvest_pct: f64,
    allocation: Allocation,
    monthly_contribution: f64,
) -> Settings {
    Settings {
        years,
        reinvest_pct,
        allocation,
        top_n: DEFAULT_TOP_N,
        monthly_contribution,
        reserve_pt_tax: false,
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
        price_growth: None,
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

fn three_payers() -> Vec<Holding> {
    vec![
        annual_payer("AAA", 100.0, 10.0, 1.0),
        annual_payer("BBB", 50.0, 10.0, 1.0),
        annual_payer("CCC", 25.0, 10.0, 1.0),
    ]
}

#[test]
fn top_value_reinvestment_buys_only_the_largest_holdings() {
    let outlook = project(
        &three_payers(),
        &settings_top(1, 100.0, Allocation::TopValue, 1, false),
        None,
    );

    // Year 1 pays 100 + 50 + 25; the largest holding takes the whole 175.
    assert_close(holding_row(&outlook, "AAA").end_units, 117.5, "AAA units");
    assert_close(holding_row(&outlook, "BBB").end_units, 50.0, "BBB units");
    assert_close(holding_row(&outlook, "CCC").end_units, 25.0, "CCC units");

    let two = project(
        &three_payers(),
        &settings_top(1, 100.0, Allocation::TopValue, 2, false),
        None,
    );
    assert_close(
        holding_row(&two, "AAA").end_units,
        111.666_666_666_7,
        "AAA units",
    );
    assert_close(
        holding_row(&two, "BBB").end_units,
        55.833_333_333_3,
        "BBB units",
    );
    assert_close(holding_row(&two, "CCC").end_units, 25.0, "CCC units");
}

#[test]
fn equal_weight_reinvestment_splits_the_pool_evenly() {
    let outlook = project(
        &three_payers(),
        &settings_top(1, 100.0, Allocation::EqualWeight, 5, false),
        None,
    );

    let each_units = 175.0 / 3.0 / 10.0;
    assert_close(
        holding_row(&outlook, "AAA").end_units,
        100.0 + each_units,
        "AAA units",
    );
    assert_close(
        holding_row(&outlook, "BBB").end_units,
        50.0 + each_units,
        "BBB units",
    );
    assert_close(
        holding_row(&outlook, "CCC").end_units,
        25.0 + each_units,
        "CCC units",
    );
}

#[test]
fn top_yield_reinvestment_picks_the_highest_yielders() {
    let outlook = project(
        &[
            annual_payer("AAA", 100.0, 10.0, 1.0),
            annual_payer("BBB", 50.0, 10.0, 1.1),
            annual_payer("CCC", 25.0, 10.0, 0.5),
        ],
        &settings_top(1, 100.0, Allocation::TopYield, 1, false),
        None,
    );

    // BBB pays 55 and yields 11%; it takes the whole 167.5 pool.
    assert_close(holding_row(&outlook, "AAA").end_units, 100.0, "AAA units");
    assert_close(holding_row(&outlook, "BBB").end_units, 66.75, "BBB units");
    assert_close(holding_row(&outlook, "CCC").end_units, 25.0, "CCC units");
}

#[test]
fn underweight_reinvestment_moves_toward_equal_weight() {
    let outlook = project(
        &three_payers(),
        &settings_top(1, 100.0, Allocation::Underweight, 5, false),
        None,
    );

    // 175 is split by the shortfall below the 583.33 equal-weight target: 35 to BBB
    // and 140 to CCC.
    assert_close(holding_row(&outlook, "AAA").end_units, 100.0, "AAA units");
    assert_close(holding_row(&outlook, "BBB").end_units, 53.5, "BBB units");
    assert_close(holding_row(&outlook, "CCC").end_units, 39.0, "CCC units");
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

#[test]
fn a_high_yield_fund_with_a_falling_price_cannot_compound_at_its_headline_yield() {
    // 90% yield on today's price, paid once a year; the price trends down 40% a year.
    let mut fund = annual_payer("HIGH", 100.0, 10.0, 9.0);
    fund.price_growth = Some(-0.4);
    let outlook = project(
        &[fund],
        &settings(10, 100.0, Allocation::SameHolding, false),
        None,
    );
    let base = scenario(&outlook, Scenario::Base);
    // Total return is capped at 8%: the price falls at least 90% − 8% = 82% a year, so
    // units grow ×1.9 while the payout per unit falls ×0.18, and income shrinks.
    let ratio = base.years[9].net / base.years[0].net;
    assert!(ratio < 1.0, "{ratio}");
    // Its value after ten years can't exceed an 8% a year total return.
    assert!(
        base.years[9].value <= 1000.0 * 1.08f64.powi(10) * 1.01,
        "{}",
        base.years[9].value
    );
    assert!(outlook.notes.iter().any(|n| n.contains("HIGH yields 90%")));

    // A normal payer with a measured price trend keeps growing its payout on its own.
    let mut normal = annual_payer("NORM", 100.0, 10.0, 0.5);
    normal.price_growth = Some(0.05);
    let outlook = project(
        &[normal],
        &settings(5, 0.0, Allocation::SameHolding, false),
        None,
    );
    let base = scenario(&outlook, Scenario::Base);
    assert_close(
        base.years[4].net,
        base.years[0].net,
        "flat payout at 0% growth",
    );
    assert_close(
        base.years[4].value,
        100.0 * 10.0 * 1.05f64.powf(5.0),
        "value follows the price trend",
    );
}

#[test]
fn a_falling_price_pulls_a_growing_payout_down_instead_of_exploding() {
    // Payouts grew 20% a year (capped to 15%) while the price trend is -30% a year: left
    // alone, the yield would compound by about 1.6x a year and DRIP income would explode.
    let mut falling = holding("FALL", 100.0, 100.0, steady_history(0.20));
    falling.price_growth = Some(-0.30);
    let ttm = falling.history.last().unwrap().per_share;
    let today_yield = ttm / falling.price;
    let outlook = project(
        &[falling],
        &settings(30, 100.0, Allocation::SameHolding, false),
        None,
    );
    let base = scenario(&outlook, Scenario::Base);
    for row in &base.years {
        // Income can never exceed the cap times the holding's value at the start of the
        // year it pays from; the year-end value is a safe upper bound for that.
        let cap = today_yield.max(sail_capital::dividends::HIGH_YIELD);
        assert!(row.net.is_finite(), "year {} is not finite", row.year);
        assert!(
            row.gross <= row.value * cap / (1.0 - 0.30) + 1e-6,
            "year {}: gross {} against value {}",
            row.year,
            row.gross,
            row.value
        );
    }
    let last = base.years.last().unwrap();
    assert!(
        last.net < base.years[0].net * 50.0,
        "year 30 income {} ran away from year 1 {}",
        last.net,
        base.years[0].net
    );
}

#[test]
fn monthly_allowance_buys_units_every_month_without_being_a_dividend() {
    // 100 units at 10; no dividend reinvestment; 120 a month buys 12 units a month.
    // The June payment uses the units held before June's allowance is invested.
    let outlook = project(
        &[annual_payer("AAA", 100.0, 10.0, 1.0)],
        &settings_with_contribution(2, 0.0, Allocation::SameHolding, 120.0),
        Some(1000.0),
    );
    let base = scenario(&outlook, Scenario::Base);

    assert_close(base.years[0].net, 160.0, "year 1 net");
    assert_close(base.years[0].contributed, 1440.0, "year 1 contributed");
    assert_close(base.years[0].reinvested, 0.0, "no dividend reinvestment");
    assert_close(base.years[0].cash, 160.0, "dividends stay as cash");
    assert_close(base.years[0].value, 2440.0, "year 1 value");
    assert_close(base.years[1].net, 304.0, "year 2 net");
    assert_close(base.years[1].contributed, 1440.0, "year 2 contributed");
    assert_close(base.years[1].value, 3880.0, "year 2 value");
    assert_close(
        base.years[0].yield_on_cost.expect("cost is known"),
        160.0 / 2440.0,
        "yield on cost includes the allowance",
    );

    let row = holding_row(&outlook, "AAA");
    assert_close(row.end_units, 388.0, "end units");
    assert!(
        outlook
            .notes
            .iter()
            .any(|note| note.contains("A monthly allowance of 120.00"))
    );
}

#[test]
fn the_comparison_without_reinvestment_still_invests_the_allowance() {
    let outlook = project(
        &[annual_payer("AAA", 100.0, 10.0, 1.0)],
        &settings_with_contribution(1, 100.0, Allocation::SameHolding, 120.0),
        None,
    );
    let with = scenario(&outlook, Scenario::Base);
    let without = &outlook.without_reinvestment;

    assert!(with.years[0].reinvested > 0.0);
    assert_close(without.years[0].reinvested, 0.0, "nothing reinvested");
    assert_close(without.years[0].contributed, 1440.0, "allowance invested");
    assert_close(
        without.years[0].value,
        2440.0,
        "the allowance still buys units",
    );
    assert!(without.years[0].value < with.years[0].value);
}

#[test]
fn a_zero_allowance_changes_nothing() {
    let holdings = [annual_payer("AAA", 100.0, 10.0, 1.0)];
    for allocation in [
        Allocation::Proportional,
        Allocation::SameHolding,
        Allocation::TopValue,
        Allocation::EqualWeight,
        Allocation::TopYield,
        Allocation::Underweight,
    ] {
        let zero = project(
            &holdings,
            &settings_with_contribution(5, 100.0, allocation, 0.0),
            Some(1000.0),
        );
        let old = project(
            &holdings,
            &settings(5, 100.0, allocation, false),
            Some(1000.0),
        );
        assert_eq!(zero, old, "{allocation:?}");
    }
}

#[test]
fn the_same_holding_mode_spreads_the_allowance_by_value() {
    // Two holdings worth 1,000 and 500. The allowance is split 2:1, while dividends
    // would stay in the holding that paid them.
    let outlook = project(
        &[
            annual_payer("AAA", 100.0, 10.0, 1.0),
            annual_payer("BBB", 50.0, 10.0, 1.0),
        ],
        &settings_with_contribution(1, 0.0, Allocation::SameHolding, 120.0),
        None,
    );

    assert_close(
        holding_row(&outlook, "AAA").end_units,
        196.0,
        "AAA units from allowance",
    );
    assert_close(
        holding_row(&outlook, "BBB").end_units,
        98.0,
        "BBB units from allowance",
    );
}
