//! SQLite event store (#234): WAL, append-only events, idempotency keys, version guard,
//! plus persistent Knowledge Graph tables.
use crate::core::events::{Envelope, LearningEvent, NewEvent, SCHEMA_VERSION};
use crate::core::graph::{
    GraphNodePosition, GraphProposition, GraphReasoningChain, GraphRenderSettings, GraphSnapshot,
    GraphSourceInfo,
};
use crate::core::ports::{EventStore, SessionSummary, StoreError};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeMap;
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
                 PRIMARY KEY (session, seq), UNIQUE (session, key));
             CREATE TABLE IF NOT EXISTS learn_graph_props (
                 name TEXT PRIMARY KEY,
                 data TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS learn_graph_chains (
                 id TEXT PRIMARY KEY,
                 data TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS learn_graph_positions (
                 name TEXT PRIMARY KEY,
                 x REAL NOT NULL,
                 y REAL NOT NULL
             );
             CREATE TABLE IF NOT EXISTS learn_graph_settings (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 data TEXT NOT NULL
             );",
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

    fn graph_snapshot(&self) -> Result<GraphSnapshot, StoreError> {
        let conn = self.lock();
        let mut propositions = Vec::new();
        {
            let mut stmt = conn
                .prepare("SELECT data FROM learn_graph_props ORDER BY name")
                .map_err(backend)?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(backend)?;
            for row in rows {
                let raw = row.map_err(backend)?;
                if let Ok(prop) = serde_json::from_str::<GraphProposition>(&raw) {
                    propositions.push(prop);
                }
            }
        }
        let mut chains = Vec::new();
        {
            let mut stmt = conn
                .prepare("SELECT data FROM learn_graph_chains ORDER BY id")
                .map_err(backend)?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(backend)?;
            for row in rows {
                let raw = row.map_err(backend)?;
                if let Ok(chain) = serde_json::from_str::<GraphReasoningChain>(&raw) {
                    chains.push(chain);
                }
            }
        }
        let mut positions = BTreeMap::new();
        {
            let mut stmt = conn
                .prepare("SELECT name, x, y FROM learn_graph_positions")
                .map_err(backend)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, f64>(1)?,
                        r.get::<_, f64>(2)?,
                    ))
                })
                .map_err(backend)?;
            for row in rows {
                let (name, x, y) = row.map_err(backend)?;
                positions.insert(name, GraphNodePosition { x, y });
            }
        }
        let render_settings = conn
            .query_row(
                "SELECT data FROM learn_graph_settings WHERE id = 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(backend)?
            .and_then(|s| serde_json::from_str::<GraphRenderSettings>(&s).ok())
            .unwrap_or_default();

        let mut sources_map: BTreeMap<String, GraphSourceInfo> = BTreeMap::new();
        for prop in &propositions {
            for prov in &prop.provenance {
                let (id, label) = if let Some(sid) = &prov.session_id {
                    (
                        sid.clone(),
                        prov.label.clone().unwrap_or_else(|| sid.clone()),
                    )
                } else if let Some(fname) = &prov.file_name {
                    (format!("notes:{fname}"), fname.clone())
                } else {
                    continue;
                };
                sources_map.entry(id.clone()).or_insert(GraphSourceInfo {
                    id,
                    kind: prov.kind.clone(),
                    label,
                    created_at: prov.created_at,
                });
            }
        }
        for chain in &chains {
            sources_map
                .entry(chain.source_id.clone())
                .or_insert(GraphSourceInfo {
                    id: chain.source_id.clone(),
                    kind: chain.source_kind.clone(),
                    label: chain.source_label.clone(),
                    created_at: chain.created_at,
                });
        }
        let sources = sources_map.into_values().collect();
        Ok(GraphSnapshot {
            propositions,
            chains,
            sources,
            positions,
            render_settings,
        })
    }

    fn upsert_graph_items(
        &self,
        props: &[GraphProposition],
        chains: &[GraphReasoningChain],
    ) -> Result<(), StoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(backend)?;
        for prop in props {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT data FROM learn_graph_props WHERE name=?1",
                    [&prop.name],
                    |r| r.get(0),
                )
                .optional()
                .map_err(backend)?;
            let merged = if let Some(raw) = existing
                && let Ok(mut old) = serde_json::from_str::<GraphProposition>(&raw)
            {
                for alias in &prop.aliases {
                    if !old.aliases.contains(alias) {
                        old.aliases.push(alias.clone());
                    }
                }
                for p in &prop.provenance {
                    if !old
                        .provenance
                        .iter()
                        .any(|op| op.session_id == p.session_id && op.file_name == p.file_name)
                    {
                        old.provenance.push(p.clone());
                    }
                }
                if !prop.statement.trim().is_empty() {
                    old.statement = prop.statement.clone();
                }
                old
            } else {
                prop.clone()
            };
            let json = serde_json::to_string(&merged).map_err(backend)?;
            tx.execute(
                "INSERT INTO learn_graph_props (name, data) VALUES (?1, ?2)
                 ON CONFLICT(name) DO UPDATE SET data=excluded.data",
                params![merged.name, json],
            )
            .map_err(backend)?;
        }
        for chain in chains {
            let json = serde_json::to_string(chain).map_err(backend)?;
            tx.execute(
                "INSERT INTO learn_graph_chains (id, data) VALUES (?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET data=excluded.data",
                params![chain.id, json],
            )
            .map_err(backend)?;
        }
        tx.commit().map_err(backend)?;
        Ok(())
    }

    fn delete_graph_nodes(&self, names_or_ids: &[String]) -> Result<usize, StoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(backend)?;
        let mut removed = 0usize;
        let mut target_names = std::collections::BTreeSet::new();
        {
            let mut stmt = tx
                .prepare("SELECT name, data FROM learn_graph_props")
                .map_err(backend)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(backend)?;
            for row in rows {
                let (name, raw) = row.map_err(backend)?;
                if names_or_ids.contains(&name) {
                    target_names.insert(name);
                } else if let Ok(prop) = serde_json::from_str::<GraphProposition>(&raw)
                    && names_or_ids.contains(&prop.id)
                {
                    target_names.insert(name);
                }
            }
        }
        for name in &target_names {
            removed += tx
                .execute("DELETE FROM learn_graph_props WHERE name=?1", [name])
                .map_err(backend)?;
            let _ = tx.execute("DELETE FROM learn_graph_positions WHERE name=?1", [name]);
        }
        let mut drop_chains = Vec::new();
        {
            let mut stmt = tx
                .prepare("SELECT id, data FROM learn_graph_chains")
                .map_err(backend)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(backend)?;
            for row in rows {
                let (id, raw) = row.map_err(backend)?;
                if let Ok(chain) = serde_json::from_str::<GraphReasoningChain>(&raw)
                    && (target_names.contains(&chain.conclusion)
                        || chain.premises.iter().any(|p| target_names.contains(p)))
                {
                    drop_chains.push(id);
                }
            }
        }
        for id in drop_chains {
            let _ = tx.execute("DELETE FROM learn_graph_chains WHERE id=?1", [id]);
        }
        tx.commit().map_err(backend)?;
        Ok(removed)
    }

    fn delete_graph_source(&self, source_id: &str) -> Result<usize, StoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(backend)?;
        let mut removed = 0usize;
        let mut drop_props = Vec::new();
        let mut update_props = Vec::new();
        {
            let mut stmt = tx
                .prepare("SELECT name, data FROM learn_graph_props")
                .map_err(backend)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(backend)?;
            for row in rows {
                let (name, raw) = row.map_err(backend)?;
                if let Ok(mut prop) = serde_json::from_str::<GraphProposition>(&raw) {
                    let before = prop.provenance.len();
                    prop.provenance.retain(|p| {
                        let pid = p
                            .session_id
                            .clone()
                            .or_else(|| p.file_name.as_ref().map(|f| format!("notes:{f}")))
                            .unwrap_or_default();
                        pid != source_id && p.file_name.as_deref() != Some(source_id)
                    });
                    if prop.provenance.is_empty() && before > 0 {
                        drop_props.push(name);
                    } else if prop.provenance.len() != before {
                        update_props.push((name, prop));
                    }
                }
            }
        }
        for name in &drop_props {
            removed += tx
                .execute("DELETE FROM learn_graph_props WHERE name=?1", [name])
                .map_err(backend)?;
            let _ = tx.execute("DELETE FROM learn_graph_positions WHERE name=?1", [name]);
        }
        for (name, prop) in update_props {
            let json = serde_json::to_string(&prop).map_err(backend)?;
            tx.execute(
                "UPDATE learn_graph_props SET data=?2 WHERE name=?1",
                params![name, json],
            )
            .map_err(backend)?;
        }
        let mut drop_chains = Vec::new();
        {
            let mut stmt = tx
                .prepare("SELECT id, data FROM learn_graph_chains")
                .map_err(backend)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(backend)?;
            for row in rows {
                let (id, raw) = row.map_err(backend)?;
                if let Ok(chain) = serde_json::from_str::<GraphReasoningChain>(&raw)
                    && (chain.source_id == source_id
                        || chain.source_label == source_id
                        || drop_props.contains(&chain.conclusion)
                        || chain.premises.iter().any(|p| drop_props.contains(p)))
                {
                    drop_chains.push(id);
                }
            }
        }
        for id in drop_chains {
            let _ = tx.execute("DELETE FROM learn_graph_chains WHERE id=?1", [id]);
        }
        tx.commit().map_err(backend)?;
        Ok(removed)
    }

    fn save_graph_positions(
        &self,
        positions: &BTreeMap<String, GraphNodePosition>,
    ) -> Result<(), StoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(backend)?;
        for (name, pos) in positions {
            tx.execute(
                "INSERT INTO learn_graph_positions (name, x, y) VALUES (?1, ?2, ?3)
                 ON CONFLICT(name) DO UPDATE SET x=excluded.x, y=excluded.y",
                params![name, pos.x, pos.y],
            )
            .map_err(backend)?;
        }
        tx.commit().map_err(backend)?;
        Ok(())
    }

    fn save_graph_settings(&self, settings: &GraphRenderSettings) -> Result<(), StoreError> {
        let conn = self.lock();
        let json = serde_json::to_string(settings).map_err(backend)?;
        conn.execute(
            "INSERT INTO learn_graph_settings (id, data) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            [json],
        )
        .map_err(backend)?;
        Ok(())
    }
}
