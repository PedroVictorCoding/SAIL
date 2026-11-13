//! Model signals, the blended outlook and the outlook track record.

use super::today::ewma_sigma;
use super::*;

/// The last run of one model signal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelRun {
    /// When it finished.
    pub at: DateTime<Utc>,
    /// Model version the helper reported.
    pub model_version: Option<String>,
    /// Values asked for.
    pub requested: u32,
    /// New values stored.
    pub added: u32,
    /// Origins the helper couldn't score.
    pub skipped: u32,
    /// Why it failed, if it did.
    pub error: Option<String>,
    /// The helper's notes.
    pub notes: Vec<String>,
}

/// `GET /signals/models`: what each model signal is and what is stored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelsStatus {
    /// A refresh is running now.
    pub running: bool,
    /// Every known signal model, whether on or off.
    pub models: Vec<ModelStatus>,
    /// Every known outlook model, whether on or off.
    pub outlook: Vec<OutlookModelStatus>,
}

/// One outlook model's status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutlookModelStatus {
    /// What it is.
    pub spec: crate::models::OutlookSpec,
    /// Turned on in `SAIL_MODEL_SIGNALS`.
    pub enabled: bool,
    /// The last run, if any since SAIL started.
    pub last_run: Option<ModelRun>,
    /// Model version stored most recently, with instruments and paths.
    pub stored: Option<(String, u32, u32)>,
}

/// `GET /outlook`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutlookView {
    /// The blended outlook.
    pub outlook: crate::outlook::Outlook,
    /// Closes up to the origin, about `2·horizon` of them and at least 63.
    pub history: Vec<(chrono::NaiveDate, f64)>,
    /// Every stored session of the instrument, for choosing a replay cutoff.
    pub sessions: Vec<chrono::NaiveDate>,
}

/// One model's status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelStatus {
    /// What it is.
    pub spec: crate::models::ModelSpec,
    /// Turned on in `SAIL_MODEL_SIGNALS`.
    pub enabled: bool,
    /// The last run, if any since SAIL started.
    pub last_run: Option<ModelRun>,
    /// What is stored.
    pub stored: Option<crate::store::SignalValueCoverage>,
}

impl CapitalService {
    /// Turns on model signals (the service reads `SAIL_MODEL_SIGNALS` in `open`).
    pub fn with_models(mut self, specs: Vec<crate::models::ModelSpec>) -> Self {
        self.model_specs = specs;
        self
    }

    /// Sets the helper interpreter, script directory and cache (`open` reads
    /// `SAIL_SIGNALS_PYTHON`, `SAIL_SIGNALS_SCRIPTS` and `SAIL_SIGNALS_CACHE`).
    pub fn with_helper_paths(mut self, python: &str, scripts: &str, cache: &str) -> Self {
        self.helper_paths = (python.into(), scripts.into(), cache.into());
        self
    }

    /// Turns on outlook models (the service reads `SAIL_MODEL_SIGNALS` in `open`).
    pub fn with_outlook_models(mut self, specs: Vec<crate::models::OutlookSpec>) -> Self {
        self.outlook_specs = specs;
        self
    }

    /// Stored paths of the enabled outlook models for `key`, from each model's
    /// `evaluate_from` on.
    pub(super) fn external_paths(
        &self,
        key: &str,
    ) -> Result<Vec<crate::outlook::ExternalPath>, ServiceError> {
        let mut out = Vec::new();
        for spec in &self.outlook_specs {
            let (_, paths) = self.store.outlook_paths(spec.component, Some(key))?;
            out.extend(paths.into_iter().filter(|p| p.origin >= spec.evaluate_from));
        }
        Ok(out)
    }

    /// The signal horizon a direction tilt at `horizon` is read from, if any. Signals are
    /// validated at 20 and 60 sessions; longer horizons get no tilt.
    fn tilt_horizon(horizon: u32) -> Option<u32> {
        match horizon {
            21 => Some(20),
            63 => Some(60),
            _ => None,
        }
    }

    /// The blended outlook for one symbol in the signal universe (a holding or a
    /// watchlist stock or ETF), from the latest close or as a replay from `cutoff`.
    pub fn outlook(
        &self,
        account: &AccountId,
        symbol: &str,
        horizon: u32,
        cutoff: Option<chrono::NaiveDate>,
    ) -> Result<OutlookView, ServiceError> {
        if !crate::outlook::HORIZONS.contains(&horizon) {
            return Err(ServiceError::BadRequest(format!(
                "The horizon must be one of {:?} sessions.",
                crate::outlook::HORIZONS
            )));
        }
        let mut held = self.held_instruments(account)?;
        held.extend(self.universe_pairs(account)?);
        let wanted = symbol.trim().to_ascii_uppercase();
        let Some((key, symbol)) = held
            .iter()
            .find(|(_, s)| s.to_ascii_uppercase() == wanted)
            .cloned()
        else {
            return Err(ServiceError::BadRequest(format!(
                "{symbol} isn't a holding or on the watchlist."
            )));
        };
        let data = self.closes_for(&[(key.clone(), symbol.clone())], Utc::now())?;
        let closes = &data[0].2;
        let series = crate::forecast::CloseSeries {
            instrument_key: &key,
            symbol: &symbol,
            closes,
        };

        // The tilt as it was known at the cutoff: signals re-validated on data up to it.
        let (tilt, tilt_signals) = match Self::tilt_horizon(horizon) {
            None => (Vec::new(), Vec::new()),
            Some(signal_h) => {
                let report = match cutoff {
                    None => self.signals(account)?,
                    Some(c) => self.signals_at(account, c)?,
                };
                let alpha = report
                    .latest
                    .iter()
                    .find(|s| s.instrument_key == key && s.horizon_sessions == signal_h)
                    .map(|s| s.alpha)
                    .unwrap_or(0.0);
                let passed = report
                    .skill
                    .iter()
                    .filter(|s| s.horizon_sessions == signal_h && s.ic_used != 0.0)
                    .map(|s| {
                        format!(
                            "{} h{} t={:.1}",
                            Self::signal_name(s.signal),
                            s.horizon_sessions,
                            s.t_stat
                        )
                    })
                    .collect();
                (vec![(horizon, alpha)], passed)
            }
        };
        let instrument = crate::outlook::Instrument {
            series: series.clone(),
            external: self.external_paths(&key)?,
            tilt,
            tilt_signals,
        };
        let outlook = crate::outlook::outlook(&instrument, horizon, cutoff);
        use rust_decimal::prelude::ToPrimitive;
        let up_to: Vec<(chrono::NaiveDate, f64)> = closes
            .iter()
            .filter(|(d, _)| *d <= outlook.origin)
            .filter_map(|(d, c)| Some((*d, c.to_f64()?)))
            .collect();
        let keep = (2 * horizon as usize).max(63);
        let history = up_to[up_to.len().saturating_sub(keep)..].to_vec();
        Ok(OutlookView {
            outlook,
            history,
            sessions: closes.iter().map(|(d, _)| *d).collect(),
        })
    }

    /// The signal report with every input cut at `cutoff` (no look-ahead into a replay).
    fn signals_at(
        &self,
        account: &AccountId,
        cutoff: chrono::NaiveDate,
    ) -> Result<crate::signals::SignalReport, ServiceError> {
        let held = self.universe_pairs(account)?;
        let data = self.closes_for(&held, Utc::now())?;
        type Closes = Vec<(chrono::NaiveDate, rust_decimal::Decimal)>;
        let cut: Vec<(String, String, Closes)> = data
            .into_iter()
            .map(|(k, s, c)| (k, s, c.into_iter().filter(|(d, _)| *d <= cutoff).collect()))
            .collect();
        let series: Vec<crate::forecast::CloseSeries<'_>> = cut
            .iter()
            .filter(|(_, _, c)| !c.is_empty())
            .map(|(key, symbol, closes)| crate::forecast::CloseSeries {
                instrument_key: key,
                symbol,
                closes,
            })
            .collect();
        let held_keys: Vec<&str> = held.iter().map(|(k, _)| k.as_str()).collect();
        let external: Vec<crate::signals::ExternalScores> = self
            .external_scores(&held_keys)?
            .into_iter()
            .map(|mut e| {
                e.values.retain(|(_, d, _)| *d <= cutoff);
                e
            })
            .collect();
        let sigma = |key: &str, h: u32| -> Option<f64> {
            let s = series.iter().find(|s| s.instrument_key == key)?;
            ewma_sigma(s, h)
        };
        Ok(crate::signals::evaluate_and_combine(
            &series,
            &external,
            &sigma,
            &crate::signals::SignalEvalConfig::default(),
        ))
    }

    /// The blend's out-of-sample record at `horizon` over the held instruments.
    pub fn track_record(
        &self,
        account: &AccountId,
        horizon: u32,
    ) -> Result<crate::outlook::TrackRecord, ServiceError> {
        if !crate::outlook::HORIZONS.contains(&horizon) {
            return Err(ServiceError::BadRequest(format!(
                "The horizon must be one of {:?} sessions.",
                crate::outlook::HORIZONS
            )));
        }
        let held = self.held_instruments(account)?;
        let today = Utc::now().date_naive();
        let paths: u64 = self
            .store
            .outlook_path_counts()?
            .iter()
            .map(|(.., n)| u64::from(*n))
            .sum();
        let version = format!(
            "{}|{}|{}|{}|{}|{}",
            account.as_str(),
            self.store.price_version()?,
            paths,
            horizon,
            today,
            held.iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>()
                .join("|")
        );
        if let Ok(cache) = self.track_cache.lock()
            && let Some(record) = cache.get(&version)
        {
            return Ok(record.clone());
        }
        let data = self.closes_for(&held, Utc::now())?;
        let mut instruments = Vec::new();
        for (key, symbol, closes) in &data {
            if closes.is_empty() {
                continue;
            }
            instruments.push(crate::outlook::Instrument {
                series: crate::forecast::CloseSeries {
                    instrument_key: key,
                    symbol,
                    closes,
                },
                external: self.external_paths(key)?,
                tilt: Vec::new(),
                tilt_signals: Vec::new(),
            });
        }
        let record = crate::outlook::track_record(&instruments, horizon, today);
        if let Ok(mut cache) = self.track_cache.lock() {
            if cache.len() >= 16 {
                cache.clear();
            }
            cache.insert(version, record.clone());
        }
        Ok(record)
    }

    fn signal_name(signal: crate::signals::SignalId) -> String {
        serde_json::to_value(signal)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// Stored values of the enabled models for `held` instruments, from each model's
    /// `evaluate_from` on.
    pub(super) fn external_scores(
        &self,
        held: &[&str],
    ) -> Result<Vec<crate::signals::ExternalScores>, ServiceError> {
        let mut out = Vec::new();
        for spec in &self.model_specs {
            let (_, values) = self.store.signal_values(&Self::signal_name(spec.signal))?;
            let values: Vec<_> = values
                .into_iter()
                .filter(|(key, session, _)| {
                    *session >= spec.evaluate_from && held.contains(&key.as_str())
                })
                .collect();
            if !values.is_empty() {
                out.push(crate::signals::ExternalScores {
                    signal: spec.signal,
                    values,
                });
            }
        }
        Ok(out)
    }

    /// What each model signal is, whether it is on, and what is stored.
    pub fn models_status(&self) -> Result<ModelsStatus, ServiceError> {
        let coverage = self.store.signal_value_coverage()?;
        let runs = self
            .model_runs
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default();
        let models = crate::models::specs()
            .into_iter()
            .map(|spec| {
                let name = Self::signal_name(spec.signal);
                ModelStatus {
                    enabled: self.model_specs.iter().any(|s| s.name == spec.name),
                    last_run: runs.get(&spec.name).cloned(),
                    stored: coverage.iter().rfind(|c| c.signal == name).cloned(),
                    spec,
                }
            })
            .collect();
        let counts = self.store.outlook_path_counts()?;
        let outlook = crate::models::outlook_specs()
            .into_iter()
            .map(|spec| {
                let name = serde_json::to_value(spec.component)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                OutlookModelStatus {
                    enabled: self.outlook_specs.iter().any(|s| s.name == spec.name),
                    last_run: runs.get(&format!("outlook:{}", spec.name)).cloned(),
                    stored: counts
                        .iter()
                        .rfind(|(c, ..)| *c == name)
                        .map(|(_, v, i, n)| (v.clone(), *i, *n)),
                    spec,
                }
            })
            .collect();
        Ok(ModelsStatus {
            running: self.model_refreshing.try_lock().is_err(),
            models,
            outlook,
        })
    }

    /// Runs every enabled model helper over the held instruments, for the origins not
    /// stored yet, and stores the values. Slow the first time (it downloads weights and
    /// scores years of weekly origins); later runs only add new weeks.
    pub async fn refresh_model_signals(
        &self,
        account: &AccountId,
    ) -> Result<ModelsStatus, ServiceError> {
        let _running = self.model_refreshing.lock().await;
        let (python, scripts, cache) = self.helper_paths.clone();
        let held = self.held_instruments(account)?;
        let now = Utc::now();
        for spec in self.model_specs.clone() {
            let signal = Self::signal_name(spec.signal);
            let (_, stored) = self.store.signal_values(&signal)?;
            let done: std::collections::BTreeSet<(String, chrono::NaiveDate)> =
                stored.into_iter().map(|(k, d, _)| (k, d)).collect();
            let mut items = Vec::new();
            let mut requested = 0u32;
            for (key, _) in &held {
                let observations = self.store.prices(key, now)?;
                if spec.needs_ohl && observations.iter().all(|o| o.ohl.is_none()) {
                    continue;
                }
                let sessions: Vec<chrono::NaiveDate> =
                    observations.iter().map(|o| o.session).collect();
                let origins: Vec<chrono::NaiveDate> =
                    crate::models::origins(&sessions, spec.evaluate_from)
                        .into_iter()
                        .filter(|d| !done.contains(&(key.clone(), *d)))
                        .collect();
                if origins.is_empty() {
                    continue;
                }
                requested += origins.len() as u32;
                use rust_decimal::prelude::ToPrimitive;
                let bars = observations
                    .iter()
                    .filter_map(|o| {
                        let close = o.close.to_f64()?;
                        let (open, high, low) = match &o.ohl {
                            Some(ohl) => (ohl.open.to_f64(), ohl.high.to_f64(), ohl.low.to_f64()),
                            None => (None, None, None),
                        };
                        Some((o.session, open, high, low, close))
                    })
                    .collect();
                items.push(crate::models::HelperItem {
                    key: key.clone(),
                    origins,
                    bars,
                });
            }
            let run = if items.is_empty() {
                ModelRun {
                    at: Utc::now(),
                    model_version: None,
                    requested: 0,
                    added: 0,
                    skipped: 0,
                    error: None,
                    notes: vec!["Nothing new to compute.".into()],
                }
            } else {
                match crate::models::run_helper(&spec, &python, &scripts, Some(&cache), &items)
                    .await
                {
                    Ok(output) => {
                        let added = self.store.record_signal_values(
                            &signal,
                            &output.model_version,
                            &output.values,
                            Utc::now(),
                        )?;
                        ModelRun {
                            at: Utc::now(),
                            model_version: Some(output.model_version),
                            requested,
                            added,
                            skipped: output.skipped,
                            error: None,
                            notes: output.notes,
                        }
                    }
                    Err(e) => ModelRun {
                        at: Utc::now(),
                        model_version: None,
                        requested,
                        added: 0,
                        skipped: 0,
                        error: Some(e.to_string()),
                        notes: Vec::new(),
                    },
                }
            };
            if let Ok(mut runs) = self.model_runs.lock() {
                runs.insert(spec.name.clone(), run);
            }
        }
        for spec in self.outlook_specs.clone() {
            let (_, stored) = self.store.outlook_paths(spec.component, None)?;
            let done: std::collections::BTreeSet<(String, chrono::NaiveDate)> = stored
                .into_iter()
                .map(|p| (p.instrument_key, p.origin))
                .collect();
            let mut items = Vec::new();
            let mut requested = 0u32;
            for (key, _) in &held {
                let observations = self.store.prices(key, now)?;
                let sessions: Vec<chrono::NaiveDate> =
                    observations.iter().map(|o| o.session).collect();
                let origins: Vec<chrono::NaiveDate> =
                    crate::models::outlook_origins(&sessions, spec.evaluate_from)
                        .into_iter()
                        .filter(|d| !done.contains(&(key.clone(), *d)))
                        .collect();
                if origins.is_empty() {
                    continue;
                }
                requested += origins.len() as u32;
                use rust_decimal::prelude::ToPrimitive;
                let bars = observations
                    .iter()
                    .filter_map(|o| Some((o.session, None, None, None, o.close.to_f64()?)))
                    .collect();
                items.push(crate::models::HelperItem {
                    key: key.clone(),
                    origins,
                    bars,
                });
            }
            let run = if items.is_empty() {
                ModelRun {
                    at: Utc::now(),
                    model_version: None,
                    requested: 0,
                    added: 0,
                    skipped: 0,
                    error: None,
                    notes: vec!["Nothing new to compute.".into()],
                }
            } else {
                match crate::models::run_outlook_helper(
                    &spec,
                    &python,
                    &scripts,
                    Some(&cache),
                    &items,
                )
                .await
                {
                    Ok((version, paths, skipped, notes)) => {
                        let added =
                            self.store
                                .record_outlook_paths(&version, &paths, Utc::now())?;
                        ModelRun {
                            at: Utc::now(),
                            model_version: Some(version),
                            requested,
                            added,
                            skipped,
                            error: None,
                            notes,
                        }
                    }
                    Err(e) => ModelRun {
                        at: Utc::now(),
                        model_version: None,
                        requested,
                        added: 0,
                        skipped: 0,
                        error: Some(e.to_string()),
                        notes: Vec::new(),
                    },
                }
            };
            if let Ok(mut runs) = self.model_runs.lock() {
                runs.insert(format!("outlook:{}", spec.name), run);
            }
        }
        self.models_status()
    }
}
