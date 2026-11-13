//! Prices, price files, the walk-forward evaluation and baseline forecasts.

use super::*;
use crate::calendar::ExchangeCalendar;

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
        self.held_instruments_as_of(account, DecisionTime::now())
    }

    /// Held owned instruments as known at `decision.at`. CFDs are never forecast.
    pub(super) fn held_instruments_as_of(
        &self,
        account: &AccountId,
        decision: DecisionTime,
    ) -> Result<Vec<(String, String)>, ServiceError> {
        let projection = self.portfolio(account, decision.as_of())?;
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

    /// Closes and bar input manifests for `held` as of `decision` under `policy`.
    #[allow(clippy::type_complexity)]
    pub(super) fn closes_for_as_of(
        &self,
        held: &[(String, String)],
        decision: DecisionTime,
        policy: VintagePolicy,
    ) -> Result<
        (
            Vec<(
                String,
                String,
                Vec<(chrono::NaiveDate, rust_decimal::Decimal)>,
            )>,
            Vec<crate::replay::BarInputManifest>,
        ),
        ServiceError,
    > {
        let mut out = Vec::with_capacity(held.len());
        let mut manifests = Vec::with_capacity(held.len());
        for (key, symbol) in held {
            let revisions = self.store.prices_with_provenance(key, decision, policy)?;
            let closes: Vec<_> = revisions
                .iter()
                .map(|r| (r.observation.session, r.observation.close))
                .collect();
            let hash_input: Vec<_> = revisions
                .iter()
                .map(|r| {
                    (
                        r.id.clone(),
                        r.bar_sha256.clone(),
                        r.observation.clone(),
                        r.provenance,
                    )
                })
                .collect();
            let provenance =
                crate::replay::combine_provenance(revisions.iter().map(|r| r.provenance));
            manifests.push(crate::replay::BarInputManifest {
                instrument_key: key.clone(),
                symbol: symbol.clone(),
                provenance,
                bar_count: revisions.len() as u32,
                first_session: revisions.first().map(|r| r.observation.session),
                last_session: revisions.last().map(|r| r.observation.session),
                bars_sha256: crate::replay::hash_bar_revisions(&hash_input),
            });
            out.push((key.clone(), symbol.clone(), closes));
        }
        Ok((out, manifests))
    }

    /// Collects corporate-action (dividend and earnings) manifests for `held` at `decision`.
    pub(super) fn corporate_actions_manifest_as_of(
        &self,
        held: &[(String, String)],
        decision: DecisionTime,
        policy: VintagePolicy,
    ) -> Result<Vec<crate::replay::CorporateActionInputManifest>, ServiceError> {
        let mut out = Vec::new();
        for (key, symbol) in held {
            let yahoo = crate::market::yahoo_symbol(symbol).unwrap_or_else(|| symbol.clone());
            if let Some(((fetched_at, currency, payments), prov)) = self
                .store
                .dividend_history_as_of(&yahoo, decision, policy)?
            {
                out.push(crate::replay::CorporateActionInputManifest {
                    key: key.clone(),
                    kind: "dividends".into(),
                    provenance: prov,
                    fetched_at: Some(fetched_at),
                    version_sha256: crate::replay::hash_dividend_payments(
                        currency.as_deref(),
                        &payments,
                    ),
                });
            }
            if let Some(((fetched_at, dates), prov)) =
                self.store.earnings_as_of(&yahoo, decision, policy)?
            {
                out.push(crate::replay::CorporateActionInputManifest {
                    key: key.clone(),
                    kind: "earnings".into(),
                    provenance: prov,
                    fetched_at: Some(fetched_at),
                    version_sha256: crate::replay::hash_earnings_dates(&dates),
                });
            }
        }
        Ok(out)
    }

    /// Collects research-index membership manifests as of `decision`.
    pub(super) fn research_membership_manifest_as_of(
        &self,
        decision: DecisionTime,
        policy: VintagePolicy,
    ) -> Result<Vec<crate::replay::MembershipInputManifest>, ServiceError> {
        let mut out = Vec::new();
        for index in ["sp500", "stoxx50"] {
            if let Some(((fetched_at, members), prov)) =
                self.store.research_members_as_of(index, decision, policy)?
            {
                out.push(crate::replay::MembershipInputManifest {
                    source: format!("research:{index}"),
                    provenance: prov,
                    snapshot_at: Some(fetched_at),
                    snapshot_sha256: crate::replay::hash_research_members(&members),
                });
            }
        }
        Ok(out)
    }

    /// Policy / cost version string as known at `decision.at`.
    pub(super) fn cost_policy_version_as_of(
        &self,
        account: &AccountId,
        decision: DecisionTime,
    ) -> Result<String, ServiceError> {
        let policies = self.store.policies(account)?;
        let active = policies
            .into_iter()
            .find(|p| p.saved_at.is_none_or(|ts| ts <= decision.at))
            .unwrap_or_else(crate::policy::RiskPolicy::builtin_draft);
        Ok(format!(
            "policy:v{}:risk={}:costs={}",
            active.version,
            crate::replay::canonical_decimal(active.limits.risk_per_trade_pct),
            crate::trade::cache::costs_tag(crate::trade::Costs::default_for(
                crate::trade::AssetClass::Stock,
            ))
        ))
    }

    /// The walk-forward evaluation over held instruments, cached until prices or the held
    /// set change.
    pub fn evaluation(
        &self,
        account: &AccountId,
    ) -> Result<crate::forecast::EvalReport, ServiceError> {
        let decision = DecisionTime::now();
        let held = self.held_instruments_as_of(account, decision)?;
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
        let report = self
            .evaluation_as_of(account, decision, VintagePolicy::ObservedOnly)?
            .value;
        if let Ok(mut cache) = self.evaluation_cache.lock() {
            *cache = Some((version, report.clone()));
        }
        Ok(report)
    }

    /// Point-in-time walk-forward evaluation at `decision`, returning the report and its
    /// deterministic [`crate::replay::ReplayManifest`].
    pub fn evaluation_as_of(
        &self,
        account: &AccountId,
        decision: DecisionTime,
        policy: VintagePolicy,
    ) -> Result<crate::replay::ReplayOutput<crate::forecast::EvalReport>, ServiceError> {
        let held = self.held_instruments_as_of(account, decision)?;
        let (data, bar_manifests) = self.closes_for_as_of(&held, decision, policy)?;
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
        let mut manifest = crate::replay::ReplayManifest::new(
            decision,
            crate::calendar::WeekdayCalendar.id(),
            self.cost_policy_version_as_of(account, decision)?,
            format!(
                "{}|{}",
                crate::forecast::SCHEMA_VERSION,
                crate::replay::REPLAY_CODE_VERSION
            ),
        );
        manifest.instrument_keys = held.iter().map(|(k, _)| k.clone()).collect();
        manifest.bars = bar_manifests;
        manifest
            .membership
            .push(crate::replay::MembershipInputManifest {
                source: "portfolio".into(),
                provenance: InputProvenance::Observed,
                snapshot_at: Some(decision.at),
                snapshot_sha256: crate::replay::hash_universe_pairs(&held),
            });
        manifest
            .membership
            .extend(self.research_membership_manifest_as_of(decision, policy)?);
        manifest.corporate_actions =
            self.corporate_actions_manifest_as_of(&held, decision, policy)?;
        Ok(crate::replay::ReplayOutput {
            value: report,
            manifest: manifest.finalize(),
        })
    }

    /// Forecasts at `horizon` with `model` for every held owned instrument with prices.
    /// Calibration comes from the latest evaluation.
    pub fn forecasts(
        &self,
        account: &AccountId,
        horizon: u32,
        model: crate::forecast::ModelId,
    ) -> Result<ForecastsView, ServiceError> {
        let decision = DecisionTime::now();
        Ok(self
            .forecasts_as_of(
                account,
                horizon,
                model,
                decision,
                &crate::calendar::WeekdayCalendar,
                VintagePolicy::ObservedOnly,
            )?
            .value)
    }

    /// Point-in-time forecasts at `horizon` with `model` at `decision`, using `calendar`
    /// and `policy`. Never calls `Utc::now()`.
    pub fn forecasts_as_of(
        &self,
        account: &AccountId,
        horizon: u32,
        model: crate::forecast::ModelId,
        decision: DecisionTime,
        calendar: &dyn crate::calendar::ExchangeCalendar,
        policy: VintagePolicy,
    ) -> Result<crate::replay::ReplayOutput<ForecastsView>, ServiceError> {
        if !crate::forecast::HORIZONS.contains(&horizon) {
            return Err(ServiceError::BadRequest(format!(
                "The horizon must be one of {:?} sessions.",
                crate::forecast::HORIZONS
            )));
        }
        let eval_out = self.evaluation_as_of(account, decision, policy)?;
        let evaluation = eval_out.value;
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
        let held = self.held_instruments_as_of(account, decision)?;
        let (data, bar_manifests) = self.closes_for_as_of(&held, decision, policy)?;
        let mut forecasts = Vec::new();
        let mut without_prices = Vec::new();
        for (key, symbol, closes) in data {
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
        let mut manifest = crate::replay::ReplayManifest::new(
            decision,
            calendar.id(),
            self.cost_policy_version_as_of(account, decision)?,
            format!(
                "{}|{}",
                crate::forecast::SCHEMA_VERSION,
                crate::replay::REPLAY_CODE_VERSION
            ),
        );
        manifest.instrument_keys = held.iter().map(|(k, _)| k.clone()).collect();
        manifest.bars = bar_manifests;
        manifest.membership = eval_out.manifest.membership;
        manifest.corporate_actions = eval_out.manifest.corporate_actions;
        Ok(crate::replay::ReplayOutput {
            value: ForecastsView {
                as_of: decision.at,
                horizon_sessions: horizon,
                model,
                forecasts,
                without_prices,
            },
            manifest: manifest.finalize(),
        })
    }
}
