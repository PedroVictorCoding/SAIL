//! Prices, price files, the walk-forward evaluation and baseline forecasts.

use super::*;

/// Most observations one `/prices` reply returns.
pub const MAX_PRICES_REPLY: usize = 10_000;

/// The price refresh state, for `/prices/status`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceStatus {
    /// True when the owner opted in to fetching.
    pub configured: bool,
    /// Source id when configured.
    pub source: Option<String>,
    /// When the last refresh ran.
    pub last_refresh_at: Option<DateTime<Utc>>,
    /// Why the last refresh stopped, if it did.
    pub last_error: Option<String>,
    /// Instruments with stored prices.
    pub instruments: u32,
    /// Distinct stored sessions over all instruments.
    pub observations: u32,
    /// Per instrument.
    pub per_instrument: Vec<crate::store::PriceCoverage>,
    /// Held instruments the source can't name (CFDs, unmapped listings), by symbol.
    pub unmapped: Vec<String>,
}

/// What importing one price file did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceImport {
    /// Broker symbol.
    pub symbol: String,
    /// Ledger instrument key the prices were stored under.
    pub instrument_key: String,
    /// Bars read.
    pub sessions_seen: u32,
    /// New sessions.
    pub added: u32,
    /// Changed sessions.
    pub revisions: u32,
    /// First and last session in the file.
    pub first: Option<chrono::NaiveDate>,
    /// Last session in the file.
    pub last: Option<chrono::NaiveDate>,
}

/// Forecasts for the held instruments at one horizon and model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForecastsView {
    /// When SAIL computed them.
    pub as_of: DateTime<Utc>,
    /// Horizon in sessions.
    pub horizon_sessions: u32,
    /// The model.
    pub model: crate::forecast::ModelId,
    /// One per held owned instrument with prices.
    pub forecasts: Vec<crate::forecast::ForecastDistribution>,
    /// Held owned instruments with no stored prices, by symbol.
    pub without_prices: Vec<String>,
}

impl CapitalService {
    /// Held owned instruments: (instrument key, symbol). CFDs are never forecast.
    pub(super) fn held_instruments(
        &self,
        account: &AccountId,
    ) -> Result<Vec<(String, String)>, ServiceError> {
        let projection = self.portfolio(account, AsOf::now())?;
        let mut held: Vec<(String, String)> = projection
            .positions
            .iter()
            .map(|p| (p.instrument.key(), p.instrument.symbol.clone()))
            .collect();
        held.sort();
        held.dedup();
        Ok(held)
    }

    /// The current price status.
    pub fn price_status(&self) -> Result<PriceStatus, ServiceError> {
        let mut status = self
            .price_status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default();
        status.configured = self.prices_configured;
        status.source = self
            .prices_configured
            .then(|| self.prices.source_id().to_string());
        status.per_instrument = self.store.price_coverage()?;
        status.instruments = status.per_instrument.len() as u32;
        status.observations = status.per_instrument.iter().map(|c| c.sessions).sum();
        Ok(status)
    }

    /// Fetches daily prices for every held owned instrument the source can name. Makes
    /// no request unless the owner opted in; stops at the first refusal.
    pub async fn refresh_prices(
        &self,
        account: Option<&AccountId>,
    ) -> Result<PriceStatus, ServiceError> {
        let _running = self.price_refreshing.lock().await;
        let now = Utc::now();
        let mut last_error = None;
        let mut unmapped = Vec::new();
        if !self.prices_configured {
            last_error = Some(crate::market::PriceSourceError::NotConfigured.to_string());
        } else {
            let accounts = match account {
                Some(a) => vec![a.clone()],
                None => self.accounts()?.into_iter().map(|a| a.account).collect(),
            };
            let mut held: Vec<(String, String)> = Vec::new();
            for a in &accounts {
                held.extend(self.held_instruments(a)?);
            }
            held.sort();
            held.dedup();
            held.truncate(crate::market::MAX_INSTRUMENTS_PER_REFRESH);
            for (key, symbol) in held {
                match self.prices.daily(&symbol).await {
                    Ok(series) => {
                        let store = self.store.clone();
                        tokio::task::spawn_blocking(move || {
                            store.record_prices(&key, &series, Utc::now())
                        })
                        .await
                        .map_err(|_| ServiceError::Store(crate::store::StoreError::Poisoned))?
                        .map_err(ServiceError::from)?;
                    }
                    Err(crate::market::PriceSourceError::UnknownSymbol(_)) => unmapped.push(symbol),
                    Err(
                        e @ (crate::market::PriceSourceError::Refused(_)
                        | crate::market::PriceSourceError::Blocked),
                    ) => {
                        last_error = Some(e.to_string());
                        break;
                    }
                    Err(e) => last_error = Some(format!("{symbol}: {e}")),
                }
            }
        }
        if let Ok(mut status) = self.price_status.lock() {
            status.last_refresh_at = Some(now);
            status.last_error = last_error;
            status.unmapped = unmapped;
        }
        self.price_status()
    }

    /// Stores an owner-uploaded price file for `symbol`. The prices go under the held
    /// instrument with that symbol, or `sym:<SYMBOL>` when none is held.
    pub fn import_price_file(
        &self,
        account: Option<&AccountId>,
        symbol: &str,
        bytes: &[u8],
        currency: Option<&str>,
        adjustment: crate::market::Adjustment,
        now: DateTime<Utc>,
    ) -> Result<PriceImport, ServiceError> {
        let symbol = symbol.trim().to_ascii_uppercase();
        if symbol.is_empty() || symbol.len() > 32 {
            return Err(ServiceError::BadRequest(
                "Name the symbol the prices are for.".into(),
            ));
        }
        if bytes.len() > crate::market::MAX_PRICE_FILE_BYTES {
            return Err(ServiceError::BadRequest(
                "The price file is too large.".into(),
            ));
        }
        let currency = match currency.filter(|c| !c.is_empty()) {
            Some(c) => Some(Currency::parse(c).ok_or_else(|| {
                ServiceError::BadRequest("The currency must be a three-letter code.".into())
            })?),
            None => None,
        };
        let series = crate::market::parse_price_file(bytes, &symbol, currency, adjustment)
            .map_err(|e| ServiceError::BadRequest(e.to_string()))?;
        let held = match account {
            Some(a) => self.held_instruments(a)?,
            None => Vec::new(),
        };
        let key = held
            .into_iter()
            .find(|(_, s)| s.eq_ignore_ascii_case(&symbol))
            .map(|(k, _)| k)
            .unwrap_or_else(|| format!("sym:{symbol}"));
        let outcome = self.store.record_prices(&key, &series, now)?;
        Ok(PriceImport {
            symbol,
            instrument_key: key,
            sessions_seen: outcome.seen,
            added: outcome.added,
            revisions: outcome.revisions,
            first: series.bars.first().map(|b| b.session),
            last: series.bars.last().map(|b| b.session),
        })
    }

    /// Stored closes for `symbol` in `[from, to]`, as known now, at most
    /// [`MAX_PRICES_REPLY`].
    pub fn prices(
        &self,
        symbol: &str,
        from: Option<chrono::NaiveDate>,
        to: Option<chrono::NaiveDate>,
    ) -> Result<Vec<crate::market::MarketObservation>, ServiceError> {
        let symbol = symbol.trim();
        let Some(coverage) = self
            .store
            .price_coverage()?
            .into_iter()
            .find(|c| c.symbol.eq_ignore_ascii_case(symbol))
        else {
            return Ok(Vec::new());
        };
        let mut rows =
            self.store
                .prices_in_range(&coverage.instrument_key, Utc::now(), from, to)?;
        if rows.len() > MAX_PRICES_REPLY {
            rows.drain(..rows.len() - MAX_PRICES_REPLY);
        }
        Ok(rows)
    }

    /// Closes per held instrument as known at `known_by`: (key, symbol, closes).
    #[allow(clippy::type_complexity)]
    fn held_closes(
        &self,
        account: &AccountId,
        known_by: DateTime<Utc>,
    ) -> Result<
        Vec<(
            String,
            String,
            Vec<(chrono::NaiveDate, rust_decimal::Decimal)>,
        )>,
        ServiceError,
    > {
        let held = self.held_instruments(account)?;
        self.closes_for(&held, known_by)
    }

    /// Closes for an already resolved held set, so callers that need the set for a cache
    /// key don't project the portfolio twice.
    #[allow(clippy::type_complexity)]
    pub(super) fn closes_for(
        &self,
        held: &[(String, String)],
        known_by: DateTime<Utc>,
    ) -> Result<
        Vec<(
            String,
            String,
            Vec<(chrono::NaiveDate, rust_decimal::Decimal)>,
        )>,
        ServiceError,
    > {
        let mut out = Vec::new();
        for (key, symbol) in held {
            let closes: Vec<_> = self
                .store
                .prices(key, known_by)?
                .into_iter()
                .map(|o| (o.session, o.close))
                .collect();
            out.push((key.clone(), symbol.clone(), closes));
        }
        Ok(out)
    }

    /// The walk-forward evaluation over held instruments, cached until prices or the held
    /// set change.
    pub fn evaluation(
        &self,
        account: &AccountId,
    ) -> Result<crate::forecast::EvalReport, ServiceError> {
        let held = self.held_instruments(account)?;
        let mut held_keys: Vec<&str> = held.iter().map(|(key, _)| key.as_str()).collect();
        held_keys.sort_unstable();
        let version = format!(
            "{}|{}|{}",
            account.as_str(),
            self.store.price_version()?,
            held_keys.join("|")
        );
        if let Ok(cache) = self.evaluation_cache.lock()
            && let Some((cached_version, report)) = cache.as_ref()
            && *cached_version == version
        {
            return Ok(report.clone());
        }
        let data = self.closes_for(&held, Utc::now())?;
        let series: Vec<crate::forecast::CloseSeries<'_>> = data
            .iter()
            .filter(|(_, _, closes)| !closes.is_empty())
            .map(|(key, symbol, closes)| crate::forecast::CloseSeries {
                instrument_key: key,
                symbol,
                closes,
            })
            .collect();
        let report = crate::forecast::evaluate(&series, &crate::forecast::EvalConfig::default());
        if let Ok(mut cache) = self.evaluation_cache.lock() {
            *cache = Some((version, report.clone()));
        }
        Ok(report)
    }

    /// Forecasts at `horizon` with `model` for every held owned instrument with prices.
    /// Calibration comes from the latest evaluation.
    pub fn forecasts(
        &self,
        account: &AccountId,
        horizon: u32,
        model: crate::forecast::ModelId,
    ) -> Result<ForecastsView, ServiceError> {
        if !crate::forecast::HORIZONS.contains(&horizon) {
            return Err(ServiceError::BadRequest(format!(
                "The horizon must be one of {:?} sessions.",
                crate::forecast::HORIZONS
            )));
        }
        let evaluation = self.evaluation(account)?;
        let calibration = evaluation
            .scores
            .iter()
            .find(|s| s.model == model && s.horizon_sessions == horizon)
            .map(|s| crate::forecast::CalibrationState {
                verified: s.calibrated,
                cohort: evaluation
                    .origins
                    .map(|(a, b)| format!("walk-forward {a}..{b}, {} forecasts", s.forecasts)),
            })
            .unwrap_or(crate::forecast::CalibrationState {
                verified: false,
                cohort: None,
            });
        let mut forecasts = Vec::new();
        let mut without_prices = Vec::new();
        for (key, symbol, closes) in self.held_closes(account, Utc::now())? {
            if closes.is_empty() {
                without_prices.push(symbol);
                continue;
            }
            let series = crate::forecast::CloseSeries {
                instrument_key: &key,
                symbol: &symbol,
                closes: &closes,
            };
            forecasts.push(crate::forecast::forecast(
                &series,
                model,
                horizon,
                calibration.clone(),
            ));
        }
        Ok(ForecastsView {
            as_of: Utc::now(),
            horizon_sessions: horizon,
            model,
            forecasts,
            without_prices,
        })
    }
}
