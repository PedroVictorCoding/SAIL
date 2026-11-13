//! Round 20: frozen Signal v2 baseline forecast records (Claude's slice).
//!
//! Append-only and content-addressed: writing the same record again is a no-op; a later
//! correction or model version gets a new `record_id`. Outcomes are scored separately and
//! never mutate these rows.

use super::{CapitalStore, StoreError, encode};
use crate::forecast::record::ForecastRecord;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use serde_json::{Number, Value};

/// The frozen forecast table. Claude, round 20.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS forecast_records (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    record_id TEXT NOT NULL UNIQUE,
    recorded_at TEXT NOT NULL,
    decision_at TEXT NOT NULL,
    decision_session TEXT NOT NULL,
    instrument_key TEXT NOT NULL,
    symbol TEXT NOT NULL,
    horizon_sessions INTEGER NOT NULL,
    model_version TEXT NOT NULL,
    input_hash TEXT NOT NULL,
    provenance TEXT NOT NULL,
    record_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS forecast_records_by_instrument
    ON forecast_records (instrument_key, horizon_sessions, decision_at);
CREATE TRIGGER IF NOT EXISTS forecast_records_no_update BEFORE UPDATE ON forecast_records
BEGIN SELECT RAISE(ABORT, 'forecast records are append-only'); END;
CREATE TRIGGER IF NOT EXISTS forecast_records_no_delete BEFORE DELETE ON forecast_records
BEGIN SELECT RAISE(ABORT, 'forecast records are append-only'); END;
";

impl CapitalStore {
    /// Stores a frozen record if its `record_id` is new. Returns whether a row was added.
    /// Claude, round 20.
    pub fn record_forecast(
        &self,
        record: &ForecastRecord,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        record.verify_id().map_err(StoreError::Decode)?;
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO forecast_records
             (record_id, recorded_at, decision_at, decision_session, instrument_key, symbol,
              horizon_sessions, model_version, input_hash, provenance, record_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                record.record_id,
                now.to_rfc3339(),
                record.decision.at.to_rfc3339(),
                record.decision.session.format("%Y-%m-%d").to_string(),
                record.instrument_key,
                record.symbol,
                record.horizon_sessions,
                record.model_version,
                record.input_hash,
                encode(&record.provenance)?,
                encode_record(record)?
            ],
        )?;
        Ok(inserted > 0)
    }

    /// One frozen record by content id. Claude, round 20.
    pub fn forecast_record(&self, record_id: &str) -> Result<Option<ForecastRecord>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let json: Option<String> = conn
            .query_row(
                "SELECT record_json FROM forecast_records WHERE record_id = ?1",
                params![record_id],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|json| decode_record(&json)).transpose()
    }

    /// A record's immutable content id by natural key and decision, when present.
    pub fn forecast_record_id(
        &self,
        instrument_key: &str,
        horizon_sessions: u32,
        decision_at: DateTime<Utc>,
    ) -> Result<Option<String>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(conn
            .query_row(
                "SELECT record_id FROM forecast_records
                 WHERE instrument_key = ?1 AND horizon_sessions = ?2 AND decision_at = ?3
                 ORDER BY id DESC LIMIT 1",
                params![instrument_key, horizon_sessions, decision_at.to_rfc3339()],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Every frozen record for one instrument and horizon, newest decision first.
    pub fn forecast_records_for(
        &self,
        instrument_key: &str,
        horizon_sessions: u32,
    ) -> Result<Vec<ForecastRecord>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT record_json FROM forecast_records
             WHERE instrument_key = ?1 AND horizon_sessions = ?2
             ORDER BY decision_at DESC, id DESC",
        )?;
        let rows = stmt.query_map(params![instrument_key, horizon_sessions], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|row| decode_record(&row?)).collect()
    }
}

fn decode_record(json: &str) -> Result<ForecastRecord, StoreError> {
    let mut value: Value =
        serde_json::from_str(json).map_err(|error| StoreError::Decode(error.to_string()))?;
    decode_floats(&mut value)?;
    let record: ForecastRecord =
        serde_json::from_value(value).map_err(|error| StoreError::Decode(error.to_string()))?;
    record.verify_id().map_err(StoreError::Decode)?;
    Ok(record)
}

/// JSON with every non-integer number replaced by `"f64:<16 hex bits>"`. JSON's shortest
/// float parsing can lose the last bit under serde_json; a frozen record must survive a
/// store round trip bit-for-bit.
fn encode_record(record: &ForecastRecord) -> Result<String, StoreError> {
    let mut value =
        serde_json::to_value(record).map_err(|error| StoreError::Decode(error.to_string()))?;
    encode_floats(&mut value);
    serde_json::to_string(&value).map_err(|error| StoreError::Decode(error.to_string()))
}

fn encode_floats(value: &mut Value) {
    match value {
        Value::Number(number) if number.is_f64() => {
            if let Some(float) = number.as_f64() {
                *value = Value::String(format!("f64:{:016x}", float.to_bits()));
            }
        }
        Value::Array(values) => {
            for value in values {
                encode_floats(value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                encode_floats(value);
            }
        }
        _ => {}
    }
}

fn decode_floats(value: &mut Value) -> Result<(), StoreError> {
    match value {
        Value::String(text) => {
            if let Some(bits) = text.strip_prefix("f64:") {
                let bits = u64::from_str_radix(bits, 16)
                    .map_err(|_| StoreError::Decode("bad stored float bits".into()))?;
                let number = Number::from_f64(f64::from_bits(bits))
                    .ok_or_else(|| StoreError::Decode("stored float was not finite".into()))?;
                *value = Value::Number(number);
            }
        }
        Value::Array(values) => {
            for value in values {
                decode_floats(value)?;
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                decode_floats(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}
