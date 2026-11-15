//! Round 24: stored inputs and results of the income maximizer: SEC company facts per
//! issuer, dividend calibration reports and the owner's dividend candidates. All
//! append-only.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

use super::CapitalStore;
use super::{StoreError, decode, encode, parse_ts};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS company_facts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cik INTEGER NOT NULL,
    fetched_at TEXT NOT NULL,
    facts_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS company_facts_by_cik ON company_facts (cik, id);
CREATE TRIGGER IF NOT EXISTS company_facts_no_update BEFORE UPDATE ON company_facts
BEGIN SELECT RAISE(ABORT, 'company facts are append-only'); END;
CREATE TRIGGER IF NOT EXISTS company_facts_no_delete BEFORE DELETE ON company_facts
BEGIN SELECT RAISE(ABORT, 'company facts are append-only'); END;
CREATE TABLE IF NOT EXISTS dividend_calibrations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    computed_at TEXT NOT NULL,
    report_json TEXT NOT NULL
);
CREATE TRIGGER IF NOT EXISTS dividend_calibrations_no_update BEFORE UPDATE ON dividend_calibrations
BEGIN SELECT RAISE(ABORT, 'calibrations are append-only'); END;
CREATE TRIGGER IF NOT EXISTS dividend_calibrations_no_delete BEFORE DELETE ON dividend_calibrations
BEGIN SELECT RAISE(ABORT, 'calibrations are append-only'); END;
CREATE TABLE IF NOT EXISTS dividend_candidates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol TEXT NOT NULL,
    enabled INTEGER NOT NULL,
    changed_at TEXT NOT NULL
);
CREATE TRIGGER IF NOT EXISTS dividend_candidates_no_update BEFORE UPDATE ON dividend_candidates
BEGIN SELECT RAISE(ABORT, 'candidate changes are append-only'); END;
CREATE TRIGGER IF NOT EXISTS dividend_candidates_no_delete BEFORE DELETE ON dividend_candidates
BEGIN SELECT RAISE(ABORT, 'candidate changes are append-only'); END;
";

impl CapitalStore {
    /// Stores one fetch of an issuer's company facts.
    pub fn record_company_facts(
        &self,
        facts: &crate::sec::companyfacts::CompanyFacts,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        conn.execute(
            "INSERT INTO company_facts (cik, fetched_at, facts_json) VALUES (?1, ?2, ?3)",
            params![facts.cik as i64, now.to_rfc3339(), encode(facts)?],
        )?;
        Ok(())
    }

    /// The newest company facts stored for `cik`, with when they were fetched.
    pub fn company_facts(
        &self,
        cik: u64,
    ) -> Result<Option<(DateTime<Utc>, crate::sec::companyfacts::CompanyFacts)>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT fetched_at, facts_json FROM company_facts WHERE cik = ?1
                 ORDER BY id DESC LIMIT 1",
                params![cik as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(at, json)| Ok((parse_ts(&at)?, decode(&json)?)))
            .transpose()
    }

    /// Stores a calibration report.
    pub fn record_dividend_calibration(
        &self,
        report: &crate::dividends::calibrate::CalibrationReport,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        conn.execute(
            "INSERT INTO dividend_calibrations (computed_at, report_json) VALUES (?1, ?2)",
            params![now.to_rfc3339(), encode(report)?],
        )?;
        Ok(())
    }

    /// The newest calibration report, with when it was computed.
    pub fn dividend_calibration(
        &self,
    ) -> Result<
        Option<(
            DateTime<Utc>,
            crate::dividends::calibrate::CalibrationReport,
        )>,
        StoreError,
    > {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT computed_at, report_json FROM dividend_calibrations
                 ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(at, json)| Ok((parse_ts(&at)?, decode(&json)?)))
            .transpose()
    }

    /// Marks or unmarks a watchlist symbol as a dividend candidate.
    pub fn record_dividend_candidate(
        &self,
        symbol: &str,
        enabled: bool,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        conn.execute(
            "INSERT INTO dividend_candidates (symbol, enabled, changed_at) VALUES (?1, ?2, ?3)",
            params![
                symbol.to_ascii_uppercase(),
                enabled as i64,
                now.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Symbols currently marked as dividend candidates (the latest change per symbol
    /// wins), sorted.
    pub fn dividend_candidates(&self) -> Result<Vec<String>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt =
            conn.prepare("SELECT symbol, enabled FROM dividend_candidates ORDER BY id ASC")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut state = std::collections::BTreeMap::new();
        for row in rows {
            let (symbol, enabled) = row?;
            state.insert(symbol, enabled != 0);
        }
        Ok(state
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(s, _)| s)
            .collect())
    }
}
