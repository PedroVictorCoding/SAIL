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
        let prices: Vec<f64> = sims.iter().map(|sim| price_at(sim, elapsed)).collect();

        let mut month = Totals::default();
        let mut reinvested: Vec<f64> = vec![0.0; sims.len()];

        for (index, sim) in sims.iter_mut().enumerate() {
            if !sim.scheduled || !sim.growth.months.contains(&month_of_year) {
                continue;
            }
            let per_share = sim.growth.ttm_per_share / sim.growth.months.len() as f64
                * if high_yield(sim) {
                    // Pays a constant share of its current price.
                    price_at(sim, elapsed) / sim.holding.price
                } else {
                    (1.0 + sim.g).powf(elapsed)
                };
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
            month.cash += (net - amount).max(0.0);
        }

        if pct > 0.0 {
            reinvest(
                &mut sims,
                &prices,
                &reinvested,
                settings.allocation,
                settings.top_n,
                elapsed,
            );
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
                .map(|sim| sim.units * price_at(sim, end_elapsed) * sim.holding.fx)
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

/// The price after `elapsed` years: the measured trend when known, else the dividend
/// growth (constant yield).
fn price_at(sim: &SimHolding<'_>, elapsed: f64) -> f64 {
    let mut g = sim.holding.price_growth.unwrap_or(sim.g);
    if high_yield(sim) {
        let ttm_yield = sim.growth.ttm_per_share / sim.holding.price;
        g = g
            .min(-(ttm_yield - super::HIGH_YIELD_TOTAL_RETURN))
            .max(-0.9);
    }
    sim.holding.price * (1.0 + g).powf(elapsed)
}

/// Yields more than [`super::HIGH_YIELD`] on today's price.
fn high_yield(sim: &SimHolding<'_>) -> bool {
    sim.holding.price > 0.0 && sim.growth.ttm_per_share / sim.holding.price > super::HIGH_YIELD
}

/// Buys units with the month's reinvestment. The top-N, equal-weight and underweight
/// modes spread every month's pool across the paying holdings, not only the ones that
/// paid this month.
fn reinvest(
    sims: &mut [SimHolding<'_>],
    prices: &[f64],
    reinvested: &[f64],
    allocation: Allocation,
    top_n: u32,
    elapsed: f64,
) {
    let total: f64 = reinvested.iter().sum();
    if total <= 0.0 {
        return;
    }
    let top_n = top_n.clamp(1, super::MAX_TOP_N) as usize;
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
    let scheduled: Vec<usize> = (0..sims.len())
        .filter(|index| sims[*index].scheduled)
        .collect();
    let mut amounts = vec![0.0; sims.len()];

    match allocation {
        Allocation::SameHolding => amounts.copy_from_slice(reinvested),
        Allocation::Proportional => {
            let total_value: f64 = values.iter().sum();
            if total_value <= 0.0 {
                return;
            }
            for index in scheduled {
                amounts[index] = total * values[index] / total_value;
            }
        }
        Allocation::EqualWeight => {
            if scheduled.is_empty() {
                return;
            }
            let each = total / scheduled.len() as f64;
            for index in scheduled {
                amounts[index] = each;
            }
        }
        Allocation::TopValue => {
            let mut ranked: Vec<(usize, f64)> = scheduled
                .iter()
                .map(|index| (*index, values[*index]))
                .collect();
            ranked.sort_by(|left, right| right.1.total_cmp(&left.1).then(left.0.cmp(&right.0)));
            ranked.truncate(top_n);
            if ranked.is_empty() {
                return;
            }
            let selected_value: f64 = ranked.iter().map(|(_, value)| value).sum();
            if selected_value <= 0.0 {
                return;
            }
            for (index, value) in ranked {
                amounts[index] = total * value / selected_value;
            }
        }
        Allocation::TopYield => {
            let mut ranked: Vec<(usize, f64)> = scheduled
                .iter()
                .map(|index| {
                    let sim = &sims[*index];
                    let ttm = if high_yield(sim) {
                        sim.growth.ttm_per_share
                    } else {
                        sim.growth.ttm_per_share * (1.0 + sim.g).powf(elapsed)
                    };
                    let current_yield = if prices[*index] > 0.0 {
                        ttm / prices[*index]
                    } else {
                        0.0
                    };
                    (*index, current_yield)
                })
                .collect();
            ranked.sort_by(|left, right| right.1.total_cmp(&left.1).then(left.0.cmp(&right.0)));
            ranked.truncate(top_n);
            if ranked.is_empty() {
                return;
            }
            let selected_value: f64 = ranked.iter().map(|(index, _)| values[*index]).sum();
            if selected_value <= 0.0 {
                let each = total / ranked.len() as f64;
                for (index, _) in ranked {
                    amounts[index] = each;
                }
            } else {
                for (index, _) in ranked {
                    amounts[index] = total * values[index] / selected_value;
                }
            }
        }
        Allocation::Underweight => {
            if scheduled.is_empty() {
                return;
            }
            let total_value: f64 = scheduled.iter().map(|index| values[*index]).sum();
            let target = total_value / scheduled.len() as f64;
            let shortfalls: Vec<(usize, f64)> = scheduled
                .iter()
                .map(|index| (*index, (target - values[*index]).max(0.0)))
                .collect();
            let shortfall_total: f64 = shortfalls.iter().map(|(_, value)| value).sum();
            if shortfall_total <= 0.0 {
                let each = total / scheduled.len() as f64;
                for index in scheduled {
                    amounts[index] = each;
                }
            } else {
                for (index, shortfall) in shortfalls {
                    amounts[index] = total * shortfall / shortfall_total;
                }
            }
        }
    }

    for (sim, (price, amount)) in sims.iter_mut().zip(prices.iter().zip(&amounts)) {
        if !sim.scheduled || *amount <= 0.0 {
            continue;
        }
        let unit_price = price * sim.holding.fx;
        if unit_price > 0.0 {
            sim.units += amount / unit_price;
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
        "Prices follow each holding's own price trend over the last three years (shrunk toward zero); without one, they grow with its dividends so the yield stays constant."
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
        if holding.price > 0.0 && growth.ttm_per_share / holding.price > super::HIGH_YIELD {
            notes.push(format!(
                "{} yields {:.0}% on today's price, so its payout is assumed to follow its price, and its price to fall by at least {:.0}% a year (total return capped at {:.0}%).",
                holding.symbol,
                growth.ttm_per_share / holding.price * 100.0,
                (growth.ttm_per_share / holding.price - super::HIGH_YIELD_TOTAL_RETURN) * 100.0,
                super::HIGH_YIELD_TOTAL_RETURN * 100.0
            ));
        }
        if cut(growth) {
            notes.push(format!(
                "{} had a year with no payment after a paying year, so its base growth is capped at {:.0}%.",
                holding.symbol,
                -MAX_GROWTH * 100.0
            ));
        }
    }
    let top_n = settings.top_n.clamp(1, super::MAX_TOP_N);
    notes.push(match settings.allocation {
        Allocation::Proportional => {
            "Reinvestment is spread across the paying holdings in proportion to their current value."
                .to_string()
        }
        Allocation::SameHolding => {
            "Each net payment is reinvested in the holding that paid it.".to_string()
        }
        Allocation::TopValue => format!(
            "Reinvestment is spread across the {top_n} largest paying holdings by current value, in proportion to value."
        ),
        Allocation::EqualWeight => {
            "Reinvestment is split equally across every paying holding, regardless of current weight."
                .to_string()
        }
        Allocation::TopYield => format!(
            "Reinvestment is spread across the {top_n} highest-yielding paying holdings, in proportion to value."
        ),
        Allocation::Underweight => {
            "Reinvestment goes to the paying holdings furthest below their equal-weight target."
                .to_string()
        }
    });
    notes.push(
        "Reinvestment assumes fractional units, no commissions and no price impact.".to_string(),
    );
    notes
}
