//! Round 25: parsed SEC Form 4s, one row per accession, append-only. A filing that
//! failed to parse is recorded too (with its error), so it isn't fetched again every run.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rusqlite::params;

use super::CapitalStore;
use super::{StoreError, decode, encode};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS form4_filings (
    accession TEXT PRIMARY KEY,
    cik INTEGER NOT NULL,
    published_at TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    form4_json TEXT,
    error TEXT
);
CREATE INDEX IF NOT EXISTS form4_by_cik ON form4_filings (cik, published_at);
CREATE TRIGGER IF NOT EXISTS form4_no_update BEFORE UPDATE ON form4_filings
BEGIN SELECT RAISE(ABORT, 'Form 4s are append-only'); END;
CREATE TRIGGER IF NOT EXISTS form4_no_delete BEFORE DELETE ON form4_filings
BEGIN SELECT RAISE(ABORT, 'Form 4s are append-only'); END;
";

impl CapitalStore {
    /// Stores a parsed Form 4 for `filing`, or the reason it couldn't be read. A second
    /// call for the same accession is ignored. Returns whether it was new.
    pub fn record_form4(
        &self,
        filing: &crate::evidence::Filing,
        parsed: Result<&crate::sec::form4::Form4, &str>,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let (json, error) = match parsed {
            Ok(form4) => (Some(encode(form4)?), None),
            Err(e) => (None, Some(e.chars().take(300).collect::<String>())),
        };
        let added = conn.execute(
            "INSERT OR IGNORE INTO form4_filings
             (accession, cik, published_at, fetched_at, form4_json, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                filing.accession,
                i64::try_from(filing.cik.0).unwrap_or(i64::MAX),
                filing.published_at.to_rfc3339(),
                now.to_rfc3339(),
                json,
                error
            ],
        )?;
        Ok(added > 0)
    }

    /// Accessions already stored (parsed or failed).
    pub fn form4_accessions(&self) -> Result<HashSet<String>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare("SELECT accession FROM form4_filings")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = HashSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    }

    /// Parsed Form 4s of `cik` fetched by `known_by`, oldest published first.
    pub fn form4s(
        &self,
        cik: crate::evidence::Cik,
        known_by: DateTime<Utc>,
    ) -> Result<Vec<crate::sec::form4::Form4>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT form4_json FROM form4_filings
             WHERE cik = ?1 AND form4_json IS NOT NULL AND fetched_at <= ?2
             ORDER BY published_at, accession",
        )?;
        let rows = stmt.query_map(
            params![
                i64::try_from(cik.0).unwrap_or(i64::MAX),
                known_by.to_rfc3339()
            ],
            |r| r.get::<_, String>(0),
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(decode(&row?)?);
        }
        Ok(out)
    }

    /// `(parsed, failed)` Form 4 counts, for the health panel.
    pub fn form4_counts(&self) -> Result<(u32, u32), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(conn.query_row(
            "SELECT COUNT(form4_json), COUNT(error) FROM form4_filings",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }
}
