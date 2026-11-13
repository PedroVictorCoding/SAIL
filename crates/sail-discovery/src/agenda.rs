//! The agenda: a small, durable list of things worth carrying between conversations.
//!
//! It holds topics worth discussing, investigations in progress, the owner's commitments,
//! potential actions and open questions. It is high-level semantic state, never a
//! transcript: each item is a title, a short detail, where it came from and a few match
//! terms. [`Provenance`] keeps the owner's commitments apart from SAIL's suggestions and
//! its own hypotheses, so a hypothesis is never presented as something the owner said.
//!
//! Open questions carry `match_terms`. When new evidence arrives (a Moodle change, an
//! escalation answer, a research finding) the caller passes its text to
//! [`AgendaStore::match_evidence`], which returns the open items it touches so they can be
//! revisited instead of forgotten.
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use sail_pulse::attention::Provenance;
use serde::{Deserialize, Serialize};

/// Most open items kept; the oldest low-value ones are dropped beyond this.
pub const MAX_OPEN_ITEMS: usize = 200;
pub const MAX_TITLE_CHARS: usize = 160;
pub const MAX_DETAIL_CHARS: usize = 1_000;
pub const MAX_TERMS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgendaKind {
    Topic,
    Investigation,
    Commitment,
    PotentialAction,
    OpenQuestion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgendaStatus {
    Open,
    InProgress,
    Resolved,
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewAgendaItem {
    /// Stable key so re-detecting the same thing updates instead of duplicating.
    pub key: String,
    pub kind: AgendaKind,
    pub title: String,
    pub detail: String,
    pub provenance: Provenance,
    /// Lowercase words or short phrases that new evidence must contain to match.
    pub match_terms: Vec<String>,
    pub due: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgendaItem {
    pub id: String,
    pub key: String,
    pub kind: AgendaKind,
    pub title: String,
    pub detail: String,
    pub provenance: Provenance,
    pub status: AgendaStatus,
    pub match_terms: Vec<String>,
    pub due: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Latest evidence that touched this item, short.
    pub last_evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgendaError {
    Invalid(String),
    NotFound(String),
    Storage(String),
}

impl std::fmt::Display for AgendaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgendaError::Invalid(m) => write!(f, "{m}"),
            AgendaError::NotFound(id) => write!(f, "agenda item {id} not found"),
            AgendaError::Storage(m) => write!(f, "agenda storage error: {m}"),
        }
    }
}

impl std::error::Error for AgendaError {}

impl From<rusqlite::Error> for AgendaError {
    fn from(e: rusqlite::Error) -> Self {
        AgendaError::Storage(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AgendaError>;

pub struct AgendaStore {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS agenda (
    id TEXT PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    detail TEXT NOT NULL,
    provenance TEXT NOT NULL,
    status TEXT NOT NULL,
    match_terms TEXT NOT NULL,
    due TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_evidence TEXT
);
CREATE INDEX IF NOT EXISTS agenda_status ON agenda(status, updated_at);
";

impl AgendaStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).map_err(|e| AgendaError::Storage(e.to_string()))?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Insert, or update the item with the same key. A resolved or dropped item is not
    /// reopened by re-detection (the owner already closed it).
    pub fn upsert(&self, new: NewAgendaItem, now: DateTime<Utc>) -> Result<AgendaItem> {
        let title = cut(new.title.trim(), MAX_TITLE_CHARS);
        let key = cut(new.key.trim(), 200);
        if title.is_empty() || key.is_empty() {
            return Err(AgendaError::Invalid(
                "an agenda item needs a key and a title".into(),
            ));
        }
        let detail = cut(new.detail.trim(), MAX_DETAIL_CHARS);
        let terms = normalize_terms(&new.match_terms);
        let terms_json = serde_json::to_string(&terms).unwrap_or_else(|_| "[]".into());
        let conn = self.lock();
        let existing: Option<String> = conn
            .query_row("SELECT id FROM agenda WHERE key=?1", [&key], |r| r.get(0))
            .optional()?;
        let id = match existing {
            Some(id) => {
                conn.execute(
                    "UPDATE agenda SET title=?2, detail=?3, match_terms=?4, due=?5, updated_at=?6
                     WHERE id=?1",
                    params![id, title, detail, terms_json, new.due.map(ts), ts(now)],
                )?;
                id
            }
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                conn.execute(
                    "INSERT INTO agenda (id, key, kind, title, detail, provenance, status,
                         match_terms, due, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'open', ?7, ?8, ?9, ?9)",
                    params![
                        id,
                        key,
                        enum_str(&new.kind),
                        title,
                        detail,
                        enum_str(&new.provenance),
                        terms_json,
                        new.due.map(ts),
                        ts(now)
                    ],
                )?;
                id
            }
        };
        let item = get_locked(&conn, &id)?;
        drop(conn);
        self.trim_open()?;
        Ok(item)
    }

    pub fn get(&self, id: &str) -> Result<AgendaItem> {
        get_locked(&self.lock(), id)
    }

    /// Open and in-progress items: commitments by due date first, then newest updated.
    pub fn open_items(&self, limit: usize) -> Result<Vec<AgendaItem>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT * FROM agenda WHERE status IN ('open','in_progress')
             ORDER BY (due IS NULL), due, updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], row_item)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(AgendaError::from)
    }

    pub fn set_status(
        &self,
        id: &str,
        status: AgendaStatus,
        now: DateTime<Utc>,
    ) -> Result<AgendaItem> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE agenda SET status=?2, updated_at=?3 WHERE id=?1",
            params![id, enum_str(&status), ts(now)],
        )?;
        if changed == 0 {
            return Err(AgendaError::NotFound(id.into()));
        }
        get_locked(&conn, id)
    }

    /// Open questions and investigations whose every match term (or, for items with
    /// several terms, at least two of them) appears in `evidence`. Matching items get
    /// `last_evidence` recorded. Case-insensitive; nothing is matched without terms.
    pub fn match_evidence(&self, evidence: &str, now: DateTime<Utc>) -> Result<Vec<AgendaItem>> {
        let text = evidence.to_lowercase();
        let candidates = {
            let conn = self.lock();
            let mut stmt = conn.prepare(
                "SELECT * FROM agenda WHERE status IN ('open','in_progress')
                 AND kind IN ('open_question','investigation','topic')",
            )?;
            let rows = stmt.query_map([], row_item)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut matched = Vec::new();
        for item in candidates {
            if item.match_terms.is_empty() {
                continue;
            }
            let hits = item
                .match_terms
                .iter()
                .filter(|t| text.contains(t.as_str()))
                .count();
            let needed = item.match_terms.len().min(2);
            if hits >= needed {
                let short = cut(evidence.trim(), 300);
                self.lock().execute(
                    "UPDATE agenda SET last_evidence=?2, updated_at=?3 WHERE id=?1",
                    params![item.id, short, ts(now)],
                )?;
                matched.push(self.get(&item.id)?);
            }
        }
        Ok(matched)
    }

    /// Keep at most [`MAX_OPEN_ITEMS`] open items: drop the stalest hypotheses and topics
    /// first; commitments are never dropped automatically.
    fn trim_open(&self) -> Result<()> {
        let conn = self.lock();
        let open: i64 = conn.query_row(
            "SELECT COUNT(*) FROM agenda WHERE status IN ('open','in_progress')",
            [],
            |r| r.get(0),
        )?;
        let excess = open - MAX_OPEN_ITEMS as i64;
        if excess > 0 {
            conn.execute(
                "UPDATE agenda SET status='dropped' WHERE id IN (
                     SELECT id FROM agenda WHERE status IN ('open','in_progress')
                     AND provenance != 'user_commitment'
                     ORDER BY (provenance='hypothesis') DESC, updated_at ASC LIMIT ?1)",
                [excess],
            )?;
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn get_locked(conn: &Connection, id: &str) -> Result<AgendaItem> {
    conn.query_row("SELECT * FROM agenda WHERE id=?1", [id], row_item)
        .optional()?
        .ok_or_else(|| AgendaError::NotFound(id.into()))
}

fn row_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgendaItem> {
    let parse_enum = |text: String| -> rusqlite::Result<serde_json::Value> {
        Ok(serde_json::Value::String(text))
    };
    let kind: String = row.get("kind")?;
    let provenance: String = row.get("provenance")?;
    let status: String = row.get("status")?;
    let terms: String = row.get("match_terms")?;
    let due: Option<String> = row.get("due")?;
    let created: String = row.get("created_at")?;
    let updated: String = row.get("updated_at")?;
    let bad = |e: String| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    };
    Ok(AgendaItem {
        id: row.get("id")?,
        key: row.get("key")?,
        kind: serde_json::from_value(parse_enum(kind)?).map_err(|e| bad(e.to_string()))?,
        title: row.get("title")?,
        detail: row.get("detail")?,
        provenance: serde_json::from_value(parse_enum(provenance)?)
            .map_err(|e| bad(e.to_string()))?,
        status: serde_json::from_value(parse_enum(status)?).map_err(|e| bad(e.to_string()))?,
        match_terms: serde_json::from_str(&terms).unwrap_or_default(),
        due: due.map(|d| parse_ts(&d)).transpose().map_err(bad)?,
        created_at: parse_ts(&created).map_err(bad)?,
        updated_at: parse_ts(&updated).map_err(bad)?,
        last_evidence: row.get("last_evidence")?,
    })
}

fn normalize_terms(terms: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for term in terms {
        let term = term.trim().to_lowercase();
        if term.chars().count() >= 3 && !out.contains(&term) {
            out.push(cut(&term, 60));
        }
        if out.len() == MAX_TERMS {
            break;
        }
    }
    out
}

fn enum_str<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn ts(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn parse_ts(text: &str) -> std::result::Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(text)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| e.to_string())
}

fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
