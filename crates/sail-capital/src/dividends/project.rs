//! The monthly dividend projection: three scenarios, same-month reinvestment, year rows
//! and the comparison without reinvestment.

use chrono::Datelike;

use super::growth::growth;
use super::{
    Allocation, DividendOutlook, Growth, Holding, HoldingRow, MAX_GROWTH, MAX_YEARS, PT_TAX,
    Projection, Scenario, Settings, YearRow,
};

/// One holding's mutable state while a scenario is simulated.
struct SimHolding<'a> {
    /// The inputs.
    holding: &'a Holding,
    /// The estimated growth and payment schedule.
    growth: &'a Growth,
    /// Scenario growth for this holding, already capped.
    g: f64,
    /// Units held now.
    units: f64,
    /// Whether the projection gives this holding a payment schedule.
    scheduled: bool,
    /// Whether the holding has any trailing payment (it appears in the holdings table).
    paying: bool,
    /// Net income booked in the current year.
    net_year: f64,
    /// Net income in year 1.
    first_year_net: f64,
    /// Net income in the most recently completed year.
    last_year_net: f64,
}

/// What to simulate.
struct SimConfig {
    /// Low, base or high.
    scenario: Scenario,
    /// Share of each net payment reinvested.
    reinvest_pct: f64,
    /// Cost of the starting holdings, for yield on cost.
    original_cost: Option<f64>,
}

/// Per-holding results of the base scenario.
struct HoldingStat {
    /// Index into the caller's holdings.
    index: usize,
    /// Net income in year 1.
    first_year_net: f64,
    /// Net income in the last year.
    last_year_net: f64,
    /// Units at the end.
    end_units: f64,
}

/// Totals for one month or year.
#[derive(Default)]
struct Totals {
    /// Gross dividends.
    gross: f64,
    /// Withheld at source.
    withheld: f64,
    /// Portuguese tax reserve.
    pt_tax: f64,
    /// Net after withholding and reserve.
    net: f64,
    /// Reinvested.
    reinvested: f64,
    /// Kept as cash.
    cash: f64,
}

/// Projects every scenario. `original_cost` is what the starting holdings cost in the
/// account currency, for yield on cost.
pub fn project(
    holdings: &[Holding],
    settings: &Settings,
    original_cost: Option<f64>,
) -> DividendOutlook {
    let growths: Vec<Growth> = holdings
        .iter()
        .map(|holding| growth(&holding.history, settings.start))
        .collect();

    let (low, _) = simulate(
        holdings,
        &growths,
        settings,
        &SimConfig {
            scenario: Scenario::Low,
            reinvest_pct: settings.reinvest_pct,
            original_cost,
        },
    );
    let (base, stats) = simulate(
        holdings,
        &growths,
        settings,
        &SimConfig {
            scenario: Scenario::Base,
            reinvest_pct: settings.reinvest_pct,
            original_cost,
        },
    );
    let (high, _) = simulate(
        holdings,
        &growths,
        settings,
        &SimConfig {
            scenario: Scenario::High,
            reinvest_pct: settings.reinvest_pct,
            original_cost,
        },
    );
    let (without_reinvestment, _) = simulate(
        holdings,
        &growths,
        settings,
        &SimConfig {
            scenario: Scenario::Base,
            reinvest_pct: 0.0,
            original_cost,
        },
    );

    let holding_rows: Vec<HoldingRow> = stats
        .iter()
        .map(|stat| {
            let holding = &holdings[stat.index];
            HoldingRow {
                symbol: holding.symbol.clone(),
                first_year_net: stat.first_year_net,
                last_year_net: stat.last_year_net,
                end_units: stat.end_units,
                growth: growths[stat.index].clone(),
                withholding: (holding.withholding, holding.withholding_source.clone()),
            }
        })
        .collect();

    let non_paying: Vec<String> = holdings
        .iter()
        .zip(&growths)
        .filter(|(_, growth)| growth.ttm_per_share <= 0.0)
        .map(|(holding, _)| holding.symbol.clone())
        .collect();

    let notes = notes(holdings, &growths, settings);

    DividendOutlook {
        settings: settings.clone(),
        scenarios: vec![low, base, high],
        holdings: holding_rows,
        non_paying,
        without_reinvestment,
        notes,
    }
}

/// Simulates one scenario, returning its projection and the per-holding base statistics.
fn simulate(
    holdings: &[Holding],
    growths: &[Growth],
    settings: &Settings,
    config: &SimConfig,
) -> (Projection, Vec<HoldingStat>) {
    let years = settings.years.clamp(1, MAX_YEARS);
    let months_total = years * 12;
    let pct = config.reinvest_pct.clamp(0.0, 100.0);
    let mut sims: Vec<SimHolding<'_>> = holdings
        .iter()
        .zip(growths)
        .map(|(holding, growth)| {
            let g = scenario_growth(growth, config.scenario);
            SimHolding {
                holding,
                growth,
                g,
                units: holding.units,
                scheduled: growth.ttm_per_share > 0.0 && !growth.months.is_empty(),
                paying: growth.ttm_per_share > 0.0,
                net_year: 0.0,
                first_year_net: 0.0,
                last_year_net: 0.0,
            }
        })
        .collect();

    let mut rows: Vec<YearRow> = Vec::with_capacity(years as usize);
    let mut monthly_net: Vec<f64> = Vec::with_capacity(months_total as usize);
    let mut year_totals = Totals::default();

    for month_index in 0..months_total {
        let elapsed = month_index as f64 / 12.0;
        let month_of_year = (settings.start.month0() + month_index) % 12 + 1;
        let prices: Vec<f64> = sims
            .iter()
            .map(|sim| sim.holding.price * (1.0 + sim.g).powf(elapsed))
            .collect();

        let mut month = Totals::default();
        let mut reinvested: Vec<f64> = vec![0.0; sims.len()];

        for (index, sim) in sims.iter_mut().enumerate() {
            if !sim.scheduled || !sim.growth.months.contains(&month_of_year) {
                continue;
            }
            let per_share = sim.growth.ttm_per_share / sim.growth.months.len() as f64
                * (1.0 + sim.g).powf(elapsed);
            let gross = sim.units * per_share * sim.holding.fx;
            let withheld = gross * sim.holding.withholding.clamp(0.0, 1.0);
            let pt_tax = if settings.reserve_pt_tax {
                (PT_TAX * gross - withheld).max(0.0)
            } else {
                0.0
            };
            let net = gross - withheld - pt_tax;
            let amount = net * pct / 100.0;

            reinvested[index] = amount;
            sim.net_year += net;
            month.gross += gross;
            month.withheld += withheld;
            month.pt_tax += pt_tax;
            month.net += net;
            month.reinvested += amount;
            month.cash += net - amount;
        }

        if pct > 0.0 {
            reinvest(&mut sims, &prices, &reinvested, settings.allocation);
        }

        monthly_net.push(month.net);
        year_totals.gross += month.gross;
        year_totals.withheld += month.withheld;
        year_totals.pt_tax += month.pt_tax;
        year_totals.net += month.net;
        year_totals.reinvested += month.reinvested;
        year_totals.cash += month.cash;

        if (month_index + 1) % 12 == 0 {
            let year = (month_index + 1) / 12;
            for sim in &mut sims {
                if year == 1 {
                    sim.first_year_net = sim.net_year;
                }
                sim.last_year_net = sim.net_year;
                sim.net_year = 0.0;
            }

            let end_elapsed = (month_index + 1) as f64 / 12.0;
            let value: f64 = sims
                .iter()
                .filter(|sim| sim.scheduled)
                .map(|sim| {
                    let price = sim.holding.price * (1.0 + sim.g).powf(end_elapsed);
                    sim.units * price * sim.holding.fx
                })
                .sum();
            let yield_on_cost = config
                .original_cost
                .and_then(|cost| (cost > 0.0).then(|| year_totals.net / cost));

            rows.push(YearRow {
                year,
                gross: year_totals.gross,
                withheld: year_totals.withheld,
                pt_tax: year_totals.pt_tax,
                net: year_totals.net,
                reinvested: year_totals.reinvested,
                cash: year_totals.cash,
                value,
                yield_on_cost,
            });
            year_totals = Totals::default();
        }
    }

    let first_year_net = rows.first().map(|row| row.net).unwrap_or(0.0);
    let doubling_year = if first_year_net > 0.0 {
        rows.iter()
            .find(|row| row.net >= 2.0 * first_year_net)
            .map(|row| row.year)
    } else {
        None
    };

    let stats = sims
        .iter()
        .enumerate()
        .filter(|(_, sim)| sim.paying)
        .map(|(index, sim)| HoldingStat {
            index,
            first_year_net: sim.first_year_net,
            last_year_net: sim.last_year_net,
            end_units: sim.units,
        })
        .collect();

    (
        Projection {
            scenario: config.scenario,
            years: rows,
            monthly_net,
            doubling_year,
        },
        stats,
    )
}

/// Buys units with the month's reinvestment.
fn reinvest(
    sims: &mut [SimHolding<'_>],
    prices: &[f64],
    reinvested: &[f64],
    allocation: Allocation,
) {
    match allocation {
        Allocation::SameHolding => {
            for (sim, (price, amount)) in sims.iter_mut().zip(prices.iter().zip(reinvested)) {
                if !sim.scheduled || *amount <= 0.0 {
                    continue;
                }
                let unit_price = price * sim.holding.fx;
                if unit_price > 0.0 {
                    sim.units += amount / unit_price;
                }
            }
        }
        Allocation::Proportional => {
            let total: f64 = reinvested.iter().sum();
            let values: Vec<f64> = sims
                .iter()
                .zip(prices)
                .map(|(sim, price)| {
                    if sim.scheduled {
                        sim.units * price * sim.holding.fx
                    } else {
                        0.0
                    }
                })
                .collect();
            let total_value: f64 = values.iter().sum();
            if total <= 0.0 || total_value <= 0.0 {
                return;
            }
            for (sim, (price, value)) in sims.iter_mut().zip(prices.iter().zip(&values)) {
                if !sim.scheduled {
                    continue;
                }
                let unit_price = price * sim.holding.fx;
                if unit_price > 0.0 {
                    sim.units += (total * value / total_value) / unit_price;
                }
            }
        }
    }
}

/// The scenario's growth for one holding, capped.
fn scenario_growth(growth: &Growth, scenario: Scenario) -> f64 {
    let value = match scenario {
        Scenario::Low => growth.g - growth.sigma,
        Scenario::Base => growth.g,
        Scenario::High => growth.g + growth.sigma,
    };
    value.clamp(-MAX_GROWTH, MAX_GROWTH)
}

/// Whether the history shows a year with no payment after a paying year.
fn cut(growth: &Growth) -> bool {
    growth
        .annual
        .iter()
        .skip(1)
        .any(|(_, amount)| *amount <= 0.0)
}

/// Plain notes on the assumptions and limits.
fn notes(holdings: &[Holding], growths: &[Growth], settings: &Settings) -> Vec<String> {
    let mut notes = vec![
        "Projection from past dividend payouts; dividends can be cut, suspended or resumed."
            .to_string(),
        "Scenarios are not probabilities: the low and high cases shift each holding's growth by its own payout volatility."
            .to_string(),
        "Prices grow with each holding's projected dividend growth, so the starting yield is held constant."
            .to_string(),
        "FX is held at today's rate for the whole projection.".to_string(),
        "Withholding is the rate observed in the ledger over the last 24 months where available, otherwise a default by listing country."
            .to_string(),
    ];
    if settings.reserve_pt_tax {
        notes.push(
            "Portuguese tax is simplified: 28% of gross minus foreign withholding, floored at zero; no other credits or deductions are modelled."
                .to_string(),
        );
    }
    if !growths
        .iter()
        .any(|growth| growth.ttm_per_share > 0.0 && !growth.months.is_empty())
    {
        notes.push(
            "No holding has a payment schedule in the last complete year, so the projection shows no dividend income."
                .to_string(),
        );
    }
    for (holding, growth) in holdings.iter().zip(growths) {
        if cut(growth) {
            notes.push(format!(
                "{} had a year with no payment after a paying year, so its base growth is capped at {:.0}%.",
                holding.symbol,
                -MAX_GROWTH * 100.0
            ));
        }
    }
    notes.push(
        "Reinvestment assumes fractional units, no commissions and no price impact.".to_string(),
    );
    notes
}
