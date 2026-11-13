//! The risk policy, suggestion actions, signals and the Today view.

use super::*;

/// `GET /policy`: the policy in force and every stored version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyView {
    /// The latest stored version, or the built-in draft (version 0).
    pub current: crate::policy::RiskPolicy,
    /// Stored versions, newest first.
    pub history: Vec<crate::policy::RiskPolicy>,
}

/// Horizon, in sessions, that ideas are formed at: about a quarter, so trading costs
/// weigh less against the expected gain.
pub const IDEA_HORIZON: u32 = 60;

/// XTB suffixes whose listings trade outside the euro.
const NON_EURO_SUFFIXES: [&str; 7] = ["US", "UK", "CH", "DK", "SE", "NO", "PL"];

impl CapitalService {
    /// The policy in force and its history.
    pub fn policy(&self, account: &AccountId) -> Result<PolicyView, ServiceError> {
        let history = self.store.policies(account)?;
        let current = history
            .first()
            .cloned()
            .unwrap_or_else(crate::policy::RiskPolicy::builtin_draft);
        Ok(PolicyView { current, history })
    }

    /// Stores a new policy version, as a draft or confirmed.
    pub fn save_policy(
        &self,
        account: &AccountId,
        limits: &crate::policy::RiskLimits,
        confirm: bool,
        now: DateTime<Utc>,
    ) -> Result<crate::policy::RiskPolicy, ServiceError> {
        limits.validate().map_err(ServiceError::BadRequest)?;
        let status = if confirm {
            crate::policy::PolicyStatus::Confirmed
        } else {
            crate::policy::PolicyStatus::Draft
        };
        Ok(self.store.save_policy(account, limits, status, now)?)
    }

    /// Records a dismissal or "done" for a suggestion.
    pub fn suggestion_action(
        &self,
        account: &AccountId,
        id: &str,
        action: crate::suggest::Action,
        now: DateTime<Utc>,
    ) -> Result<crate::suggest::ActionRecord, ServiceError> {
        if id.len() != 16 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ServiceError::BadRequest("Unknown suggestion id.".into()));
        }
        let record = crate::suggest::ActionRecord {
            suggestion_id: id.to_string(),
            action,
            at: now,
        };
        self.store.record_suggestion_action(account, &record)?;
        Ok(record)
    }

    /// The signal report over the signal universe (holdings plus the watchlist's stocks
    /// and ETFs), cached until prices, model values or the universe change.
    pub fn signals(
        &self,
        account: &AccountId,
    ) -> Result<crate::signals::SignalReport, ServiceError> {
        let held = self.universe_pairs(account)?;
        let mut held_keys: Vec<&str> = held.iter().map(|(key, _)| key.as_str()).collect();
        held_keys.sort_unstable();
        let model_version: u64 = self
            .store
            .signal_value_coverage()?
            .iter()
            .map(|c| u64::from(c.values))
            .sum();
        let version = format!(
            "{}|{}|{}|{}",
            account.as_str(),
            self.store.price_version()?,
            model_version,
            held_keys.join("|")
        );
        if let Ok(cache) = self.signals_cache.lock()
            && let Some((cached, report)) = cache.as_ref()
            && *cached == version
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
        let sigma = |key: &str, horizon: u32| -> Option<f64> {
            let s = series.iter().find(|s| s.instrument_key == key)?;
            ewma_sigma(s, horizon)
        };
        let external = self.external_scores(&held_keys)?;
        let report = crate::signals::evaluate_and_combine(
            &series,
            &external,
            &sigma,
            &crate::signals::SignalEvalConfig::default(),
        );
        if let Ok(mut cache) = self.signals_cache.lock() {
            *cache = Some((version, report.clone()));
        }
        Ok(report)
    }

    /// Everything the Today view shows.
    pub fn today(
        &self,
        account: &AccountId,
        now: DateTime<Utc>,
    ) -> Result<crate::suggest::Today, ServiceError> {
        use rust_decimal::prelude::ToPrimitive;
        let policy = self.policy(account)?.current;
        let handled = self.store.handled_suggestions(account)?;
        let insights = self.insights(account, AsOf::now(), now)?;
        let projection = self.portfolio(account, AsOf::now())?;
        let prices_configured = self.price_status()?.configured;

        let held = self.held_instruments(account)?;
        let data = self.closes_for(&held, now)?;
        let mut without_prices = Vec::new();
        let mut stale_prices = Vec::new();
        for (_, symbol, closes) in &data {
            match closes.last() {
                None => without_prices.push(symbol.clone()),
                Some((session, _))
                    if (now.date_naive() - *session).num_days()
                        > crate::suggest::build::STALE_PRICE_DAYS =>
                {
                    stale_prices.push((symbol.clone(), *session))
                }
                Some(_) => {}
            }
        }

        let new_filings = match self.filings(account, AsOf::now()) {
            Ok(view) => view
                .items
                .iter()
                .filter(|w| {
                    (now - w.first_observed_at).num_days() <= crate::suggest::build::NEW_FILING_DAYS
                })
                .map(|w| crate::suggest::build::NewFiling {
                    symbol: w.issuer_name.clone(),
                    form: w.filing.form.clone(),
                    accession: w.filing.accession.clone(),
                })
                .collect(),
            Err(_) => Vec::new(),
        };

        let report = self.signals(account)?;
        let combined_ic = report
            .combined_ic
            .iter()
            .find(|(h, _)| *h == IDEA_HORIZON)
            .map(|(_, ic)| *ic)
            .unwrap_or(0.0);
        let signals_passed: Vec<String> = report
            .skill
            .iter()
            .filter(|s| s.horizon_sessions == IDEA_HORIZON && s.ic_used != 0.0)
            .map(|s| {
                format!(
                    "{} h{} t={:.1}",
                    serde_json::to_value(s.signal)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default(),
                    s.horizon_sessions,
                    s.t_stat
                )
            })
            .collect();

        let mut policy_plan = None;
        let mut idea_plan = None;
        if let Some(account_view) = insights.account.as_ref()
            && let Some(account_value) = account_view.account_value.amount.to_f64()
            && account_value > 0.0
        {
            let free = account_view.free_funds.amount.to_f64().unwrap_or(0.0);
            let mut assets = Vec::new();
            let mut series_in_order = Vec::new();
            for slice in &insights.allocation {
                let closes = data
                    .iter()
                    .find(|(key, _, _)| *key == slice.instrument_key)
                    .map(|(_, _, c)| c.as_slice())
                    .unwrap_or(&[]);
                let series = crate::forecast::CloseSeries {
                    instrument_key: &slice.instrument_key,
                    symbol: &slice.symbol,
                    closes,
                };
                let sigma_annual = ewma_sigma(&series, 20).map(|s| s * (252.0f64 / 20.0).sqrt());
                let alpha = report
                    .latest
                    .iter()
                    .find(|c| {
                        c.instrument_key == slice.instrument_key
                            && c.horizon_sessions == IDEA_HORIZON
                    })
                    .map(|c| c.alpha)
                    .unwrap_or(0.0);
                let suffix = slice
                    .symbol
                    .rsplit_once('.')
                    .map(|(_, s)| s.to_ascii_uppercase())
                    .unwrap_or_default();
                assets.push(crate::optimize::Asset {
                    instrument_key: slice.instrument_key.clone(),
                    symbol: slice.symbol.clone(),
                    weight: slice.value.amount.to_f64().unwrap_or(0.0) / account_value,
                    alpha,
                    sigma_annual,
                    foreign_currency: NON_EURO_SUFFIXES.contains(&suffix.as_str()),
                    locked: closes.is_empty(),
                });
                series_in_order.push(series);
            }
            let problem = crate::optimize::Problem {
                assets,
                cash: free / account_value,
                covariance: crate::optimize::covariance(&series_in_order, IDEA_HORIZON),
                account_value,
                horizon_sessions: IDEA_HORIZON,
            };
            // Trimming a position to a limit needs no prices, so nothing is locked for the
            // policy plan; ideas need prices and keep holdings without them locked.
            let mut policy_problem = problem.clone();
            for asset in &mut policy_problem.assets {
                asset.locked = false;
            }
            let plan = crate::optimize::plan(
                &policy_problem,
                &policy.limits,
                crate::optimize::Mode::Policy,
            );
            policy_plan = Some(plan);
            if policy.is_confirmed() && combined_ic > 0.0 {
                idea_plan = Some(crate::optimize::plan(
                    &problem,
                    &policy.limits,
                    crate::optimize::Mode::Ideas,
                ));
            }
        }

        let facts = crate::suggest::build::Facts {
            now,
            account: insights.account.clone(),
            policy,
            snapshot_as_of: insights.freshness.snapshot_as_of,
            variances: projection
                .reconciliation
                .variances
                .iter()
                .map(|v| v.subject.clone())
                .collect(),
            unvalued: insights.unvalued.clone(),
            prices_configured,
            without_prices,
            stale_prices,
            new_filings,
            policy_plan,
            idea_plan,
            idea_horizon: IDEA_HORIZON,
            signals_passed,
            combined_ic,
            board: self
                .signal_board(account)
                .map(|b| b.rows)
                .unwrap_or_default(),
        };
        Ok(crate::suggest::build::build(facts, &handled))
    }
}

/// The `EwmaVolatility` sigma of the horizon log return, when the series supports one.
pub(super) fn ewma_sigma(series: &crate::forecast::CloseSeries<'_>, horizon: u32) -> Option<f64> {
    if series.closes.is_empty() {
        return None;
    }
    let f = crate::forecast::forecast(
        series,
        crate::forecast::ModelId::EwmaVolatility,
        horizon,
        crate::forecast::CalibrationState {
            verified: false,
            cohort: None,
        },
    );
    f.sigma_log_return
}
