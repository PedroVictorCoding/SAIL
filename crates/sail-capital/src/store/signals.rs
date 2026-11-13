//! Stored model-signal values, keyed by signal, model version, instrument and session.

use super::CapitalStore;
use super::{StoreError, parse_ts};
use crate::contracts::{DecisionTime, InputProvenance};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// One stored model value: `(instrument key, session, value)`.
pub type SignalValue = (String, chrono::NaiveDate, f64);

/// Model signal values stored for one signal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignalValueCoverage {
    /// Signal, as its snake_case name.
    pub signal: String,
    /// Model version.
    pub model_version: String,
    /// Instruments with values.
    pub instruments: u32,
    /// Values stored.
    pub values: u32,
    /// Latest session with a value.
    pub latest: Option<chrono::NaiveDate>,
}

impl CapitalStore {
    /// Stores model signal values; values already stored for the same signal, model
    /// version, instrument and session are kept as they were. Returns how many were new.
    pub fn record_signal_values(
        &self,
        signal: &str,
        model_version: &str,
        values: &[(String, chrono::NaiveDate, f64)],
        now: DateTime<Utc>,
    ) -> Result<u32, StoreError> {
        let mut conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let tx = conn.transaction()?;
        let mut added = 0u32;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO signal_values
                 (signal, model_version, instrument_key, session, value, computed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for (key, session, value) in values {
                if !value.is_finite() {
                    continue;
                }
                added += stmt.execute(params![
                    signal,
                    model_version,
                    key,
                    session.format("%Y-%m-%d").to_string(),
                    value,
                    now.to_rfc3339()
                ])? as u32;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// Values of `signal` from its most recently computed model version, as
    /// `(instrument key, session, value)`, oldest first.
    pub fn signal_values(
        &self,
        signal: &str,
    ) -> Result<(Option<String>, Vec<SignalValue>), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let version: Option<String> = conn
            .query_row(
                "SELECT model_version FROM signal_values WHERE signal = ?1
                 ORDER BY computed_at DESC LIMIT 1",
                params![signal],
                |r| r.get(0),
            )
            .optional()?;
        let Some(version) = version else {
            return Ok((None, Vec::new()));
        };
        let mut stmt = conn.prepare(
            "SELECT instrument_key, session, value FROM signal_values
             WHERE signal = ?1 AND model_version = ?2 ORDER BY session, instrument_key",
        )?;
        let rows = stmt.query_map(params![signal, version], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (key, session, value) = row?;
            let session = chrono::NaiveDate::parse_from_str(&session, "%Y-%m-%d")
                .map_err(|_| StoreError::Decode(format!("bad session {session}")))?;
            out.push((key, session, value));
        }
        Ok((Some(version), out))
    }

    /// Values of `signal` from the newest model version computed at or before `decision.at`,
    /// keeping only rows with `computed_at <= decision.at` and `session <= decision.session`.
    pub fn signal_values_as_of(
        &self,
        signal: &str,
        decision: DecisionTime,
    ) -> Result<(Option<String>, Vec<SignalValue>, InputProvenance), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT model_version, instrument_key, session, value, computed_at
             FROM signal_values
             WHERE signal = ?1
             ORDER BY session, instrument_key",
        )?;
        let rows = stmt.query_map(params![signal], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;

        let mut latest_version: Option<(DateTime<Utc>, String)> = None;
        let mut eligible: Vec<(String, String, chrono::NaiveDate, f64)> = Vec::new();
        for row in rows {
            let (version, key, session_str, value, computed_str) = row?;
            let computed_at = parse_ts(&computed_str)?;
            if computed_at > decision.at {
                continue;
            }
            let session = chrono::NaiveDate::parse_from_str(&session_str, "%Y-%m-%d")
                .map_err(|_| StoreError::Decode(format!("bad session {session_str}")))?;
            if session > decision.session {
                continue;
            }
            if latest_version
                .as_ref()
                .is_none_or(|(prev_ts, _)| computed_at > *prev_ts)
            {
                latest_version = Some((computed_at, version.clone()));
            }
            eligible.push((version, key, session, value));
        }

        let Some((_, version)) = latest_version else {
            return Ok((None, Vec::new(), InputProvenance::Unavailable));
        };
        let out: Vec<SignalValue> = eligible
            .into_iter()
            .filter(|(v, ..)| v == &version)
            .map(|(_, key, session, value)| (key, session, value))
            .collect();
        let prov = if out.is_empty() {
            InputProvenance::Unavailable
        } else {
            InputProvenance::Observed
        };
        Ok((Some(version), out, prov))
    }

    /// What is stored per model signal.
    pub fn signal_value_coverage(&self) -> Result<Vec<SignalValueCoverage>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT signal, model_version, COUNT(DISTINCT instrument_key), COUNT(*), MAX(session)
             FROM signal_values GROUP BY signal, model_version ORDER BY signal, model_version",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SignalValueCoverage {
                signal: r.get(0)?,
                model_version: r.get(1)?,
                instruments: r.get(2)?,
                values: r.get(3)?,
                latest: r
                    .get::<_, Option<String>>(4)?
                    .and_then(|s| chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}
