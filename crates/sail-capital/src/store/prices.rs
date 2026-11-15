//! Daily price bars: one row per (instrument, source, session, version). A changed bar
//! for a known session is a revision that keeps the session's first availability time. A
//! revert to an older content is a new version too, so the append-only history can
//! represent it.

use super::CapitalStore;
use super::{StoreError, decode, encode, parse_ts};
use crate::contracts::*;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One row per (instrument, source, session, version). A changed bar for a known session
/// is a revision that keeps the session's first availability time. A revert to an older
/// content is a new version too, so the append-only history can represent it.
/// Decoded histories keyed by instrument, valid while no price is written. Reads "as of
/// now" (a `known_by` at or after the newest `ingested_at`) are served from here.
#[derive(Default)]
pub(super) struct PriceCache {
    /// `MAX(rowid)` of `price_bars` when the cache was filled; `None` until first used.
    version: Option<i64>,
    /// The newest `ingested_at` at that version.
    max_ingested: Option<String>,
    series: std::collections::HashMap<String, Arc<Vec<crate::market::MarketObservation>>>,
}

const PRICE_TABLE_COLUMNS: &str = "
    id TEXT PRIMARY KEY,
    instrument_key TEXT NOT NULL,
    source_id TEXT NOT NULL,
    session TEXT NOT NULL,
    bar_sha256 TEXT NOT NULL,
    first_available_at TEXT NOT NULL,
    ingested_at TEXT NOT NULL,
    observation_json TEXT NOT NULL
";

/// Creates the price schema, rebuilding a pre-release table that still had the old
/// `(instrument, source, session, content)` uniqueness rule. That rule would silently drop
/// a revision that reverts to an older close.
pub(super) fn init_price_schema(conn: &Connection) -> Result<(), StoreError> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'price_bars'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if existing
        .as_deref()
        .is_some_and(|sql| sql.contains("UNIQUE (instrument_key, source_id, session, bar_sha256)"))
    {
        conn.execute_batch(&format!(
            "BEGIN IMMEDIATE;
             DROP TRIGGER IF EXISTS price_bars_no_update;
             DROP TRIGGER IF EXISTS price_bars_no_delete;
             DROP INDEX IF EXISTS price_bars_by_instrument;
             ALTER TABLE price_bars RENAME TO price_bars_old;
             CREATE TABLE price_bars ({PRICE_TABLE_COLUMNS});
             INSERT INTO price_bars (id, instrument_key, source_id, session, bar_sha256,
                 first_available_at, ingested_at, observation_json)
             SELECT id, instrument_key, source_id, session, bar_sha256,
                 first_available_at, ingested_at, observation_json
             FROM price_bars_old ORDER BY session, ingested_at, id;
             DROP TABLE price_bars_old;
             COMMIT;"
        ))?;
    }
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS price_bars ({PRICE_TABLE_COLUMNS});
         CREATE INDEX IF NOT EXISTS price_bars_by_instrument ON price_bars (instrument_key, session);
         CREATE TRIGGER IF NOT EXISTS price_bars_no_update BEFORE UPDATE ON price_bars
         BEGIN SELECT RAISE(ABORT, 'price bars are append-only'); END;
         CREATE TRIGGER IF NOT EXISTS price_bars_no_delete BEFORE DELETE ON price_bars
         BEGIN SELECT RAISE(ABORT, 'price bars are append-only'); END;"
    ))?;
    Ok(())
}

/// What storing one series did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceOutcome {
    /// Bars in the series.
    pub seen: u32,
    /// Sessions stored for the first time.
    pub added: u32,
    /// Known sessions whose bar changed.
    pub revisions: u32,
}

/// One price bar revision returned by an as-of query, with its provenance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceBarRevision {
    /// Deterministic bar observation id (`price_bars.id`).
    pub id: String,
    /// Content hash of the bar (`price_bars.bar_sha256`).
    pub bar_sha256: String,
    /// The decoded market observation.
    pub observation: crate::market::MarketObservation,
    /// Whether this bar was observed by `decision.at` or reconstructed from a later archive.
    pub provenance: InputProvenance,
}

/// Stored prices for one instrument.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceCoverage {
    /// Ledger instrument key.
    pub instrument_key: String,
    /// Broker symbol.
    pub symbol: String,
    /// Distinct sessions.
    pub sessions: u32,
    /// First session.
    pub first: Option<chrono::NaiveDate>,
    /// Last session.
    pub last: Option<chrono::NaiveDate>,
    /// Sources seen, sorted.
    pub sources: Vec<String>,
}

impl CapitalStore {
    /// Stores `series` for `instrument_key`, observed at `now`. Idempotent: the same bars
    /// add nothing.
    pub fn record_prices(
        &self,
        instrument_key: &str,
        series: &crate::market::PriceSeries,
        now: DateTime<Utc>,
    ) -> Result<PriceOutcome, StoreError> {
        use crate::market::MarketObservation;
        let mut conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let tx = conn.transaction()?;
        let mut outcome = PriceOutcome::default();

        // One grouped read for the whole series instead of one query per bar. It keeps the
        // latest version and the session's first availability, so a refresh that only
        // repeats stored bars writes nothing.
        // (bar sha, first availability, whether the stored version has open/high/low,
        // whether it has a volume).
        let mut latest: std::collections::BTreeMap<String, (String, String, bool, bool)> =
            Default::default();
        {
            let mut stmt = tx.prepare(
                "SELECT session, bar_sha256, first_available_at,
                        json_extract(observation_json, '$.ohl') IS NOT NULL,
                        json_extract(observation_json, '$.volume') IS NOT NULL
                 FROM price_bars
                 WHERE instrument_key = ?1 AND source_id = ?2
                 ORDER BY session, ingested_at, rowid",
            )?;
            let rows = stmt.query_map(params![instrument_key, series.source_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, bool>(3)?,
                    r.get::<_, bool>(4)?,
                ))
            })?;
            for row in rows {
                let (session, bar_sha, first_available_at, has_ohl, has_volume) = row?;
                latest.insert(session, (bar_sha, first_available_at, has_ohl, has_volume));
            }
        }

        for bar in &series.bars {
            outcome.seen += 1;
            let session = bar.session.format("%Y-%m-%d").to_string();
            let content = encode(&(bar, &series.currency, series.adjustment))?;
            let bar_sha = sha256_hex(content.as_bytes());
            let brings_ohl = bar.open.is_some() && bar.high.is_some() && bar.low.is_some();
            // Round 24: a zero volume (FX, most indices) means "not reported".
            let volume = bar.volume.filter(|v| *v > rust_decimal::Decimal::ZERO);
            let brings_volume = volume.is_some();
            let (first_available_at, is_revision) = match latest.get(&session) {
                // content hash; a bar that now carries them is stored once as a revision.
                // Round 24: the same for a volume that earlier versions dropped.
                Some((stored_sha, _, has_ohl, has_volume))
                    if stored_sha == &bar_sha
                        && (*has_ohl || !brings_ohl)
                        && (*has_volume || !brings_volume) =>
                {
                    continue;
                }
                Some((_, first_available_at, ..)) => (parse_ts(first_available_at)?, true),
                None => (now, false),
            };
            let observation = MarketObservation {
                instrument_key: instrument_key.to_string(),
                symbol: series.symbol.clone(),
                session: bar.session,
                close: bar.close,
                ohl: match (bar.open, bar.high, bar.low) {
                    (Some(open), Some(high), Some(low)) => {
                        Some(crate::market::Ohl { open, high, low })
                    }
                    _ => None,
                },
                volume,
                currency: series.currency.clone(),
                price_kind: "close".to_string(),
                adjustment: series.adjustment,
                source_id: series.source_id.clone(),
                first_available_at,
                ingested_at: now,
            };
            // The ingest time is part of the id so a revision that reverts to an older
            // content is stored as a new version rather than being ignored.
            let id = sha256_hex(
                format!(
                    "{instrument_key}\n{}\n{session}\n{bar_sha}\n{}",
                    series.source_id,
                    now.to_rfc3339()
                )
                .as_bytes(),
            )[..16]
                .to_string();
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO price_bars (id, instrument_key, source_id, session,
                     bar_sha256, first_available_at, ingested_at, observation_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id,
                    instrument_key,
                    series.source_id,
                    session,
                    bar_sha,
                    first_available_at.to_rfc3339(),
                    now.to_rfc3339(),
                    encode(&observation)?
                ],
            )?;
            if inserted > 0 {
                if is_revision {
                    outcome.revisions += 1;
                } else {
                    outcome.added += 1;
                }
            }
            latest.insert(
                session,
                (
                    bar_sha,
                    first_available_at.to_rfc3339(),
                    brings_ohl,
                    brings_volume,
                ),
            );
        }
        tx.commit()?;
        drop(conn);
        self.invalidate_price_cache();
        Ok(outcome)
    }

    /// The closes for `instrument_key` as SAIL knew them at `known_by`: per session, the
    /// newest version stored by then (across sources, the most recently ingested wins),
    /// oldest session first.
    pub fn prices(
        &self,
        instrument_key: &str,
        known_by: DateTime<Utc>,
    ) -> Result<Vec<crate::market::MarketObservation>, StoreError> {
        self.prices_in_range(instrument_key, known_by, None, None)
    }

    /// Closes for `instrument_key` as known at `known_by`, optionally restricted to a
    /// session range. The range is applied in SQL so a narrow request never materializes
    /// the whole stored history.
    pub fn prices_in_range(
        &self,
        instrument_key: &str,
        known_by: DateTime<Utc>,
        from: Option<chrono::NaiveDate>,
        to: Option<chrono::NaiveDate>,
    ) -> Result<Vec<crate::market::MarketObservation>, StoreError> {
        let in_range = |o: &crate::market::MarketObservation| {
            from.is_none_or(|f| o.session >= f) && to.is_none_or(|t| o.session <= t)
        };
        if let Some(all) = self.cached_series(instrument_key, known_by)? {
            return Ok(all.iter().filter(|o| in_range(o)).cloned().collect());
        }
        self.query_prices(instrument_key, Some(known_by), from, to)
    }

    /// Closes for `instrument_key` at `decision`, with explicit provenance (`Observed` when
    /// available by `decision.at`, or `Reconstructed` when `policy == AllowReconstructed`
    /// and a session `<= decision.session` only has a later-ingested archive row).
    pub fn prices_with_provenance(
        &self,
        instrument_key: &str,
        decision: DecisionTime,
        policy: VintagePolicy,
    ) -> Result<Vec<PriceBarRevision>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT id, session, bar_sha256, first_available_at, ingested_at, observation_json
             FROM price_bars
             WHERE instrument_key = ?1 AND session <= ?2
             ORDER BY session, ingested_at, rowid",
        )?;
        let to = decision.session.format("%Y-%m-%d").to_string();
        let rows = stmt.query_map(params![instrument_key, to], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        type RowEntry = (String, String, DateTime<Utc>, DateTime<Utc>, String);
        let mut by_session: std::collections::BTreeMap<String, Vec<RowEntry>> = Default::default();
        for row in rows {
            let (id, session, bar_sha256, first_avail, ingested, json) = row?;
            let first_available_at = parse_ts(&first_avail)?;
            let ingested_at = parse_ts(&ingested)?;
            by_session.entry(session).or_default().push((
                id,
                bar_sha256,
                first_available_at,
                ingested_at,
                json,
            ));
        }

        let mut out = Vec::with_capacity(by_session.len());
        for (_, mut entries) in by_session {
            entries.sort_by_key(|e| e.3);
            if let Some((id, bar_sha256, _, _, json)) = entries
                .iter()
                .rev()
                .find(|e| e.3 <= decision.at && e.2 <= decision.at)
                .cloned()
            {
                out.push(PriceBarRevision {
                    id,
                    bar_sha256,
                    observation: decode(&json)?,
                    provenance: InputProvenance::Observed,
                });
            } else if policy == VintagePolicy::AllowReconstructed
                && let Some((id, bar_sha256, _, _, json)) = entries.into_iter().next()
            {
                out.push(PriceBarRevision {
                    id,
                    bar_sha256,
                    observation: decode(&json)?,
                    provenance: InputProvenance::Reconstructed,
                });
            }
        }
        Ok(out)
    }

    /// The whole decoded series from the cache when `known_by` sees every stored row
    /// (the usual "as of now" read), loading it once if needed. `None` for an earlier
    /// `known_by`, which must go to SQL to respect "known since".
    fn cached_series(
        &self,
        instrument_key: &str,
        known_by: DateTime<Utc>,
    ) -> Result<Option<Arc<Vec<crate::market::MarketObservation>>>, StoreError> {
        let mut cache = self.price_cache.lock().map_err(|_| StoreError::Poisoned)?;
        // Any write (from this store or another connection) moves the newest rowid; that
        // check is a single index lookup, so the cache can never serve stale bars.
        {
            let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
            let version: i64 =
                conn.query_row("SELECT IFNULL(MAX(rowid), 0) FROM price_bars", [], |r| {
                    r.get(0)
                })?;
            if cache.version != Some(version) {
                // Once per version: the newest ingest time decides which reads may use it.
                let mut stmt = conn.prepare("SELECT DISTINCT ingested_at FROM price_bars")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                let mut newest: Option<DateTime<Utc>> = None;
                for row in rows {
                    let ts = parse_ts(&row?)?;
                    newest = Some(newest.map_or(ts, |cur| cur.max(ts)));
                }
                *cache = PriceCache {
                    version: Some(version),
                    max_ingested: newest.map(|ts| ts.to_rfc3339()),
                    series: Default::default(),
                };
            }
        }
        if let Some(max_ingested) = cache.max_ingested.as_deref().filter(|s| !s.is_empty())
            && parse_ts(max_ingested)? > known_by
        {
            return Ok(None);
        }
        if let Some(series) = cache.series.get(instrument_key) {
            return Ok(Some(series.clone()));
        }
        let series = Arc::new(self.query_prices(instrument_key, None, None, None)?);
        cache
            .series
            .insert(instrument_key.to_string(), series.clone());
        Ok(Some(series))
    }

    fn query_prices(
        &self,
        instrument_key: &str,
        known_by: Option<DateTime<Utc>>,
        from: Option<chrono::NaiveDate>,
        to: Option<chrono::NaiveDate>,
    ) -> Result<Vec<crate::market::MarketObservation>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT session, ingested_at, observation_json FROM price_bars
             WHERE instrument_key = ?1
               AND (?2 IS NULL OR session >= ?2)
               AND (?3 IS NULL OR session <= ?3)
             ORDER BY session, ingested_at, rowid",
        )?;
        let from = from.map(|date| date.format("%Y-%m-%d").to_string());
        let to = to.map(|date| date.format("%Y-%m-%d").to_string());
        let rows = stmt.query_map(params![instrument_key, from, to], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut latest: std::collections::BTreeMap<String, (DateTime<Utc>, String)> =
            Default::default();
        for row in rows {
            let (session, ingested_at, json) = row?;
            let ingested = parse_ts(&ingested_at)?;
            if let Some(cutoff) = known_by
                && ingested > cutoff
            {
                continue;
            }
            match latest.get(&session) {
                Some((prev_ts, _)) if *prev_ts > ingested => {}
                _ => {
                    latest.insert(session, (ingested, json));
                }
            }
        }
        latest.values().map(|(_, json)| decode(json)).collect()
    }

    /// Forgets cached histories after a write.
    fn invalidate_price_cache(&self) {
        if let Ok(mut cache) = self.price_cache.lock() {
            *cache = PriceCache::default();
        }
    }

    /// Per-instrument coverage of stored prices.
    pub fn price_coverage(&self) -> Result<Vec<PriceCoverage>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT instrument_key, COUNT(DISTINCT session), MIN(session), MAX(session),
                    GROUP_CONCAT(DISTINCT source_id),
                    (SELECT json_extract(observation_json, '$.symbol') FROM price_bars p2
                     WHERE p2.instrument_key = p.instrument_key LIMIT 1)
             FROM price_bars p GROUP BY instrument_key ORDER BY instrument_key",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        let date = |s: Option<String>| {
            s.and_then(|s| chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
        };
        let mut out = Vec::new();
        for row in rows {
            let (key, sessions, first, last, sources, symbol) = row?;
            let mut sources: Vec<String> = sources
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            sources.sort();
            out.push(PriceCoverage {
                symbol: symbol.unwrap_or_else(|| key.clone()),
                instrument_key: key,
                sessions,
                first: date(first),
                last: date(last),
                sources,
            });
        }
        Ok(out)
    }

    /// A value that changes whenever prices are added, for caching evaluations.
    pub fn price_version(&self) -> Result<String, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let last: Option<i64> =
            conn.query_row("SELECT MAX(rowid) FROM price_bars", [], |r| r.get(0))?;
        Ok(last.unwrap_or(0).to_string())
    }
}
