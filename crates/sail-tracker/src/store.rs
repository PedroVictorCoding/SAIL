//! Local SQLite store: the config document, metric history for baselines, and every
//! headline seen (so "new since" and per-topic volume survive restarts).
use crate::config::TrackerConfig;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Samples and headlines older than this are pruned.
pub const RETENTION_DAYS: i64 = 30;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("tracker storage: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("tracker storage: {0}")]
    Io(#[from] std::io::Error),
    #[error("tracker config is unreadable: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredHeadline {
    pub id: String,
    pub title: String,
    pub url: String,
    pub source: String,
    pub topic: Option<String>,
    pub published: i64,
    pub first_seen: i64,
}

pub struct Store {
    conn: Mutex<Connection>,
    dir: PathBuf,
}

pub fn default_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SAIL_TRACKER_DIR") {
        return dir.into();
    }
    sail_common::storage::data_path("tracker")
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir)?;
        let conn = Connection::open(dir.join("tracker.db"))?;
        Self::init(conn, dir.to_path_buf())
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?, std::env::temp_dir())
    }

    fn init(conn: Connection, dir: PathBuf) -> Result<Self, StoreError> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS tracker_config (id INTEGER PRIMARY KEY CHECK (id = 1), json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS samples (
                 metric TEXT NOT NULL, ts INTEGER NOT NULL, value REAL NOT NULL,
                 PRIMARY KEY (metric, ts)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS headlines (
                 id TEXT PRIMARY KEY, title TEXT NOT NULL, url TEXT NOT NULL, source TEXT NOT NULL,
                 topic TEXT, published INTEGER NOT NULL, first_seen INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS headlines_seen ON headlines(first_seen);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            dir,
        })
    }

    /// Directory for cached files such as map tiles.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn load_config(&self) -> Result<Option<TrackerConfig>, StoreError> {
        let json: Option<String> = self
            .conn()
            .query_row("SELECT json FROM tracker_config WHERE id = 1", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(match json {
            Some(json) => Some(serde_json::from_str(&json)?),
            None => None,
        })
    }

    pub fn save_config(&self, config: &TrackerConfig) -> Result<(), StoreError> {
        let json = serde_json::to_string(config)?;
        self.conn().execute(
            "INSERT INTO tracker_config (id, json) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET json = excluded.json",
            params![json],
        )?;
        Ok(())
    }

    pub fn record(&self, metric: &str, ts: i64, value: f64) -> Result<(), StoreError> {
        if !value.is_finite() {
            return Ok(());
        }
        self.conn().execute(
            "INSERT OR REPLACE INTO samples (metric, ts, value) VALUES (?1, ?2, ?3)",
            params![metric, ts, value],
        )?;
        Ok(())
    }

    /// Samples for `metric` at or after `since`, oldest first.
    pub fn series(&self, metric: &str, since: i64) -> Result<Vec<(i64, f64)>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT ts, value FROM samples WHERE metric = ?1 AND ts >= ?2 ORDER BY ts",
        )?;
        let rows = stmt.query_map(params![metric, since], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Inserts headlines not seen before. Returns how many were new.
    pub fn add_headlines(&self, items: &[StoredHeadline]) -> Result<usize, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut added = 0;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO headlines (id, title, url, source, topic, published, first_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for h in items {
                added += stmt.execute(params![
                    h.id,
                    h.title,
                    h.url,
                    h.source,
                    h.topic,
                    h.published,
                    h.first_seen
                ])?;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// Headlines first seen at or after `since`, newest first.
    pub fn headlines(&self, since: i64, limit: usize) -> Result<Vec<StoredHeadline>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT id, title, url, source, topic, published, first_seen FROM headlines
             WHERE first_seen >= ?1 ORDER BY published DESC, first_seen DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![since, limit as i64], |r| {
            Ok(StoredHeadline {
                id: r.get(0)?,
                title: r.get(1)?,
                url: r.get(2)?,
                source: r.get(3)?,
                topic: r.get(4)?,
                published: r.get(5)?,
                first_seen: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn prune(&self, now: i64) -> Result<(), StoreError> {
        let cutoff = now - RETENTION_DAYS * 86_400;
        let conn = self.conn();
        conn.execute("DELETE FROM samples WHERE ts < ?1", params![cutoff])?;
        conn.execute(
            "DELETE FROM headlines WHERE first_seen < ?1",
            params![cutoff],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip_and_series() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.load_config().unwrap().is_none());
        let config = TrackerConfig::default();
        store.save_config(&config).unwrap();
        assert_eq!(store.load_config().unwrap(), Some(config));

        store.record("air:dublin", 100, 4.0).unwrap();
        store.record("air:dublin", 200, 6.0).unwrap();
        store.record("air:dublin", 300, f64::NAN).unwrap();
        assert_eq!(store.series("air:dublin", 150).unwrap(), vec![(200, 6.0)]);
    }

    #[test]
    fn headlines_are_inserted_once() {
        let store = Store::open_in_memory().unwrap();
        let h = StoredHeadline {
            id: "a".into(),
            title: "Title".into(),
            url: "https://example.com".into(),
            source: "Example".into(),
            topic: None,
            published: 10,
            first_seen: 20,
        };
        assert_eq!(store.add_headlines(&[h.clone(), h.clone()]).unwrap(), 1);
        assert_eq!(store.headlines(0, 10).unwrap(), vec![h]);
        store.prune(20 + RETENTION_DAYS * 86_400 + 1).unwrap();
        assert!(store.headlines(0, 10).unwrap().is_empty());
    }
}
