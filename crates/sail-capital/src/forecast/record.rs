//! Frozen Signal v2 baseline forecast records and their maturity scoring.
//!
//! A record is written **before** an outcome exists. It contains the baseline features,
//! the exact [`ForecastDistribution`], the decision time, the model version, the replay
//! input hash and the input provenance. `record_id` is a content hash; writing the same
//! record again is a no-op, while a corrected input or model version creates a new record.
//!
//! Scoring is deliberately separate: the caller supplies the next-tradable open, the
//! horizon close, split/dividend adjustments and an explicit delisting flag. The module is
//! pure: no I/O and no clock.

use crate::contracts::{DecisionTime, InputProvenance, sha256_hex};
use crate::forecast::baseline::BaselineFeatures;
use crate::forecast::{ForecastDistribution, ForecastStatus};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Version of the record envelope.
pub const RECORD_SCHEMA_VERSION: &str = "capital-forecast-record/1";

/// Baseline model version used by the shadow record.
pub const BASELINE_MODEL_VERSION: &str = "capital-baseline/1";

/// A frozen forecast and the inputs that produced it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForecastRecord {
    /// [`RECORD_SCHEMA_VERSION`].
    pub schema_version: String,
    /// Deterministic content identity (excluding the write time).
    pub record_id: String,
    /// Explicit point-in-time decision.
    pub decision: DecisionTime,
    /// Ledger or watchlist key.
    pub instrument_key: String,
    /// Display/broker symbol.
    pub symbol: String,
    /// Horizon in sessions.
    pub horizon_sessions: u32,
    /// Baseline model version.
    pub model_version: String,
    /// Frozen feature values.
    pub features: BaselineFeatures,
    /// The forecast itself.
    pub distribution: ForecastDistribution,
    /// Deterministic replay input manifest hash from the strict replay layer.
    pub input_hash: String,
    /// Whether the inputs were observed at the decision or reconstructed later.
    pub provenance: InputProvenance,
}

#[derive(Serialize)]
struct RecordIdentity<'a> {
    schema_version: &'a str,
    decision: DecisionTime,
    instrument_key: &'a str,
    symbol: &'a str,
    horizon_sessions: u32,
    model_version: &'a str,
    features: &'a BaselineFeatures,
    distribution: &'a ForecastDistribution,
    input_hash: &'a str,
    provenance: InputProvenance,
}

impl ForecastRecord {
    /// Builds a record and computes its content identity. Rejects a distribution that does
    /// not describe the same instrument, session and horizon.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        decision: DecisionTime,
        instrument_key: impl Into<String>,
        symbol: impl Into<String>,
        horizon_sessions: u32,
        model_version: impl Into<String>,
        features: BaselineFeatures,
        distribution: ForecastDistribution,
        input_hash: impl Into<String>,
        provenance: InputProvenance,
    ) -> Result<Self, String> {
        let instrument_key = instrument_key.into();
        let symbol = symbol.into();
        let model_version = model_version.into();
        let input_hash = input_hash.into();
        if instrument_key.trim().is_empty() || symbol.trim().is_empty() {
            return Err("the record needs an instrument key and symbol".into());
        }
        if model_version.trim().is_empty() {
            return Err("the record needs a model version".into());
        }
        if input_hash.trim().is_empty() {
            return Err("the record needs a replay input hash".into());
        }
        if features.as_of_session != decision.session {
            return Err("features are not from the decision session".into());
        }
        if distribution.instrument_key != instrument_key
            || distribution.symbol != symbol
            || distribution.horizon_sessions != horizon_sessions
            || distribution.as_of_session != decision.session
        {
            return Err("the forecast distribution does not match the record".into());
        }
        let mut record = Self {
            schema_version: RECORD_SCHEMA_VERSION.to_string(),
            record_id: String::new(),
            decision,
            instrument_key,
            symbol,
            horizon_sessions,
            model_version,
            features,
            distribution,
            input_hash,
            provenance,
        };
        record.record_id = record.compute_id()?;
        Ok(record)
    }

    /// Recomputes the content identity. Used by the store before accepting a row.
    pub fn verify_id(&self) -> Result<(), String> {
        let expected = self.compute_id()?;
        if self.record_id != expected {
            return Err("the forecast record id does not match its content".into());
        }
        Ok(())
    }

    fn compute_id(&self) -> Result<String, String> {
        let identity = RecordIdentity {
            schema_version: &self.schema_version,
            decision: self.decision,
            instrument_key: &self.instrument_key,
            symbol: &self.symbol,
            horizon_sessions: self.horizon_sessions,
            model_version: &self.model_version,
            features: &self.features,
            distribution: &self.distribution,
            input_hash: &self.input_hash,
            provenance: self.provenance,
        };
        let bytes = serde_json::to_vec(&identity)
            .map_err(|error| format!("could not canonicalize the forecast record: {error}"))?;
        Ok(sha256_hex(&bytes))
    }
}

/// The realized outcome supplied after maturity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RealizedOutcome {
    /// Session whose open is the entry.
    pub entry_session: NaiveDate,
    /// Session whose close is the exit.
    pub exit_session: NaiveDate,
    /// Next-tradable open.
    pub entry_open: f64,
    /// Exit close, before split adjustment and cash distributions.
    pub exit_close: f64,
    /// Share-count/split factor applicable to the exit price (1.0 when already adjusted).
    pub split_factor: f64,
    /// Cash per share paid between entry and exit, not reinvested.
    pub cash_per_share: f64,
    /// The outcome reached the horizon; false means an early/delisted stop.
    pub horizon_complete: bool,
    /// The instrument delisted before its horizon.
    pub delisted: bool,
}

/// A scored forecast, kept separate from the frozen record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoredForecast {
    /// The record this score belongs to.
    pub record_id: String,
    /// Realized log total return from the next open to the exit close.
    pub realized_log_return: f64,
    /// Expected minus realized log return, when the forecast published a mean.
    pub forecast_error: Option<f64>,
    /// Realized return inside the published 90% band, when that band exists.
    pub inside_90: Option<bool>,
    /// Realized return inside the published 50% band (the interquartile band), when present.
    pub inside_50: Option<bool>,
    /// Expected and realized directions agree (zero outcomes are not scored).
    pub direction_hit: Option<bool>,
    /// The outcome was complete, early or a delisting.
    pub status: ScoreStatus,
}

/// Why an outcome is not a complete horizon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreStatus {
    /// Reached the horizon.
    Complete,
    /// Stopped before the horizon without a delisting.
    Early,
    /// Delisted before the horizon; proceeds are included when supplied.
    Delisted,
}

/// Scores one frozen record against one supplied outcome.
pub fn score(record: &ForecastRecord, outcome: &RealizedOutcome) -> Result<ScoredForecast, String> {
    if outcome.exit_session < outcome.entry_session {
        return Err("the exit session precedes the entry session".into());
    }
    if !outcome.entry_open.is_finite() || outcome.entry_open <= 0.0 {
        return Err("the entry open must be positive and finite".into());
    }
    if !outcome.exit_close.is_finite() || outcome.exit_close < 0.0 {
        return Err("the exit close must be finite and non-negative".into());
    }
    if !outcome.split_factor.is_finite() || outcome.split_factor <= 0.0 {
        return Err("the split factor must be positive and finite".into());
    }
    if !outcome.cash_per_share.is_finite() || outcome.cash_per_share < 0.0 {
        return Err("cash per share must be finite and non-negative".into());
    }
    let total_value = outcome.exit_close * outcome.split_factor + outcome.cash_per_share;
    if total_value <= 0.0 {
        return Err("the outcome has no positive terminal value".into());
    }
    let realized_log_return = (total_value / outcome.entry_open).ln();
    let expected = record.distribution.expected_log_return;
    let forecast_error = expected.map(|value| value - realized_log_return);
    let inside_90 = band(&record.distribution, 0.05, 0.95, realized_log_return);
    let inside_50 = band(&record.distribution, 0.25, 0.75, realized_log_return);
    let direction_hit = expected.and_then(|value| {
        if value == 0.0 || realized_log_return == 0.0 {
            None
        } else {
            Some(value.signum() == realized_log_return.signum())
        }
    });
    let status = if outcome.delisted {
        ScoreStatus::Delisted
    } else if outcome.horizon_complete {
        ScoreStatus::Complete
    } else {
        ScoreStatus::Early
    };
    Ok(ScoredForecast {
        record_id: record.record_id.clone(),
        realized_log_return,
        forecast_error,
        inside_90,
        inside_50,
        direction_hit,
        status,
    })
}

fn band(distribution: &ForecastDistribution, low: f64, high: f64, realized: f64) -> Option<bool> {
    if distribution.status != ForecastStatus::Ok {
        return None;
    }
    let low = distribution
        .quantiles
        .iter()
        .find(|q| (q.p - low).abs() < 1e-9)?;
    let high = distribution
        .quantiles
        .iter()
        .find(|q| (q.p - high).abs() < 1e-9)?;
    Some(realized >= low.log_return && realized <= high.log_return)
}
