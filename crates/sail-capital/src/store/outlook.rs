//! Stored outlook component paths from the Python helpers, and their counts.

use super::CapitalStore;
use super::{decode, encode, StoreError};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

impl CapitalStore {
    /// Stores outlook component paths; ones already stored for the same component,
    /// model version, instrument, origin and horizon are kept. Returns how many were new.
    pub fn record_outlook_paths(
        &self,
        model_version: &str,
        paths: &[crate::outlook::ExternalPath],
        now: DateTime<Utc>,
    ) -> Result<u32, StoreError> {
        let mut conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let tx = conn.transaction()?;
        let mut added = 0u32;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO outlook_paths
                 (component, model_version, instrument_key, origin, horizon, path_json, computed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for path in paths {
                added += stmt.execute(params![
                    encode(&path.component)?.trim_matches('"'),
                    model_version,
                    path.instrument_key,
                    path.origin.format("%Y-%m-%d").to_string(),
                    path.horizon,
                    encode(&path.path)?,
                    now.to_rfc3339()
                ])? as u32;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// Paths of `component` from its most recently computed model version, for
    /// `instrument_key` (all instruments when `None`), oldest origin first.
    pub fn outlook_paths(
        &self,
        component: crate::outlook::ComponentId,
        instrument_key: Option<&str>,
    ) -> Result<(Option<String>, Vec<crate::outlook::ExternalPath>), StoreError> {
        let name = encode(&component)?.trim_matches('"').to_string();
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let version: Option<String> = conn
            .query_row(
                "SELECT model_version FROM outlook_paths WHERE component = ?1
                 ORDER BY computed_at DESC LIMIT 1",
                params![name],
                |r| r.get(0),
            )
            .optional()?;
        let Some(version) = version else {
            return Ok((None, Vec::new()));
        };
        let mut stmt = conn.prepare(
            "SELECT instrument_key, origin, horizon, path_json FROM outlook_paths
             WHERE component = ?1 AND model_version = ?2
               AND (?3 IS NULL OR instrument_key = ?3)
             ORDER BY origin, instrument_key, horizon",
        )?;
        let rows = stmt.query_map(params![name, version, instrument_key], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u32>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (key, origin, horizon, json) = row?;
            let origin = chrono::NaiveDate::parse_from_str(&origin, "%Y-%m-%d")
                .map_err(|_| StoreError::Decode(format!("bad origin {origin}")))?;
            out.push(crate::outlook::ExternalPath {
                component,
                instrument_key: key,
                origin,
                horizon,
                path: decode(&json)?,
            });
        }
        Ok((Some(version), out))
    }

    /// Paths stored, per component: (component, model version, instruments, paths).
    pub fn outlook_path_counts(&self) -> Result<Vec<(String, String, u32, u32)>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT component, model_version, COUNT(DISTINCT instrument_key), COUNT(*)
             FROM outlook_paths GROUP BY component, model_version",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}
