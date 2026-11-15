//! Round 24: the income distribution, the maximizer job, dividend safety from SEC company
//! facts, dividend candidates and the weekly growth calibration.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

use super::dividends_run::{check_dividend_controls, first_of_next_month};
use super::{CapitalService, ServiceError};
use crate::contracts::{AccountId, AsOf, InstrumentKind};
use crate::dividends::calibrate::{CalibrationInput, CalibrationReport};
use crate::dividends::maximize::{Constraints, IncomePlan, Objective};
use crate::dividends::montecarlo::{
    DEFAULT_PATHS, IncomeDistribution, Limits, MAX_PATHS, McHolding, McInputs, McSettings, Policy,
    Rules,
};
use crate::dividends::safety::{Safety, SafetyGrade};
use crate::dividends::{Allocation, DEFAULT_SHRINK_K, Payment, PaymentKind};
use crate::evidence::Cik;
use crate::trade::{AssetClass, Instrument, Region};

/// Average correlation assumed when too little common price history exists (fixed a
/// priori: typical of a diversified equity portfolio).
const DEFAULT_CORRELATION: f64 = 0.4;

/// FX volatility assumed for a holding outside the account currency (fixed a priori:
/// EUR/USD's long-run annual volatility is about 7–9%).
const DEFAULT_FX_VOL: f64 = 0.08;

/// Company facts and calibrations are refreshed after this many days.
const REFRESH_DAYS: i64 = 7;

/// Paths the maximizer searches on.
const SEARCH_PATHS: u32 = 200;

/// Paths it scores on.
const SCORE_PATHS: u32 = 2_000;

/// Most SEC company-facts requests in one refresh.
const MAX_FACTS_PER_REFRESH: usize = 40;

/// The projection controls a distribution or a plan runs with. Unknown fields are
/// refused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncomeRequest {
    /// Years, 1..=30.
    pub years: u32,
    /// Share of each net payment reinvested, 0..=100.
    pub reinvest_pct: f64,
    /// Where the pool goes, for a distribution (the plan searches its own weights).
    #[serde(default = "default_allocation")]
    pub allocation: Allocation,
    /// Monthly allowance.
    #[serde(default)]
    pub monthly_contribution: f64,
    /// Names for the top-N modes and the plan.
    #[serde(default = "default_top_n")]
    pub top_n: u32,
    /// Portuguese reserve.
    #[serde(default)]
    pub reserve_pt_tax: bool,
    /// Paths for a distribution (default 2,000).
    #[serde(default)]
    pub paths: Option<u32>,
    /// For a distribution: `(year, monthly net target)`, in the projection's nominal money
    /// (not inflated; the caller inflates a target given in today's money).
    #[serde(default)]
    pub target: Option<(u32, f64)>,
    /// For a plan: what to maximize (default robust income at the last year).
    #[serde(default)]
    pub objective: Option<Objective>,
    /// For a plan: limits (default from the confirmed policy).
    #[serde(default)]
    pub constraints: Option<Constraints>,
}

fn default_allocation() -> Allocation {
    Allocation::Proportional
}

fn default_top_n() -> u32 {
    crate::dividends::DEFAULT_TOP_N
}

/// Where the latest plan job is. JSON: `"idle"`, `"running"`, `"done"` or `"failed"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncomeJobState {
    /// Never started since the daemon started.
    #[default]
    Idle,
    /// Searching.
    Running,
    /// Finished; `plan` is set.
    Done,
    /// Failed; `error` is set.
    Failed,
}

/// The latest plan job.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IncomeJob {
    /// State.
    pub state: IncomeJobState,
    /// The request it ran with.
    pub request: Option<IncomeRequest>,
    /// The plan, when done.
    pub plan: Option<IncomePlan>,
    /// Why it failed.
    pub error: Option<String>,
    /// When it started.
    pub started_at: Option<DateTime<Utc>>,
    /// When it finished.
    pub finished_at: Option<DateTime<Utc>>,
}

/// A dividend candidate as the owner sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateView {
    /// XTB symbol.
    pub symbol: String,
    /// Yahoo symbol.
    pub yahoo: String,
    /// Name.
    pub name: String,
    /// Marked as a candidate.
    pub enabled: bool,
    /// Trailing-year payout per share, when a history is stored.
    pub ttm_per_share: Option<f64>,
    /// Trailing yield on the latest price.
    pub trailing_yield: Option<f64>,
    /// Years of payments stored.
    pub years_of_history: u32,
    /// Safety, for US listings with company facts.
    pub safety: Safety,
}

fn annual_vol(closes: &[f64]) -> Option<f64> {
    let start = closes.len().saturating_sub(253);
    let returns: Vec<f64> = closes[start..]
        .windows(2)
        .filter(|w| w[0] > 0.0 && w[1] > 0.0)
        .map(|w| (w[1] / w[0]).ln())
        .collect();
    if returns.len() < 60 {
        return None;
    }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let var = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (returns.len() - 1) as f64;
    Some(var.sqrt() * 252f64.sqrt()).filter(|v| v.is_finite() && *v > 0.0)
}

/// Average pairwise correlation of weekly (every fifth close) log returns over the last
/// two years, over pairs with at least 52 common weeks.
fn average_correlation(series: &[Vec<(NaiveDate, f64)>]) -> f64 {
    let weekly: Vec<BTreeMap<NaiveDate, f64>> = series
        .iter()
        .map(|closes| {
            let start = closes.len().saturating_sub(5 * 104 + 1);
            let picked: Vec<&(NaiveDate, f64)> = closes[start..].iter().step_by(5).collect();
            picked
                .windows(2)
                .filter(|w| w[0].1 > 0.0 && w[1].1 > 0.0)
                .map(|w| (w[1].0, (w[1].1 / w[0].1).ln()))
                .collect()
        })
        .collect();
    let mut sum = 0.0;
    let mut pairs = 0u32;
    for i in 0..weekly.len() {
        for j in i + 1..weekly.len() {
            let common: Vec<(f64, f64)> = weekly[i]
                .iter()
                .filter_map(|(d, a)| weekly[j].get(d).map(|b| (*a, *b)))
                .collect();
            if common.len() < 52 {
                continue;
            }
            let n = common.len() as f64;
            let (ma, mb) = (
                common.iter().map(|c| c.0).sum::<f64>() / n,
                common.iter().map(|c| c.1).sum::<f64>() / n,
            );
            let cov: f64 = common.iter().map(|(a, b)| (a - ma) * (b - mb)).sum();
            let va: f64 = common.iter().map(|(a, _)| (a - ma).powi(2)).sum();
            let vb: f64 = common.iter().map(|(_, b)| (b - mb).powi(2)).sum();
            if va > 0.0 && vb > 0.0 {
                sum += cov / (va * vb).sqrt();
                pairs += 1;
            }
        }
    }
    if pairs == 0 {
        DEFAULT_CORRELATION
    } else {
        (sum / pairs as f64).clamp(0.0, 0.95)
    }
}

fn regular(history: &[Payment]) -> Vec<Payment> {
    crate::dividends::classify(history)
        .into_iter()
        .filter(|(_, kind)| *kind == PaymentKind::Regular)
        .map(|(p, _)| p)
        .collect()
}

impl CapitalService {
    /// The SEC issuer for a US-listed symbol, from the stored ticker map.
    pub(super) fn cik_for(
        &self,
        symbol: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Cik>, ServiceError> {
        if super::trade_context::region_of(symbol) != Region::Us {
            return Ok(None);
        }
        let Some(yahoo) = crate::market::yahoo_symbol(symbol) else {
            return Ok(None);
        };
        let Some(index) = self.store.ticker_index(now)? else {
            return Ok(None);
        };
        let wanted = yahoo.to_ascii_uppercase();
        let mut matches = index.entries.iter().filter(|e| e.ticker == wanted);
        Ok(match (matches.next(), matches.next()) {
            (Some(entry), None) => Some(entry.cik),
            _ => None,
        })
    }

    /// Dividend safety for a symbol as of today, from stored company facts.
    pub(super) fn safety_for(
        &self,
        symbol: &str,
        history: &[Payment],
        now: DateTime<Utc>,
    ) -> Result<Safety, ServiceError> {
        let today = now.date_naive();
        let facts = match self.cik_for(symbol, now)? {
            Some(cik) => self
                .store
                .company_facts(cik.0)?
                .map(|(_, f)| f.known_by(today)),
            None => None,
        };
        Ok(crate::dividends::safety::grade(
            facts.as_ref(),
            &regular(history),
            today,
        ))
    }

    /// Fetches company facts for the US holdings and candidates whose stored facts are
    /// missing or a week old (at most [`MAX_FACTS_PER_REFRESH`]). Without SEC access it
    /// does nothing. Returns how many were stored.
    pub(super) async fn refresh_company_facts(&self, symbols: &[String]) -> u32 {
        if !self.sec_configured {
            return 0;
        }
        let now = Utc::now();
        let mut ciks = Vec::new();
        for symbol in symbols {
            let Ok(Some(cik)) = self.cik_for(symbol, now) else {
                continue;
            };
            let fresh = self
                .store
                .company_facts(cik.0)
                .ok()
                .flatten()
                .is_some_and(|(at, _)| (now - at).num_days() < REFRESH_DAYS);
            if !fresh && !ciks.contains(&cik) {
                ciks.push(cik);
            }
        }
        let mut stored = 0;
        for cik in ciks.into_iter().take(MAX_FACTS_PER_REFRESH) {
            match self.edgar.company_facts(cik).await {
                Ok(facts) => {
                    if self.store.record_company_facts(&facts, Utc::now()).is_ok() {
                        stored += 1;
                    }
                }
                Err(error) => {
                    tracing::warn!("company facts for CIK {} not fetched: {error}", cik.0)
                }
            }
        }
        stored
    }

    /// The watchlist's stocks and ETFs as dividend candidates.
    pub fn dividend_candidates(&self) -> Result<Vec<CandidateView>, ServiceError> {
        let now = Utc::now();
        let enabled = self.store.dividend_candidates()?;
        let mut out = Vec::new();
        for item in self.watchlist()? {
            if !matches!(item.class, AssetClass::Stock | AssetClass::Etf) {
                continue;
            }
            let history = self
                .store
                .dividend_history(&item.yahoo)?
                .map(|(_, _, h)| h)
                .unwrap_or_default();
            let growth = crate::dividends::growth(&history, now.date_naive());
            let price = self
                .store
                .prices(&super::watch_key(&item.symbol), now)?
                .last()
                .and_then(|o| o.close.to_f64());
            let years: std::collections::BTreeSet<i32> =
                history.iter().map(|p| p.ex_date.year()).collect();
            out.push(CandidateView {
                enabled: enabled.contains(&item.symbol.to_ascii_uppercase()),
                ttm_per_share: (growth.ttm_per_share > 0.0).then_some(growth.ttm_per_share),
                trailing_yield: price
                    .filter(|p| *p > 0.0 && growth.ttm_per_share > 0.0)
                    .map(|p| growth.ttm_per_share / p),
                years_of_history: years.len() as u32,
                safety: self.safety_for(&item.symbol, &history, now)?,
                symbol: item.symbol,
                yahoo: item.yahoo,
                name: item.name,
            });
        }
        Ok(out)
    }

    /// Marks or unmarks a watchlist stock or ETF as a dividend candidate.
    pub fn set_dividend_candidate(&self, symbol: &str, enabled: bool) -> Result<(), ServiceError> {
        let known = self.watchlist()?.into_iter().any(|item| {
            item.symbol.eq_ignore_ascii_case(symbol)
                && matches!(item.class, AssetClass::Stock | AssetClass::Etf)
        });
        if !known {
            return Err(ServiceError::BadRequest(format!(
                "{symbol} is not a stock or ETF on the watchlist."
            )));
        }
        self.store
            .record_dividend_candidate(symbol, enabled, Utc::now())?;
        Ok(())
    }

    /// The simulation inputs: the paying holdings, then the enabled candidates.
    fn mc_inputs(
        &self,
        account: &AccountId,
        now: DateTime<Utc>,
    ) -> Result<(McInputs, Vec<String>), ServiceError> {
        let (holdings, _, notes) = self.cached_dividend_inputs(account, now)?;
        let projection = self.portfolio(account, AsOf::now())?;
        let account_currency = projection
            .cash
            .first()
            .map(|m| m.currency.as_str().to_string())
            .unwrap_or_else(|| "EUR".into());
        let kinds: BTreeMap<String, InstrumentKind> = projection
            .positions
            .iter()
            .map(|p| (p.instrument.key(), p.instrument.kind))
            .collect();
        let mut series = Vec::new();
        let mut out = Vec::new();
        for holding in &holdings {
            let observations = self.store.prices(&holding.key, now)?;
            let closes: Vec<(NaiveDate, f64)> = observations
                .iter()
                .filter_map(|o| Some((o.session, o.close.to_f64()?)))
                .collect();
            let currency = observations
                .last()
                .and_then(|o| o.currency.as_ref().map(|c| c.as_str().to_string()));
            let foreign = currency.as_deref().is_some_and(|c| c != account_currency);
            out.push(McHolding {
                key: holding.key.clone(),
                symbol: holding.symbol.clone(),
                units: holding.units,
                price: holding.price,
                fx: holding.fx,
                history: holding.history.clone(),
                withholding: holding.withholding,
                price_growth: holding.price_growth,
                price_vol: annual_vol(&closes.iter().map(|c| c.1).collect::<Vec<_>>())
                    .unwrap_or(crate::dividends::montecarlo::DEFAULT_PRICE_VOL),
                fx_vol: if foreign { DEFAULT_FX_VOL } else { 0.0 },
                foreign,
                candidate: false,
                fund: kinds.get(&holding.key) == Some(&InstrumentKind::Etf),
                safety: self
                    .safety_for(&holding.symbol, &holding.history, now)?
                    .grade,
            });
            series.push(closes);
        }
        let enabled = self.store.dividend_candidates()?;
        let fx_closes = {
            let mut fx = super::FxCloses::new();
            for item in self.watchlist()? {
                if item.class == AssetClass::Fx
                    && let Some(close) = self
                        .store
                        .prices(&super::watch_key(&item.symbol), now)?
                        .last()
                        .and_then(|o| o.close.to_f64())
                {
                    fx.insert(item.symbol.to_ascii_uppercase(), close);
                }
            }
            fx
        };
        for item in self.watchlist()? {
            if !enabled.contains(&item.symbol.to_ascii_uppercase())
                || out
                    .iter()
                    .any(|h| h.symbol.eq_ignore_ascii_case(&item.symbol))
            {
                continue;
            }
            let key = super::watch_key(&item.symbol);
            let observations = self.store.prices(&key, now)?;
            let Some(price) = observations.last().and_then(|o| o.close.to_f64()) else {
                continue;
            };
            let Some(history) = self.store.dividend_history(&item.yahoo)?.map(|(_, _, h)| h) else {
                continue;
            };
            let years: std::collections::BTreeSet<i32> =
                history.iter().map(|p| p.ex_date.year()).collect();
            if years.len() < 3 {
                continue;
            }
            let closes: Vec<(NaiveDate, f64)> = observations
                .iter()
                .filter_map(|o| Some((o.session, o.close.to_f64()?)))
                .collect();
            let foreign = item.currency != account_currency;
            let fx = super::fx_rate(&item.currency, &account_currency, &fx_closes).unwrap_or(1.0);
            out.push(McHolding {
                key,
                symbol: item.symbol.clone(),
                units: 0.0,
                price,
                fx,
                withholding: crate::dividends::default_withholding(&item.symbol),
                price_growth: super::dividends_run::price_trend(&observations),
                price_vol: annual_vol(&closes.iter().map(|c| c.1).collect::<Vec<_>>())
                    .unwrap_or(crate::dividends::montecarlo::DEFAULT_PRICE_VOL),
                fx_vol: if foreign { DEFAULT_FX_VOL } else { 0.0 },
                foreign,
                candidate: true,
                fund: item.class == AssetClass::Etf,
                safety: self.safety_for(&item.symbol, &history, now)?.grade,
                history,
            });
            series.push(closes);
        }
        let calibration = self.store.dividend_calibration()?.map(|(_, r)| r);
        let shrink_k = calibration
            .as_ref()
            .map_or(DEFAULT_SHRINK_K, |r| r.adopted_k);
        Ok((
            McInputs {
                holdings: out,
                avg_correlation: average_correlation(&series),
                calibration,
                shrink_k,
                start: first_of_next_month(now.date_naive()),
            },
            notes,
        ))
    }

    fn rules_for(&self, request: &IncomeRequest, limits: Limits) -> Result<Rules, ServiceError> {
        check_dividend_controls(
            request.years,
            request.reinvest_pct,
            request.monthly_contribution,
            request.top_n,
        )?;
        Ok(Rules {
            years: request.years,
            reinvest_pct: request.reinvest_pct,
            monthly_contribution: request.monthly_contribution,
            reserve_pt_tax: request.reserve_pt_tax,
            limits,
            stress: Default::default(),
        })
    }

    /// The income distribution of the request's allocation mode (module doc of
    /// [`crate::dividends::montecarlo`]). The seed is the day, so a page reload shows the
    /// same numbers.
    pub fn income_distribution(
        &self,
        account: &AccountId,
        request: &IncomeRequest,
    ) -> Result<IncomeDistribution, ServiceError> {
        let now = Utc::now();
        let rules = self.rules_for(request, Limits::default())?;
        let (inputs, notes) = self.mc_inputs(account, now)?;
        let paths = request.paths.unwrap_or(DEFAULT_PATHS);
        if !(100..=MAX_PATHS).contains(&paths) {
            return Err(ServiceError::BadRequest(format!(
                "Paths must be between 100 and {MAX_PATHS}."
            )));
        }
        // Holdings only: candidates aren't bought by a fixed mode.
        let mut held = inputs.clone();
        held.holdings.retain(|h| !h.candidate);
        let target = match request.target {
            Some((year, monthly)) if monthly.is_finite() && monthly > 0.0 => {
                Some((year, monthly * 12.0))
            }
            _ => None,
        };
        let mut distribution = crate::dividends::montecarlo::simulate(
            &held,
            &rules,
            &Policy::Mode {
                allocation: request.allocation,
                top_n: request.top_n,
            },
            McSettings {
                paths,
                seed: day_seed(now.date_naive()),
            },
            target,
        );
        distribution.notes.extend(notes);
        Ok(distribution)
    }

    /// The default plan constraints: the confirmed largest position and the drafts.
    fn default_constraints(&self, account: &AccountId, top_n: u32) -> Constraints {
        let max_position_pct = self
            .policy(account)
            .ok()
            .and_then(|p| p.current.limits.max_position_pct.to_f64())
            .unwrap_or(10.0);
        Constraints {
            max_position_pct,
            top_n,
            ..Constraints::default()
        }
    }

    /// Starts a plan search in the background. Returns false when one is running.
    pub fn start_income_plan(
        self: &std::sync::Arc<Self>,
        account: &AccountId,
        request: IncomeRequest,
    ) -> Result<bool, ServiceError> {
        let objective = request.objective.unwrap_or(Objective::RobustIncome {
            year: request.years,
        });
        let constraints = request
            .constraints
            .unwrap_or_else(|| self.default_constraints(account, request.top_n));
        if let Objective::IncomeWithWealthFloor { floor_pct, .. } = objective
            && !(0.0..=100.0).contains(&floor_pct)
        {
            return Err(ServiceError::BadRequest(
                "The wealth floor must be between 0 and 100%.".into(),
            ));
        }
        let rules = self.rules_for(&request, Limits::default())?;
        let now = Utc::now();
        let (inputs, _) = self.mc_inputs(account, now)?;
        {
            let mut job = self
                .income_job
                .lock()
                .map_err(|_| ServiceError::BadRequest("The plan job is unavailable.".into()))?;
            if job.state == IncomeJobState::Running {
                return Ok(false);
            }
            *job = IncomeJob {
                state: IncomeJobState::Running,
                request: Some(request.clone()),
                plan: None,
                error: None,
                started_at: Some(now),
                finished_at: None,
            };
        }
        let job = self.income_job.clone();
        let seed = day_seed(now.date_naive());
        tokio::task::spawn_blocking(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::dividends::maximize::solve(
                    &inputs,
                    &rules,
                    objective,
                    constraints,
                    McSettings {
                        paths: SEARCH_PATHS,
                        seed,
                    },
                    McSettings {
                        paths: SCORE_PATHS,
                        seed: seed ^ 0x5EED_5C0E,
                    },
                )
            }));
            if let Ok(mut job) = job.lock() {
                job.finished_at = Some(Utc::now());
                match result {
                    Ok(plan) => {
                        job.state = IncomeJobState::Done;
                        job.plan = Some(plan);
                    }
                    Err(_) => {
                        job.state = IncomeJobState::Failed;
                        job.error = Some("The plan search stopped unexpectedly.".into());
                    }
                }
            }
        });
        Ok(true)
    }

    /// The latest plan job.
    pub fn income_plan_job(&self) -> IncomeJob {
        self.income_job
            .lock()
            .map(|job| job.clone())
            .unwrap_or_default()
    }

    /// The latest dividend calibration report and when it ran.
    pub fn dividend_calibration(
        &self,
    ) -> Result<Option<(DateTime<Utc>, CalibrationReport)>, ServiceError> {
        Ok(self.store.dividend_calibration()?)
    }

    /// Reruns the growth calibration over every stored payout history of the trade
    /// universe's stocks and ETFs when the stored report is a week old. `universe` carries
    /// the bars for the year-end prices; `yahoo_of` maps an instrument to its Yahoo symbol.
    pub(super) fn refresh_dividend_calibration(
        &self,
        universe: &[Instrument],
        yahoo_of: &dyn Fn(&Instrument) -> Option<String>,
        now: DateTime<Utc>,
    ) -> Result<(), ServiceError> {
        if let Some((at, _)) = self.store.dividend_calibration()?
            && now - at < Duration::days(REFRESH_DAYS)
        {
            return Ok(());
        }
        let mut inputs = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for instrument in universe {
            if !matches!(instrument.class, AssetClass::Stock | AssetClass::Etf) {
                continue;
            }
            let Some(yahoo) = yahoo_of(instrument) else {
                continue;
            };
            if !seen.insert(yahoo.clone()) {
                continue;
            }
            let Some((_, _, history)) = self.store.dividend_history(&yahoo)? else {
                continue;
            };
            if history.is_empty() {
                continue;
            }
            let mut year_end_prices = BTreeMap::new();
            for bar in &instrument.bars {
                // The last bar of each year wins.
                year_end_prices.insert(bar.session.year(), bar.close);
            }
            inputs.push(CalibrationInput {
                symbol: yahoo,
                history,
                year_end_prices,
            });
        }
        if inputs.is_empty() {
            return Ok(());
        }
        let report = crate::dividends::calibrate::backtest(&inputs, now.year() - 1);
        self.store.record_dividend_calibration(&report, now)?;
        Ok(())
    }

    /// Safety per held symbol, for the projection page.
    pub(super) fn holdings_safety(
        &self,
        holdings: &[crate::dividends::Holding],
        now: DateTime<Utc>,
    ) -> Result<BTreeMap<String, Safety>, ServiceError> {
        let mut out = BTreeMap::new();
        for holding in holdings {
            let safety = self.safety_for(&holding.symbol, &holding.history, now)?;
            if safety.grade != SafetyGrade::Unknown || !safety.reasons.is_empty() {
                out.insert(holding.symbol.clone(), safety);
            }
        }
        Ok(out)
    }
}

/// A seed from the date, so runs on one day agree.
fn day_seed(day: NaiveDate) -> u64 {
    (day.num_days_from_ce() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlation_falls_back_without_history() {
        assert_eq!(average_correlation(&[]), DEFAULT_CORRELATION);
        let day = |i: i64| NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + Duration::days(i);
        let a: Vec<(NaiveDate, f64)> = (0..600)
            .map(|i| (day(i), 100.0 + (i as f64).sin()))
            .collect();
        let same = average_correlation(&[a.clone(), a]);
        assert!((same - 0.95).abs() < 1e-9, "{same}");
    }

    #[test]
    fn volatility_needs_sixty_returns() {
        assert_eq!(annual_vol(&[1.0; 30]), None);
        let closes: Vec<f64> = (0..300)
            .map(|i| if i % 2 == 0 { 100.0 } else { 101.0 })
            .collect();
        assert!(annual_vol(&closes).is_some_and(|v| v > 0.1));
    }
}
