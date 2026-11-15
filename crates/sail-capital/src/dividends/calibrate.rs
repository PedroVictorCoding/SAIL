//! Round 24: does the dividend growth estimator work? (B2, Claude.)
//!
//! The projection, the Monte Carlo and the maximizer all trust each holding's growth
//! estimate `g` and its spread `σ` ([`super::growth_with`]). This backtest checks them
//! point in time on many payment histories (the research universe's, plus the holdings'):
//!
//! - **Origins:** every calendar year-end `Y` from [`FIRST_ORIGIN_YEAR`] on, for a symbol
//!   that paid in `Y` and in at least [`MIN_PAYING_YEARS`] earlier years. The estimate
//!   uses only payments up to the end of `Y` (`as_of` = 1 January of `Y + 1`).
//! - **Outcome:** the realized compound growth of regular per-share payments from `Y` to
//!   `Y + 1` and to `Y + 5` (a year with nothing paid counts as −100%), clamped to ±100%.
//! - **Grid:** the shrinkage `k` in `n / (n + k)` over [`SHRINK_GRID`], fixed a priori.
//! - **Scores per k:** bias (mean error), median error and mean absolute error at both
//!   horizons, and the share of 5-year outcomes inside `[g − σ, g + σ]`.
//! - **Choice:** the `k` with the lowest 5-year MAE is adopted only when it beats the
//!   default ([`super::DEFAULT_SHRINK_K`]) by more than one standard error of the paired
//!   difference. Overlapping 5-year windows of one symbol share outcomes, so the standard
//!   error is widened by `√5`.
//! - **Cuts:** per trailing-yield bucket at the year-end price (when known), how often the
//!   next year's regular total fell below 0.9× this year's. [`cut_hazard`] blends those
//!   rates with an a-priori table, so a thin bucket leans on the table.
//!
//! Survivorship: the research universe lists today's index members, so companies that
//! cut and left the index are missing and cut rates are understated. The notes say so.
//!
//! Pure: no I/O, no clock.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

use super::{DEFAULT_SHRINK_K, Payment, PaymentKind, classify, growth_with};

/// The first origin year.
pub const FIRST_ORIGIN_YEAR: i32 = 2010;

/// Earlier paying years needed before an origin counts.
pub const MIN_PAYING_YEARS: usize = 3;

/// The shrinkage values compared (fixed a priori).
pub const SHRINK_GRID: [f64; 4] = [1.0, 3.0, 5.0, 10.0];

/// A cut: next year's regular total below this share of this year's.
pub const CUT_SHARE: f64 = 0.9;

/// Yield buckets (lower bounds, as fractions), the last one open-ended.
pub const YIELD_BUCKETS: [f64; 5] = [0.0, 0.02, 0.04, 0.06, 0.08];

/// A-priori yearly cut chance per bucket, used until evidence accrues. Fixed before any
/// backtest ran, in line with published S&P 500 cut frequencies (a few percent a year
/// overall, rising sharply with yield).
pub const PRIOR_CUT_RATE: [f64; 5] = [0.03, 0.04, 0.06, 0.10, 0.15];

/// Weight of the prior in [`cut_hazard`], in pseudo-years.
pub const PRIOR_WEIGHT_YEARS: f64 = 20.0;

/// One symbol's inputs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalibrationInput {
    /// Yahoo symbol.
    pub symbol: String,
    /// Per-share payments, any order.
    pub history: Vec<Payment>,
    /// Close at each calendar year-end, in the payments' currency, when known.
    #[serde(default)]
    pub year_end_prices: BTreeMap<i32, f64>,
}

/// Scores for one shrinkage value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShrinkScore {
    /// `k`.
    pub k: f64,
    /// Origins with a 1-year outcome.
    pub n_1y: u32,
    /// Origins with a 5-year outcome.
    pub n_5y: u32,
    /// Mean of `g − realized`, 1 year.
    pub bias_1y: f64,
    /// Mean of `g − realized`, 5 years.
    pub bias_5y: f64,
    /// Median of `g − realized`, 5 years.
    pub median_error_5y: f64,
    /// Mean absolute error, 1 year.
    pub mae_1y: f64,
    /// Mean absolute error, 5 years.
    pub mae_5y: f64,
    /// Mean of `|e(k)| − |e(default)|` over the same 5-year origins (negative is better).
    pub mae_5y_vs_default: f64,
    /// Its standard error, widened by `√5` for overlapping windows.
    pub mae_5y_vs_default_se: f64,
    /// Share of 5-year outcomes inside `[g − σ, g + σ]`.
    pub band_coverage_5y: f64,
}

/// Measured cuts in one yield bucket.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CutRate {
    /// Lower bound of the bucket's trailing yield.
    pub yield_from: f64,
    /// Upper bound, `None` for the last bucket.
    pub yield_to: Option<f64>,
    /// Symbol-years observed.
    pub years: u32,
    /// Of which the next year was a cut.
    pub cuts: u32,
    /// `cuts / years` (0 when none).
    pub rate: f64,
    /// The a-priori rate for the bucket.
    pub prior: f64,
}

/// The backtest's result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalibrationReport {
    /// Symbols with at least one origin.
    pub symbols: u32,
    /// One row per grid value, in grid order.
    pub shrink: Vec<ShrinkScore>,
    /// The `k` with the lowest 5-year MAE.
    pub best_k: f64,
    /// The `k` to use: `best_k` if it beat the default by more than a standard error,
    /// else the default.
    pub adopted_k: f64,
    /// Cut rates per yield bucket.
    pub cuts: Vec<CutRate>,
    /// Plain notes on what the numbers say and their limits.
    pub notes: Vec<String>,
}

/// Regular per-share totals per calendar year, from the first paying year on (gaps are
/// 0).
fn annual_regular(history: &[Payment]) -> BTreeMap<i32, f64> {
    let mut by_year: BTreeMap<i32, f64> = BTreeMap::new();
    for (payment, kind) in classify(history) {
        if kind == PaymentKind::Regular {
            *by_year.entry(payment.ex_date.year()).or_insert(0.0) += payment.per_share;
        }
    }
    let (Some(first), Some(last)) = (
        by_year.keys().next().copied(),
        by_year.keys().next_back().copied(),
    ) else {
        return BTreeMap::new();
    };
    (first..=last)
        .map(|year| (year, by_year.get(&year).copied().unwrap_or(0.0)))
        .collect()
}

fn realized(annual: &BTreeMap<i32, f64>, from: i32, years: i32) -> Option<f64> {
    let start = *annual.get(&from)?;
    let end = *annual.get(&(from + years))?;
    if start <= 0.0 {
        return None;
    }
    let value = if end <= 0.0 {
        -1.0
    } else {
        (end / start).powf(1.0 / years as f64) - 1.0
    };
    Some(value.clamp(-1.0, 1.0))
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

fn sd(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let m = mean(values);
    (values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (values.len() - 1) as f64).sqrt()
}

/// The yield bucket of a trailing yield.
pub fn bucket_of(trailing_yield: f64) -> usize {
    YIELD_BUCKETS
        .iter()
        .rposition(|from| trailing_yield >= *from)
        .unwrap_or(0)
}

/// Pure: the backtest over `inputs`, with origins from [`FIRST_ORIGIN_YEAR`] to
/// `last_origin_year` (the latest year with a known next year is `current year − 1`).
pub fn backtest(inputs: &[CalibrationInput], last_origin_year: i32) -> CalibrationReport {
    // (k index) -> errors
    let mut errors_1y: Vec<Vec<f64>> = vec![Vec::new(); SHRINK_GRID.len()];
    let mut errors_5y: Vec<Vec<f64>> = vec![Vec::new(); SHRINK_GRID.len()];
    let mut inside_5y: Vec<u32> = vec![0; SHRINK_GRID.len()];
    let mut cut_years = [0u32; YIELD_BUCKETS.len()];
    let mut cut_count = [0u32; YIELD_BUCKETS.len()];
    let mut symbols = 0u32;
    let default_index = SHRINK_GRID
        .iter()
        .position(|k| *k == DEFAULT_SHRINK_K)
        .unwrap_or(0);

    for input in inputs {
        let annual = annual_regular(&input.history);
        let mut counted = false;
        for (&year, &total) in &annual {
            if year < FIRST_ORIGIN_YEAR || year > last_origin_year || total <= 0.0 {
                continue;
            }
            let prior_paying = annual
                .range(..year)
                .filter(|(_, amount)| **amount > 0.0)
                .count();
            // Cut base rates need only a year-end price and the next year.
            if let (Some(price), Some(next)) =
                (input.year_end_prices.get(&year), annual.get(&(year + 1)))
                && *price > 0.0
            {
                let bucket = bucket_of(total / price);
                cut_years[bucket] += 1;
                if *next < CUT_SHARE * total {
                    cut_count[bucket] += 1;
                }
            }
            if prior_paying < MIN_PAYING_YEARS {
                continue;
            }
            let Some(as_of) = NaiveDate::from_ymd_opt(year + 1, 1, 1) else {
                continue;
            };
            let r1 = realized(&annual, year, 1);
            let r5 = realized(&annual, year, 5);
            if r1.is_none() && r5.is_none() {
                continue;
            }
            counted = true;
            for (index, k) in SHRINK_GRID.iter().enumerate() {
                let estimate = growth_with(&input.history, as_of, *k);
                if let Some(r1) = r1 {
                    errors_1y[index].push(estimate.g - r1);
                }
                if let Some(r5) = r5 {
                    errors_5y[index].push(estimate.g - r5);
                    if r5 >= estimate.g - estimate.sigma && r5 <= estimate.g + estimate.sigma {
                        inside_5y[index] += 1;
                    }
                }
            }
        }
        if counted {
            symbols += 1;
        }
    }

    let default_abs: Vec<f64> = errors_5y[default_index].iter().map(|e| e.abs()).collect();
    let shrink: Vec<ShrinkScore> = SHRINK_GRID
        .iter()
        .enumerate()
        .map(|(index, k)| {
            let abs_5y: Vec<f64> = errors_5y[index].iter().map(|e| e.abs()).collect();
            let paired: Vec<f64> = abs_5y
                .iter()
                .zip(&default_abs)
                .map(|(a, b)| a - b)
                .collect();
            let n5 = errors_5y[index].len();
            ShrinkScore {
                k: *k,
                n_1y: errors_1y[index].len() as u32,
                n_5y: n5 as u32,
                bias_1y: mean(&errors_1y[index]),
                bias_5y: mean(&errors_5y[index]),
                median_error_5y: median(&errors_5y[index]),
                mae_1y: mean(&errors_1y[index].iter().map(|e| e.abs()).collect::<Vec<_>>()),
                mae_5y: mean(&abs_5y),
                mae_5y_vs_default: mean(&paired),
                mae_5y_vs_default_se: if n5 > 1 {
                    sd(&paired) / (n5 as f64).sqrt() * 5f64.sqrt()
                } else {
                    0.0
                },
                band_coverage_5y: if n5 > 0 {
                    inside_5y[index] as f64 / n5 as f64
                } else {
                    0.0
                },
            }
        })
        .collect();

    let best = shrink
        .iter()
        .filter(|row| row.n_5y > 0)
        .min_by(|a, b| a.mae_5y.total_cmp(&b.mae_5y).then(a.k.total_cmp(&b.k)));
    let best_k = best.map_or(DEFAULT_SHRINK_K, |row| row.k);
    let adopted_k = match best {
        Some(row)
            if row.k != DEFAULT_SHRINK_K
                && row.mae_5y_vs_default < -row.mae_5y_vs_default_se
                && row.mae_5y_vs_default_se > 0.0 =>
        {
            row.k
        }
        _ => DEFAULT_SHRINK_K,
    };

    let cuts: Vec<CutRate> = YIELD_BUCKETS
        .iter()
        .enumerate()
        .map(|(index, from)| CutRate {
            yield_from: *from,
            yield_to: YIELD_BUCKETS.get(index + 1).copied(),
            years: cut_years[index],
            cuts: cut_count[index],
            rate: if cut_years[index] > 0 {
                cut_count[index] as f64 / cut_years[index] as f64
            } else {
                0.0
            },
            prior: PRIOR_CUT_RATE[index],
        })
        .collect();

    let mut notes = Vec::new();
    let default_row = &shrink[default_index];
    if default_row.n_5y == 0 {
        notes.push(
            "No payment history is long enough yet to score five-year growth, so the projection keeps its default shrinkage.".to_string(),
        );
    } else {
        notes.push(format!(
            "Over {} five-year outcomes from {} payers, the default estimate was off by {:.1} percentage points a year on average (bias {:+.1}).",
            default_row.n_5y,
            symbols,
            default_row.mae_5y * 100.0,
            default_row.bias_5y * 100.0
        ));
        notes.push(format!(
            "{:.0}% of five-year outcomes fell inside the low–high growth band (one σ either side; about 68% would mean the band is honest).",
            default_row.band_coverage_5y * 100.0
        ));
        if adopted_k != DEFAULT_SHRINK_K {
            notes.push(format!(
                "Shrinking growth by n/(n+{adopted_k}) beat the default n/(n+3) by more than its standard error, so the projection uses it."
            ));
        } else {
            notes.push("No other shrinkage beat the default n/(n+3) by more than its standard error, so it stays.".to_string());
        }
    }
    notes.push("The research list holds today's index members, so companies that cut and left the index are missing; cut rates are understated.".to_string());
    notes.push(
        "Overlapping five-year windows share outcomes; the standard errors are widened by √5 for that, which is approximate.".to_string(),
    );

    CalibrationReport {
        symbols,
        shrink,
        best_k,
        adopted_k,
        cuts,
        notes,
    }
}

/// Pure: the yearly cut chance for a trailing yield: the bucket's measured cuts blended
/// with its a-priori rate weighted as [`PRIOR_WEIGHT_YEARS`] years of evidence. Without a
/// report, the prior.
pub fn cut_hazard(report: Option<&CalibrationReport>, trailing_yield: f64) -> f64 {
    let bucket = bucket_of(trailing_yield.max(0.0));
    let prior = PRIOR_CUT_RATE[bucket];
    match report.and_then(|r| r.cuts.get(bucket)) {
        Some(row) => {
            (row.cuts as f64 + prior * PRIOR_WEIGHT_YEARS) / (row.years as f64 + PRIOR_WEIGHT_YEARS)
        }
        None => prior,
    }
}
