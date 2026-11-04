//! SQLite event store (#234): WAL, append-only events, idempotency keys, version guard.
use crate::core::events::{Envelope, LearningEvent, NewEvent, SCHEMA_VERSION};
use crate::core::ports::{EventStore, SessionSummary, StoreError};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::Mutex;

/// Version of the table layout (separate from the event schema version).
const DB_VERSION: i64 = 1;

pub struct SqliteEventStore {
    conn: Mutex<Connection>,
}

fn backend(e: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(e.to_string())
}

impl SqliteEventStore {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(backend)?;
        }
        Self::init(Connection::open(path).map_err(backend)?)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory().map_err(backend)?)
    }

    /// `SAIL_LEARN_DB`, else `arena/learn.db` under the SAIL data root, beside Arena's store.
    pub fn default_path() -> std::path::PathBuf {
        if let Ok(path) = std::env::var("SAIL_LEARN_DB") {
            return path.into();
        }
        sail_common::storage::data_path("arena").join("learn.db")
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS learn_meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS learn_sessions (
                 id TEXT PRIMARY KEY, goal TEXT NOT NULL,
                 created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL,
                 last_seq INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS learn_events (
                 session TEXT NOT NULL REFERENCES learn_sessions(id) ON DELETE CASCADE,
                 seq INTEGER NOT NULL, at_ms INTEGER NOT NULL, key TEXT NOT NULL,
                 version INTEGER NOT NULL, event TEXT NOT NULL,
                 PRIMARY KEY (session, seq), UNIQUE (session, key));",
        )
        .map_err(backend)?;
        let stored: Option<i64> = conn
            .query_row(
                "SELECT value FROM learn_meta WHERE key='db_version'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(backend)?;
        match stored {
            None => {
                conn.execute(
                    "INSERT INTO learn_meta (key, value) VALUES ('db_version', ?1)",
                    [DB_VERSION],
                )
                .map_err(backend)?;
            }
            Some(v) if v > DB_VERSION => {
                return Err(StoreError::NewerVersion {
                    found: v as u16,
                    supported: DB_VERSION as u16,
                });
            }
            Some(_) => {}
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn decode(
    session: String,
    seq: i64,
    at_ms: i64,
    key: String,
    version: i64,
    json: String,
) -> Result<Envelope, StoreError> {
    if version > i64::from(SCHEMA_VERSION) {
        return Err(StoreError::NewerVersion {
            found: version as u16,
            supported: SCHEMA_VERSION,
        });
    }
    let event: LearningEvent = serde_json::from_str(&json).map_err(backend)?;
    Ok(Envelope {
        session,
        seq: seq as u64,
        at_ms,
        key,
        version: version as u16,
        event,
    })
}

fn insert(
    tx: &rusqlite::Transaction<'_>,
    session: &str,
    seq: u64,
    at_ms: i64,
    event: &NewEvent,
) -> Result<Envelope, StoreError> {
    let json = serde_json::to_string(&event.event).map_err(backend)?;
    tx.execute(
        "INSERT INTO learn_events (session, seq, at_ms, key, version, event)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![session, seq as i64, at_ms, event.key, SCHEMA_VERSION, json],
    )
    .map_err(backend)?;
    Ok(Envelope {
        session: session.to_string(),
        seq,
        at_ms,
        key: event.key.clone(),
        version: SCHEMA_VERSION,
        event: event.event.clone(),
    })
}

impl EventStore for SqliteEventStore {
    fn create(&self, session: &str, goal: &str, at_ms: i64) -> Result<Envelope, StoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(backend)?;
        tx.execute(
            "INSERT INTO learn_sessions (id, goal, created_ms, updated_ms, last_seq)
             VALUES (?1, ?2, ?3, ?3, 1)",
            params![session, goal, at_ms],
        )
        .map_err(backend)?;
        let envelope = insert(
            &tx,
            session,
            1,
            at_ms,
            &NewEvent::new(
                "created",
                LearningEvent::SessionCreated {
                    goal: goal.to_string(),
                },
            ),
        )?;
        tx.commit().map_err(backend)?;
        Ok(envelope)
    }

    fn append(
        &self,
        session: &str,
        expected_seq: u64,
        at_ms: i64,
        events: &[NewEvent],
    ) -> Result<Vec<Envelope>, StoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(backend)?;
        let last: Option<i64> = tx
            .query_row(
                "SELECT last_seq FROM learn_sessions WHERE id=?1",
                [session],
                |r| r.get(0),
            )
            .optional()
            .map_err(backend)?;
        let Some(last) = last else {
            return Err(StoreError::NotFound(session.to_string()));
        };
        let mut seq = last as u64;
        if seq != expected_seq {
            return Err(StoreError::Conflict {
                session: session.to_string(),
                expected: expected_seq,
                found: seq,
            });
        }
        let mut stored = Vec::new();
        for event in events {
            let exists: bool = tx
                .query_row(
                    "SELECT 1 FROM learn_events WHERE session=?1 AND key=?2",
                    params![session, event.key],
                    |_| Ok(true),
                )
                .optional()
                .map_err(backend)?
                .unwrap_or(false);
            if exists {
                continue;
            }
            seq += 1;
            stored.push(insert(&tx, session, seq, at_ms, event)?);
        }
        tx.execute(
            "UPDATE learn_sessions SET last_seq=?2, updated_ms=?3 WHERE id=?1",
            params![session, seq as i64, at_ms],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)?;
        Ok(stored)
    }

    fn events(&self, session: &str) -> Result<Vec<Envelope>, StoreError> {
        let conn = self.lock();
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM learn_sessions WHERE id=?1",
                [session],
                |_| Ok(true),
            )
            .optional()
            .map_err(backend)?
            .unwrap_or(false);
        if !exists {
            return Err(StoreError::NotFound(session.to_string()));
        }
        let mut stmt = conn
            .prepare(
                "SELECT session, seq, at_ms, key, version, event FROM learn_events
                 WHERE session=?1 ORDER BY seq",
            )
            .map_err(backend)?;
        let rows = stmt
            .query_map([session], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .map_err(backend)?;
        let mut out = Vec::new();
        for row in rows {
            let (s, seq, at, key, version, json) = row.map_err(backend)?;
            out.push(decode(s, seq, at, key, version, json)?);
        }
        Ok(out)
    }

    fn sessions(&self, limit: usize) -> Result<Vec<SessionSummary>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, goal, created_ms, updated_ms FROM learn_sessions
                 ORDER BY updated_ms DESC, id LIMIT ?1",
            )
            .map_err(backend)?;
        let rows = stmt
            .query_map([limit as i64], |r| {
                Ok(SessionSummary {
                    id: r.get(0)?,
                    goal: r.get(1)?,
                    created_ms: r.get(2)?,
                    updated_ms: r.get(3)?,
                })
            })
            .map_err(backend)?;
        rows.collect::<Result<_, _>>().map_err(backend)
    }

    fn delete(&self, session: &str) -> Result<bool, StoreError> {
        let conn = self.lock();
        let n = conn
            .execute("DELETE FROM learn_sessions WHERE id=?1", [session])
            .map_err(backend)?;
        Ok(n > 0)
    }
}
