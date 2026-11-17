//! Round 14: inputs for the dividend projection (histories, withholding, FX, calibration).
use super::{CapitalService, ServiceError};
use crate::contracts::{AccountId, AsOf};
use crate::dividends::{Allocation, DividendOutlook, Holding, Payment, Settings};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

/// Histories are refetched after this many days.
pub(super) const HISTORY_MAX_AGE_DAYS: i64 = 7;

/// When the trailing year implied by the per-share history differs from what the ledger
/// actually received by more than this factor (either way), the ledger level is used.
const CALIBRATION_TOLERANCE: f64 = 1.6;

/// Holding inputs, the original cost of the paying holdings, and notes about the data.
pub(super) type Inputs = (Vec<Holding>, Option<f64>, Vec<String>);

/// `POST /dividends/refresh`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DividendRefresh {
    /// Histories fetched.
    pub fetched: u32,
    /// Symbols that failed or couldn't be mapped to Yahoo.
    pub failed: Vec<String>,
}

impl CapitalService {
    /// Fetches per-share dividend histories for the holdings whose snapshot is missing or
    /// older than a week (yfinance; only symbols leave the host).
    pub async fn refresh_dividend_histories(
        &self,
        account: &AccountId,
    ) -> Result<DividendRefresh, ServiceError> {
        let now = Utc::now();
        let mut failed = Vec::new();
        let mut wanted = Vec::new();
        let mut symbols: Vec<String> = self
            .held_instruments(account)?
            .into_iter()
            .map(|(_, symbol)| symbol)
            .collect();
        // Round 24: dividend candidates need histories (and US company facts) too.
        let candidates = self.store.dividend_candidates()?;
        for item in self.watchlist()? {
            if candidates.contains(&item.symbol.to_ascii_uppercase()) {
                let fresh = self
                    .store
                    .dividend_history(&item.yahoo)?
                    .is_some_and(|(at, ..)| (now - at).num_days() < HISTORY_MAX_AGE_DAYS);
                if !fresh {
                    wanted.push(item.yahoo.clone());
                }
                symbols.push(item.symbol);
            }
        }
        self.refresh_company_facts(&symbols).await;
        for (_, symbol) in self.held_instruments(account)? {
            match crate::market::yahoo_symbol(&symbol) {
                Some(yahoo) => {
                    let fresh = self
                        .store
                        .dividend_history(&yahoo)?
                        .is_some_and(|(at, ..)| (now - at).num_days() < HISTORY_MAX_AGE_DAYS);
                    if !fresh {
                        wanted.push(yahoo);
                    }
                }
                None => failed.push(symbol),
            }
        }
        if wanted.is_empty() {
            return Ok(DividendRefresh { fetched: 0, failed });
        }
        let (fetched, more_failed) = self.fetch_dividend_histories(&wanted).await?;
        failed.extend(more_failed);
        Ok(DividendRefresh { fetched, failed })
    }

    /// Runs the dividends helper over `wanted` Yahoo symbols (chunks of 100) and stores
    /// each history it returns. Returns how many were stored and the symbols that failed.
    pub(super) async fn fetch_dividend_histories(
        &self,
        wanted: &[String],
    ) -> Result<(u32, Vec<String>), ServiceError> {
        let mut failed = Vec::new();
        let python = std::env::var("SAIL_YFINANCE_PYTHON")
            .unwrap_or_else(|_| crate::market::yahoo::DEFAULT_PYTHON.to_string());
        let script = std::env::var("SAIL_DIVIDENDS_SCRIPT")
            .unwrap_or_else(|_| "scripts/prices/dividends.py".to_string());
        let mut fetched = 0u32;
        for chunk in wanted.chunks(100) {
            let mut command = tokio::process::Command::new(&python);
            command
                .arg("-I")
                .arg(&script)
                .args(chunk)
                .env_clear()
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            for var in [
                "PATH",
                "HOME",
                "LANG",
                "SSL_CERT_FILE",
                "HTTPS_PROXY",
                "https_proxy",
            ] {
                if let Ok(value) = std::env::var(var) {
                    command.env(var, value);
                }
            }
            command.env("XDG_CACHE_HOME", crate::market::yahoo::DEFAULT_CACHE);
            let output =
                match tokio::time::timeout(std::time::Duration::from_secs(600), command.output())
                    .await
                {
                    Ok(Ok(output)) => output,
                    _ => {
                        failed.extend(chunk.iter().cloned());
                        continue;
                    }
                };
            #[derive(Deserialize)]
            struct Out {
                #[serde(default)]
                history: std::collections::BTreeMap<String, Vec<(NaiveDate, f64)>>,
                #[serde(default)]
                currency: std::collections::BTreeMap<String, String>,
                #[serde(default)]
                failed: Vec<String>,
            }
            let Ok(out) = serde_json::from_slice::<Out>(&output.stdout) else {
                failed.extend(chunk.iter().cloned());
                continue;
            };
            failed.extend(out.failed);
            for (symbol, rows) in out.history {
                let history: Vec<Payment> = rows
                    .into_iter()
                    .filter(|(_, v)| v.is_finite() && *v > 0.0)
                    .map(|(ex_date, per_share)| Payment { ex_date, per_share })
                    .collect();
                self.store.record_dividend_history(
                    &symbol,
                    out.currency.get(&symbol).map(String::as_str),
                    &history,
                    Utc::now(),
                )?;
                fetched += 1;
            }
        }
        Ok((fetched, failed))
    }

    /// The dividend projection for the current holdings, with the default top-N count.
    pub fn dividend_projection(
        &self,
        account: &AccountId,
        years: u32,
        reinvest_pct: f64,
        allocation: Allocation,
        monthly_contribution: f64,
        reserve_pt_tax: bool,
    ) -> Result<DividendOutlook, ServiceError> {
        self.dividend_projection_with(
            account,
            years,
            reinvest_pct,
            allocation,
            monthly_contribution,
            crate::dividends::DEFAULT_TOP_N,
            reserve_pt_tax,
        )
    }

    /// The dividend projection with an explicit top-N count for the top-value and
    /// top-yield strategies.
    #[allow(clippy::too_many_arguments)] // One argument per projection control in the API.
    pub fn dividend_projection_with(
        &self,
        account: &AccountId,
        years: u32,
        reinvest_pct: f64,
        allocation: Allocation,
        monthly_contribution: f64,
        top_n: u32,
        reserve_pt_tax: bool,
    ) -> Result<DividendOutlook, ServiceError> {
        check_dividend_controls(years, reinvest_pct, monthly_contribution, top_n)?;
        let now = Utc::now();
        let today = now.date_naive();
        let (holdings, original_cost, mut notes) = self.cached_dividend_inputs(account, now)?;
        let start = first_of_next_month(today);
        let settings = Settings {
            years,
            reinvest_pct,
            allocation,
            top_n,
            monthly_contribution,
            reserve_pt_tax,
            start,
        };
        let mut outlook = crate::dividends::project(&holdings, &settings, original_cost);
        outlook.safety = self.holdings_safety(&holdings, now)?;
        notes.append(&mut outlook.notes);
        outlook.notes = notes;
        Ok(outlook)
    }

    /// The projection together with the early-retirement plan built from the same inputs.
    /// The plan's own horizon is extended to the retirement year when that is later than
    /// `years`, so the year it compares against always exists.
    #[allow(clippy::too_many_arguments)] // One argument per projection control in the API.
    pub fn dividend_retirement_with(
        &self,
        account: &AccountId,
        years: u32,
        reinvest_pct: f64,
        allocation: Allocation,
        monthly_contribution: f64,
        top_n: u32,
        reserve_pt_tax: bool,
        assumptions: crate::dividends::retirement::Assumptions,
    ) -> Result<(DividendOutlook, crate::dividends::retirement::Plan), ServiceError> {
        check_dividend_controls(years, reinvest_pct, monthly_contribution, top_n)?;
        assumptions.validate().map_err(ServiceError::BadRequest)?;
        let now = Utc::now();
        let (holdings, original_cost, mut notes) = self.cached_dividend_inputs(account, now)?;
        let settings = Settings {
            years,
            reinvest_pct,
            allocation,
            top_n,
            monthly_contribution,
            reserve_pt_tax,
            start: first_of_next_month(now.date_naive()),
        };
        let mut outlook = crate::dividends::project(&holdings, &settings, original_cost);
        let plan =
            crate::dividends::retirement::plan(&holdings, &settings, original_cost, &assumptions)
                .map_err(ServiceError::BadRequest)?;
        notes.append(&mut outlook.notes);
        outlook.notes = notes;
        Ok((outlook, plan))
    }

    /// [`Self::dividend_inputs`], reused while the ledger, prices, payout histories and the
    /// day are unchanged: the projection's settings don't affect them, so moving a control
    /// only re-runs the simulation.
    pub(super) fn cached_dividend_inputs(
        &self,
        account: &AccountId,
        now: DateTime<Utc>,
    ) -> Result<Inputs, ServiceError> {
        let version = format!(
            "{}|{}|{}|{}|{}|{:?}",
            account.as_str(),
            self.store.ledger_version(account)?,
            self.store.price_version()?,
            self.store.dividend_history_version()?,
            now.date_naive(),
            self.tax_profile(account)?
        );
        if let Ok(cache) = self.dividend_inputs_cache.lock()
            && let Some((v, inputs)) = cache.as_ref()
            && *v == version
        {
            return Ok(inputs.clone());
        }
        let inputs = self.dividend_inputs(account, now)?;
        if let Ok(mut cache) = self.dividend_inputs_cache.lock() {
            *cache = Some((version, inputs.clone()));
        }
        Ok(inputs)
    }

    /// Holding inputs, the original cost of the paying holdings, and notes about the data.
    fn dividend_inputs(
        &self,
        account: &AccountId,
        now: DateTime<Utc>,
    ) -> Result<Inputs, ServiceError> {
        let projection = self.portfolio(account, AsOf::now())?;
        let account_currency = projection
            .cash
            .first()
            .map(|m| m.currency.as_str().to_string())
            .unwrap_or_else(|| "EUR".into());
        let fx_closes = self.watch_fx_closes(now)?;
        let tax = self.tax_profile(account)?;
        let received = self.dividends(account, Some(now - Duration::days(730)), None)?;
        // Units held at each payment, from one replay of the ledger (not one projection per
        // receipt, which took seconds on a real account).
        let paid_at: Vec<DateTime<Utc>> = received.iter().map(|d| d.paid.at).collect();
        let units_then =
            crate::ledger::units_at(account, &self.ledger_records(account, now)?, now, &paid_at);
        let mut holdings = Vec::new();
        let mut notes = Vec::new();
        let mut cost = 0.0;
        let mut cost_known = true;
        for position in &projection.positions {
            let key = position.instrument.key();
            let symbol = position.instrument.symbol.clone();
            let Some(units) = position.quantity.to_f64().filter(|u| *u > 0.0) else {
                continue;
            };
            let observations = self.store.prices(&key, now)?;
            let Some(last) = observations.last() else {
                notes.push(format!("{symbol}: no stored price, left out."));
                continue;
            };
            let Some(price) = last.close.to_f64() else {
                continue;
            };
            let trading_currency = last.currency.as_ref().map(|c| c.as_str().to_string());
            // FX: the watchlist pairs, else what XTB's own valuation implies, else 1.
            let implied = position
                .broker_market_value
                .as_ref()
                .and_then(|m| m.amount.to_f64())
                .map(|v| v / (units * price))
                .filter(|f| f.is_finite() && *f > 0.0);
            let fx = match trading_currency.as_deref() {
                Some(c) => crate::service::fx_rate(c, &account_currency, &fx_closes).or(implied),
                None => implied,
            }
            .unwrap_or(1.0);

            let history = crate::market::yahoo_symbol(&symbol)
                .map(|y| self.store.dividend_history(&y))
                .transpose()?
                .flatten()
                .map(|(_, _, h)| h)
                .unwrap_or_default();

            // Observed withholding over the last 24 months.
            let mine: Vec<(&crate::ledger::DividendView, f64)> = received
                .iter()
                .zip(&units_then)
                .filter(|(d, _)| d.instrument.as_ref().is_some_and(|i| i.key() == key))
                .map(|(d, units)| (d, units.get(&key).and_then(|u| u.to_f64()).unwrap_or(0.0)))
                .collect();
            let gross: f64 = mine
                .iter()
                .filter_map(|(d, _)| d.gross.amount.to_f64())
                .sum();
            let withheld: f64 = mine
                .iter()
                .filter_map(|(d, _)| d.withheld.as_ref().and_then(|w| w.amount.to_f64()))
                .map(f64::abs)
                .sum();
            let domicile =
                crate::dividends::tax::domicile(position.instrument.isin.as_deref(), &symbol);
            let fund = position.instrument.kind == crate::contracts::InstrumentKind::Etf;
            let (withholding, source) = if gross > 0.0 {
                ((withheld / gross).clamp(0.0, 0.6), "observed".to_string())
            } else {
                let (rate, why) = crate::dividends::tax::source_withholding(
                    &tax,
                    domicile.as_deref(),
                    &symbol,
                    fund,
                );
                (rate, format!("default: {why}"))
            };
            let pt_rate = crate::dividends::tax::pt_rate(&tax, domicile.as_deref(), fund);

            // Calibration: each receipt against the payment it came from, using the units
            // held on the payment date (positions change through the year).
            let receipts: Vec<Receipt> = mine
                .iter()
                .filter_map(|(d, held_then)| {
                    Some(Receipt {
                        paid: d.paid.at.date_naive(),
                        gross: d.gross.amount.to_f64()?,
                        units: *held_then,
                    })
                })
                .collect();
            let history = calibrate(&symbol, &history, &receipts, fx, &mut notes);
            let price_growth = price_trend(&observations);

            match position.cost_basis.as_ref().and_then(|m| m.amount.to_f64()) {
                Some(c) => cost += c.abs(),
                None => cost_known = false,
            }
            holdings.push(Holding {
                key,
                symbol,
                units,
                price,
                fx,
                history,
                withholding,
                withholding_source: source,
                pt_rate,
                price_growth,
            });
        }
        Ok((holdings, (cost_known && cost > 0.0).then_some(cost), notes))
    }

    /// Latest closes of the watchlist FX pairs, by XTB pair name.
    fn watch_fx_closes(
        &self,
        now: DateTime<Utc>,
    ) -> Result<crate::service::FxCloses, ServiceError> {
        let mut out = crate::service::FxCloses::new();
        for item in self.watchlist()? {
            if item.class != crate::trade::AssetClass::Fx {
                continue;
            }
            if let Some(close) = self
                .store
                .prices(&crate::service::watch_key(&item.symbol), now)?
                .last()
                .and_then(|o| o.close.to_f64())
            {
                out.insert(item.symbol.to_ascii_uppercase(), close);
            }
        }
        Ok(out)
    }
}

/// One dividend the ledger received, with the units held when it was paid.
struct Receipt {
    paid: NaiveDate,
    gross: f64,
    units: f64,
}

/// Scales a per-share history when it disagrees with what was actually received, e.g.
/// Yahoo quoting a fund's payout in its base currency rather than the listing's. Each
/// receipt is matched to the latest published payment with an ex-date in the 75 days
/// before it, and compared using the units held on the payment date. The history is
/// scaled by the median ratio only when that median is outside
/// [`CALIBRATION_TOLERANCE`] and the ratios agree with each other (all within a factor
/// of 1.5 of the median), so a one-off special payment doesn't trigger it.
fn calibrate(
    symbol: &str,
    history: &[Payment],
    receipts: &[Receipt],
    fx: f64,
    notes: &mut Vec<String>,
) -> Vec<Payment> {
    if fx <= 0.0 {
        return history.to_vec();
    }
    let mut ratios: Vec<f64> = receipts
        .iter()
        .filter(|r| r.units > 0.0 && r.gross > 0.0)
        .filter_map(|r| {
            let matched = history
                .iter()
                .filter(|p| p.ex_date <= r.paid && (r.paid - p.ex_date).num_days() <= 75)
                .max_by_key(|p| p.ex_date)?;
            let expected = r.units * matched.per_share * fx;
            (expected > 0.0).then(|| r.gross / expected)
        })
        .collect();
    if ratios.len() < 2 {
        return history.to_vec();
    }
    ratios.sort_by(f64::total_cmp);
    let median = ratios[ratios.len() / 2];
    let consistent = ratios
        .iter()
        .all(|r| (median / 1.5..=median * 1.5).contains(r));
    if (1.0 / CALIBRATION_TOLERANCE..=CALIBRATION_TOLERANCE).contains(&median) || !consistent {
        return history.to_vec();
    }
    notes.push(format!(
        "{symbol}: the published per-share payouts were {:.0}% of what you actually received per unit, so the history was scaled to your receipts.",
        100.0 / median
    ));
    history
        .iter()
        .map(|p| Payment {
            ex_date: p.ex_date,
            per_share: p.per_share * median,
        })
        .collect()
}

/// The annual price trend over up to the last three years of stored closes: the
/// compound rate, shrunk toward zero by `n/(n+3)` (`n` = years of data) and capped to
/// −50%..+15%. `None` with less than a year.
pub(super) fn price_trend(observations: &[crate::market::MarketObservation]) -> Option<f64> {
    let last = observations.last()?;
    let from = last.session - Duration::days(3 * 365);
    let first = observations.iter().find(|o| o.session >= from)?;
    let days = (last.session - first.session).num_days();
    if days < 365 {
        return None;
    }
    let (p0, p1) = (first.close.to_f64()?, last.close.to_f64()?);
    if p0 <= 0.0 || p1 <= 0.0 {
        return None;
    }
    let years = days as f64 / 365.25;
    let raw = (p1 / p0).powf(1.0 / years) - 1.0;
    Some((raw * years / (years + 3.0)).clamp(-0.5, 0.15))
}

pub(super) fn first_of_next_month(day: NaiveDate) -> NaiveDate {
    let (y, m) = if day.month() == 12 {
        (day.year() + 1, 1)
    } else {
        (day.year(), day.month() + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).expect("valid date")
}

/// The projection controls shared by the projection and the retirement plan.
pub(super) fn check_dividend_controls(
    years: u32,
    reinvest_pct: f64,
    monthly_contribution: f64,
    top_n: u32,
) -> Result<(), ServiceError> {
    if !(1..=crate::dividends::MAX_YEARS).contains(&years) {
        return Err(ServiceError::BadRequest(format!(
            "Years must be between 1 and {}.",
            crate::dividends::MAX_YEARS
        )));
    }
    if !(0.0..=100.0).contains(&reinvest_pct) || !reinvest_pct.is_finite() {
        return Err(ServiceError::BadRequest(
            "The reinvested share must be between 0 and 100.".into(),
        ));
    }
    if !(0.0..=crate::dividends::MAX_MONTHLY_CONTRIBUTION).contains(&monthly_contribution)
        || !monthly_contribution.is_finite()
    {
        return Err(ServiceError::BadRequest(format!(
            "The monthly allowance must be between 0 and {}.",
            crate::dividends::MAX_MONTHLY_CONTRIBUTION
        )));
    }
    if !(1..=crate::dividends::MAX_TOP_N).contains(&top_n) {
        return Err(ServiceError::BadRequest(format!(
            "Top N must be between 1 and {}.",
            crate::dividends::MAX_TOP_N
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_uses_units_held_on_each_payment() {
        let d = |m: u32, day: u32| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        let history = vec![
            Payment {
                ex_date: d(3, 1),
                per_share: 0.5,
            },
            Payment {
                ex_date: d(6, 1),
                per_share: 0.5,
            },
        ];
        let mut notes = Vec::new();
        // 10 units then 40 units: receipts match the history, nothing changes even though
        // today's holding is larger.
        let fine = [
            Receipt {
                paid: d(3, 20),
                gross: 5.0,
                units: 10.0,
            },
            Receipt {
                paid: d(6, 20),
                gross: 20.0,
                units: 40.0,
            },
        ];
        assert_eq!(calibrate("A", &history, &fine, 1.0, &mut notes), history);
        assert!(notes.is_empty());
        // Every receipt is a hundredth of the published amount (pence vs pounds).
        let pence = [
            Receipt {
                paid: d(3, 20),
                gross: 0.05,
                units: 10.0,
            },
            Receipt {
                paid: d(6, 20),
                gross: 0.20,
                units: 40.0,
            },
        ];
        let scaled = calibrate("A", &history, &pence, 1.0, &mut notes);
        assert!((scaled[0].per_share - 0.005).abs() < 1e-12);
        assert_eq!(notes.len(), 1);
        // Inconsistent ratios (a one-off) leave it alone.
        let mixed = [
            Receipt {
                paid: d(3, 20),
                gross: 0.05,
                units: 10.0,
            },
            Receipt {
                paid: d(6, 20),
                gross: 20.0,
                units: 40.0,
            },
        ];
        assert_eq!(calibrate("A", &history, &mixed, 1.0, &mut notes), history);
    }

    #[test]
    fn retirement_assumptions_are_rejected_before_any_data_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let service = CapitalService::open(dir.path()).unwrap();
        let account = crate::contracts::AccountId::new(crate::contracts::Broker::Xtb, "test");
        let bad = crate::dividends::retirement::Assumptions {
            current_age: 60,
            retire_age: 50,
            ..crate::dividends::retirement::Assumptions::default()
        };
        let outcome = service.dividend_retirement_with(
            &account,
            30,
            100.0,
            Allocation::Proportional,
            0.0,
            crate::dividends::DEFAULT_TOP_N,
            false,
            bad,
        );
        match outcome {
            Err(ServiceError::BadRequest(message)) => {
                assert!(message.contains("later than the current age"), "{message}");
            }
            Err(other) => panic!("unexpected error: {other:?}"),
            Ok(_) => panic!("an impossible retirement age must be refused"),
        }
    }

    #[test]
    fn the_projection_starts_next_month() {
        let d = NaiveDate::from_ymd_opt(2026, 12, 15).unwrap();
        assert_eq!(
            first_of_next_month(d),
            NaiveDate::from_ymd_opt(2027, 1, 1).unwrap()
        );
        let d = NaiveDate::from_ymd_opt(2026, 11, 12).unwrap();
        assert_eq!(
            first_of_next_month(d),
            NaiveDate::from_ymd_opt(2026, 12, 1).unwrap()
        );
    }
}
