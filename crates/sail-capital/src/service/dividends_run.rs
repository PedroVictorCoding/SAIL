//! Round 14: inputs for the dividend projection (histories, withholding, FX, calibration).
use super::{CapitalService, ServiceError};
use crate::contracts::{AccountId, AsOf};
use crate::dividends::{Allocation, DividendOutlook, Holding, Payment, Settings};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

/// Histories are refetched after this many days.
const HISTORY_MAX_AGE_DAYS: i64 = 7;

/// When the trailing year implied by the per-share history differs from what the ledger
/// actually received by more than this factor (either way), the ledger level is used.
const CALIBRATION_TOLERANCE: f64 = 1.6;

/// Holding inputs, the original cost of the paying holdings, and notes about the data.
type Inputs = (Vec<Holding>, Option<f64>, Vec<String>);

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
        Ok(DividendRefresh { fetched, failed })
    }

    /// The dividend projection for the current holdings.
    pub fn dividend_projection(
        &self,
        account: &AccountId,
        years: u32,
        reinvest_pct: f64,
        allocation: Allocation,
        reserve_pt_tax: bool,
    ) -> Result<DividendOutlook, ServiceError> {
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
        let now = Utc::now();
        let today = now.date_naive();
        let (holdings, original_cost, mut notes) = self.dividend_inputs(account, now)?;
        let start = first_of_next_month(today);
        let settings = Settings {
            years,
            reinvest_pct,
            allocation,
            reserve_pt_tax,
            start,
        };
        let mut outlook = crate::dividends::project(&holdings, &settings, original_cost);
        notes.append(&mut outlook.notes);
        outlook.notes = notes;
        Ok(outlook)
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
        let received = self.dividends(account, Some(now - Duration::days(730)), None)?;
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
            let mine: Vec<&crate::ledger::DividendView> = received
                .iter()
                .filter(|d| d.instrument.as_ref().is_some_and(|i| i.key() == key))
                .collect();
            let gross: f64 = mine.iter().filter_map(|d| d.gross.amount.to_f64()).sum();
            let withheld: f64 = mine
                .iter()
                .filter_map(|d| d.withheld.as_ref().and_then(|w| w.amount.to_f64()))
                .map(f64::abs)
                .sum();
            let (withholding, source) = if gross > 0.0 {
                ((withheld / gross).clamp(0.0, 0.6), "observed".to_string())
            } else {
                (
                    crate::dividends::default_withholding(&symbol),
                    "default".to_string(),
                )
            };

            // Calibration: compare the history's trailing year with what was received.
            let history = calibrate(&symbol, &history, &mine, units, fx, now, &mut notes);

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

/// Scales a per-share history to what the ledger received in the last 365 days when the
/// two disagree by more than [`CALIBRATION_TOLERANCE`] (e.g. Yahoo quoting a fund's
/// payout in its base currency rather than the listing's). Holdings bought during the
/// year make the ledger lower, so only a gross mismatch triggers it.
fn calibrate(
    symbol: &str,
    history: &[Payment],
    received: &[&crate::ledger::DividendView],
    units: f64,
    fx: f64,
    now: DateTime<Utc>,
    notes: &mut Vec<String>,
) -> Vec<Payment> {
    let year_ago = now.date_naive() - Duration::days(365);
    let history_ttm: f64 = history
        .iter()
        .filter(|p| p.ex_date > year_ago)
        .map(|p| p.per_share)
        .sum();
    let ledger_ttm: f64 = received
        .iter()
        .filter(|d| d.paid.at.date_naive() > year_ago)
        .filter_map(|d| d.gross.amount.to_f64())
        .sum();
    if history_ttm <= 0.0 || ledger_ttm <= 0.0 || units <= 0.0 || fx <= 0.0 {
        return history.to_vec();
    }
    let implied = history_ttm * units * fx;
    let ratio = ledger_ttm / implied;
    if (1.0 / CALIBRATION_TOLERANCE..=CALIBRATION_TOLERANCE).contains(&ratio) {
        return history.to_vec();
    }
    notes.push(format!(
        "{symbol}: the published per-share payouts imply {:.0}% of what you actually received over the last year, so the history was scaled to your receipts.",
        100.0 / ratio
    ));
    history
        .iter()
        .map(|p| Payment {
            ex_date: p.ex_date,
            per_share: p.per_share * ratio,
        })
        .collect()
}

fn first_of_next_month(day: NaiveDate) -> NaiveDate {
    let (y, m) = if day.month() == 12 {
        (day.year() + 1, 1)
    } else {
        (day.year(), day.month() + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).expect("valid date")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_scales_only_a_gross_mismatch() {
        let now = Utc::now();
        let day = |d: i64| (now - Duration::days(d)).date_naive();
        let history = vec![
            Payment {
                ex_date: day(300),
                per_share: 0.5,
            },
            Payment {
                ex_date: day(120),
                per_share: 0.5,
            },
        ];
        // 10 units, fx 1: the history implies 10 a year.
        let mut notes = Vec::new();
        assert_eq!(
            calibrate("A", &history, &[], 10.0, 1.0, now, &mut notes),
            history
        );
        let view = |amount: i64| crate::ledger::DividendView {
            instrument: None,
            paid: crate::contracts::EventTime {
                at: now - Duration::days(100),
                precision: crate::contracts::TimePrecision::DateOnly,
            },
            gross: crate::contracts::Money {
                amount: rust_decimal::Decimal::from(amount),
                currency: crate::contracts::Currency::eur(),
            },
            withheld: None,
            net: None,
            record_ids: Vec::new(),
        };
        // Received 8: within tolerance, unchanged.
        let close = view(8);
        assert_eq!(
            calibrate("A", &history, &[&close], 10.0, 1.0, now, &mut notes),
            history
        );
        assert!(notes.is_empty());
        // Received 25: 2.5 times the history, scaled to it.
        let far = view(25);
        let scaled = calibrate("A", &history, &[&far], 10.0, 1.0, now, &mut notes);
        assert!((scaled[0].per_share - 1.25).abs() < 1e-12);
        assert_eq!(notes.len(), 1);
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
