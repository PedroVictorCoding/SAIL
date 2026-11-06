//! SQLite adapter for course evidence. It uses its own append-only table in the Arena Learn
//! database file, separate from session events and from the knowledge graph.
use super::evidence::{CourseEvent, Stored};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
#[error("course store: {0}")]
pub struct CourseStoreError(pub String);

fn backend(e: impl std::fmt::Display) -> CourseStoreError {
    CourseStoreError(e.to_string())
}

pub struct CourseStore {
    conn: Mutex<Connection>,
}

impl CourseStore {
    pub fn open(path: &Path) -> Result<Self, CourseStoreError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(backend)?;
        }
        Self::init(Connection::open(path).map_err(backend)?)
    }

    pub fn open_in_memory() -> Result<Self, CourseStoreError> {
        Self::init(Connection::open_in_memory().map_err(backend)?)
    }

    fn init(conn: Connection) -> Result<Self, CourseStoreError> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS learn_course_events (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 at_ms INTEGER NOT NULL,
                 attempt_key TEXT UNIQUE,
                 event TEXT NOT NULL);",
        )
        .map_err(backend)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Appends one event. `attempt_key` makes an attempt idempotent: a second append with the
    /// same key stores nothing and returns `None`.
    pub fn append(
        &self,
        at_ms: i64,
        event: &CourseEvent,
        attempt_key: Option<&str>,
    ) -> Result<Option<u64>, CourseStoreError> {
        let text = serde_json::to_string(event).map_err(backend)?;
        let conn = self.lock();
        let changed = conn
            .execute(
                "INSERT OR IGNORE INTO learn_course_events (at_ms, attempt_key, event)
                 VALUES (?1, ?2, ?3)",
                params![at_ms, attempt_key, text],
            )
            .map_err(backend)?;
        Ok((changed == 1).then(|| conn.last_insert_rowid() as u64))
    }

    pub fn events(&self) -> Result<Vec<Stored>, CourseStoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT seq, at_ms, event FROM learn_course_events ORDER BY seq")
            .map_err(backend)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(backend)?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, at_ms, text) = row.map_err(backend)?;
            match serde_json::from_str(&text) {
                Ok(event) => out.push(Stored {
                    seq: seq as u64,
                    at_ms,
                    event,
                }),
                // An event this build can't read is skipped, not fatal. It stays in the log.
                Err(error) => tracing::warn!(seq, %error, "unreadable course event skipped"),
            }
        }
        Ok(out)
    }

    pub fn by_attempt_key(&self, key: &str) -> Result<Option<Stored>, CourseStoreError> {
        let conn = self.lock();
        let row: Option<(i64, i64, String)> = conn
            .query_row(
                "SELECT seq, at_ms, event FROM learn_course_events WHERE attempt_key = ?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(backend)?;
        match row {
            None => Ok(None),
            Some((seq, at_ms, text)) => Ok(Some(Stored {
                seq: seq as u64,
                at_ms,
                event: serde_json::from_str(&text).map_err(backend)?,
            })),
        }
    }

    pub fn last_seq(&self) -> Result<u64, CourseStoreError> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM learn_course_events",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|v| v as u64)
        .map_err(backend)
    }
}
