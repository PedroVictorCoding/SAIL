//! Public research evidence: SEC filings and the ticker maps they were matched with.
//! Evidence is not per account: a filing is public, and which holdings it touches is
//! decided at query time.

use super::CapitalStore;
use super::{decode, encode, parse_ts, StoreError};
use crate::contracts::*;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};


/// Public research evidence and the identifier maps it was matched with. Evidence is not
/// per account: a filing is public, and which holdings it touches is decided at query time.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ticker_maps (
    payload_sha256 TEXT PRIMARY KEY,
    fetched_at TEXT NOT NULL,
    index_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS evidence (
    id TEXT PRIMARY KEY,
    natural_key TEXT NOT NULL,
    source TEXT NOT NULL,
    cik INTEGER NOT NULL,
    published_at TEXT NOT NULL,
    first_observed_at TEXT NOT NULL,
    ingested_at TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    record_json TEXT NOT NULL,
    UNIQUE (natural_key, payload_sha256)
);
CREATE INDEX IF NOT EXISTS evidence_by_cik ON evidence (cik, published_at);
CREATE TRIGGER IF NOT EXISTS evidence_no_update BEFORE UPDATE ON evidence
BEGIN SELECT RAISE(ABORT, 'evidence is append-only'); END;
CREATE TRIGGER IF NOT EXISTS evidence_no_delete BEFORE DELETE ON evidence
BEGIN SELECT RAISE(ABORT, 'evidence is append-only'); END;
";

/// What appending one issuer's filings did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceOutcome {
    /// Filings listed in the response.
    pub seen: u32,
    /// New accessions, or new revisions of known ones.
    pub added: u32,
    /// Of `added`, how many were revisions of an accession already stored.
    pub revisions: u32,
}

impl CapitalStore {
    /// Keeps a fetched ticker map. The same bytes fetched again are stored once.
    pub fn record_ticker_index(
        &self,
        index: &crate::evidence::TickerIndex,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        conn.execute(
            "INSERT OR IGNORE INTO ticker_maps (payload_sha256, fetched_at, index_json)
             VALUES (?1, ?2, ?3)",
            params![
                index.payload_sha256,
                index.fetched_at.to_rfc3339(),
                encode(index)?
            ],
        )?;
        Ok(())
    }

    /// The newest ticker map fetched at or before `known_by`, so a historical question
    /// uses the identifiers SAIL had then.
    pub fn ticker_index(
        &self,
        known_by: DateTime<Utc>,
    ) -> Result<Option<crate::evidence::TickerIndex>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let json: Option<String> = conn
            .query_row(
                "SELECT index_json FROM ticker_maps WHERE fetched_at <= ?1
                 ORDER BY fetched_at DESC LIMIT 1",
                params![known_by.to_rfc3339()],
                |r| r.get(0),
            )
            .optional()?;
        json.map(|j| decode(&j)).transpose()
    }

    /// Appends the filings of one fetch, observed at `now`.
    ///
    /// - A new accession is stored with `first_observed_at = now`.
    /// - A known accession with identical content adds nothing.
    /// - A known accession with different content is stored as a **revision** that keeps
    ///   the first observation time, since SAIL knew of the filing since then; only its
    ///   content changed.
    pub fn record_filings(
        &self,
        filings: &crate::evidence::IssuerFilings,
        now: DateTime<Utc>,
    ) -> Result<EvidenceOutcome, StoreError> {
        use crate::evidence::{EvidenceRecord, SEC_SOURCE};
        let mut conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let tx = conn.transaction()?;
        let mut outcome = EvidenceOutcome::default();
        for filing in &filings.filings {
            outcome.seen += 1;
            let natural_key = format!("sec:{}", filing.accession);
            let payload = sha256_hex(encode(filing)?.as_bytes());
            let exists: bool = tx
                .query_row(
                    "SELECT 1 FROM evidence WHERE natural_key = ?1 AND payload_sha256 = ?2",
                    params![natural_key, payload],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if exists {
                continue;
            }
            let first_seen: Option<String> = tx
                .query_row(
                    "SELECT MIN(first_observed_at) FROM evidence WHERE natural_key = ?1",
                    params![natural_key],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let first_observed_at = match &first_seen {
                Some(text) => {
                    outcome.revisions += 1;
                    parse_ts(text)?
                }
                None => now,
            };
            let (issuer_name, _) = clean_cell_text(&filings.name);
            let record = EvidenceRecord {
                id: sha256_hex(format!("{natural_key}\n{payload}").as_bytes())[..16].to_string(),
                natural_key: natural_key.clone(),
                source: SEC_SOURCE.to_string(),
                period_end: filing.report_date,
                published_at: filing.published_at,
                first_observed_at,
                ingested_at: now,
                payload_sha256: payload.clone(),
                issuer_name,
                filing: filing.clone(),
            };
            tx.execute(
                "INSERT INTO evidence (id, natural_key, source, cik, published_at,
                 first_observed_at, ingested_at, payload_sha256, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    record.id,
                    natural_key,
                    record.source,
                    i64::try_from(filing.cik.0).unwrap_or(i64::MAX),
                    record.published_at.to_rfc3339(),
                    record.first_observed_at.to_rfc3339(),
                    now.to_rfc3339(),
                    payload,
                    encode(&record)?,
                ],
            )?;
            outcome.added += 1;
        }
        tx.commit()?;
        Ok(outcome)
    }

    /// Every stored evidence version for the given issuers, oldest ingest first. The
    /// exposure rules pick what is visible at a given `AsOf`.
    pub fn evidence_for(
        &self,
        ciks: &[crate::evidence::Cik],
    ) -> Result<Vec<crate::evidence::EvidenceRecord>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn
            .prepare("SELECT record_json FROM evidence WHERE cik = ?1 ORDER BY ingested_at, id")?;
        let mut out = Vec::new();
        for cik in ciks {
            let rows = stmt.query_map(params![i64::try_from(cik.0).unwrap_or(i64::MAX)], |r| {
                r.get::<_, String>(0)
            })?;
            for row in rows {
                out.push(decode(&row?)?);
            }
        }
        Ok(out)
    }
}
