//! K3: SQLite event/source store plus the secret feeds file and change broadcast.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::broadcast;

use crate::contracts::{
    CalendarChanged, CalendarError, Event, FeedChange, Preferences, Source, SourceKind,
};

/// Fixed local source id (contract).
pub(crate) const LOCAL_SOURCE_ID: &str = "src_local000000";
/// Fixed local source name (contract).
pub(crate) const LOCAL_SOURCE_NAME: &str = "SAIL calendar";
/// The change broadcast keeps the latest few revisions (fixed a priori).
const EVENT_CAPACITY: usize = 32;
/// Feed changes are kept 7 days (owner decision).
const CHANGE_RETENTION: Duration = Duration::days(7);

pub(crate) struct Store {
    dir: PathBuf,
    conn: Mutex<Connection>,
    feeds: Mutex<BTreeMap<String, String>>,
    events: broadcast::Sender<CalendarChanged>,
}

impl Store {
    pub(crate) fn open(dir: &Path) -> Result<Self, CalendarError> {
        std::fs::create_dir_all(dir).map_err(storage)?;
        let conn = Connection::open(dir.join("calendar.sqlite")).map_err(storage)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS sources (id TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS events (id TEXT PRIMARY KEY, source_id TEXT NOT NULL, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS annotations (event_id TEXT PRIMARY KEY, prepare INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS changes (id INTEGER PRIMARY KEY AUTOINCREMENT, data TEXT NOT NULL, observed_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS feed_http (id TEXT PRIMARY KEY, etag TEXT, last_modified TEXT);",
        )
        .map_err(storage)?;
        let feeds = load_feeds(dir)?;
        let store = Self {
            dir: dir.to_path_buf(),
            conn: Mutex::new(conn),
            feeds: Mutex::new(feeds),
            events: broadcast::channel(EVENT_CAPACITY).0,
        };
        if store.source(LOCAL_SOURCE_ID)?.is_none() {
            let local = Source {
                id: LOCAL_SOURCE_ID.into(),
                kind: SourceKind::Local,
                name: LOCAL_SOURCE_NAME.into(),
                color: "#4f7cff".into(),
                visible: true,
                writable: true,
                url_hint: None,
                refresh_minutes: 30,
                event_count: 0,
                last_sync_at: None,
                last_error: None,
                freshness: crate::contracts::Freshness::Never,
                truncated: false,
            };
            store.save_source(&local, None)?;
        }
        Ok(store)
    }

    pub(crate) fn revision(&self) -> Result<u64, CalendarError> {
        let conn = self.lock()?;
        Ok(read_revision(&conn))
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<CalendarChanged> {
        self.events.subscribe()
    }

    pub(crate) fn sources(&self) -> Result<Vec<Source>, CalendarError> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT data FROM sources ORDER BY rowid")
            .map_err(storage)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage)?;
        let mut sources = Vec::new();
        for row in rows {
            let data: String = row.map_err(storage)?;
            let mut source: Source = serde_json::from_str(&data).map_err(storage)?;
            source.event_count = count_events(&conn, &source.id)?;
            source.freshness = freshness(&source, Utc::now());
            sources.push(source);
        }
        Ok(sources)
    }

    pub(crate) fn source(&self, id: &str) -> Result<Option<Source>, CalendarError> {
        let conn = self.lock()?;
        let data: Option<String> = conn
            .query_row("SELECT data FROM sources WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(storage)?;
        let Some(data) = data else {
            return Ok(None);
        };
        let mut source: Source = serde_json::from_str(&data).map_err(storage)?;
        source.event_count = count_events(&conn, id)?;
        source.freshness = freshness(&source, Utc::now());
        Ok(Some(source))
    }

    /// Save a source's sync bookkeeping (last sync time, HTTP validators) without a
    /// revision bump, so a refresh that changed nothing does not make clients refetch.
    pub(crate) fn record_sync(&self, source: &Source) -> Result<(), CalendarError> {
        let conn = self.lock()?;
        let data = serde_json::to_string(source).map_err(storage)?;
        conn.execute(
            "INSERT INTO sources (id, data) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            params![source.id, data],
        )
        .map_err(storage)?;
        Ok(())
    }

    pub(crate) fn save_source(
        &self,
        source: &Source,
        url: Option<&str>,
    ) -> Result<(), CalendarError> {
        {
            let conn = self.lock()?;
            let data = serde_json::to_string(source).map_err(storage)?;
            conn.execute(
                "INSERT INTO sources (id, data) VALUES (?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET data=excluded.data",
                params![source.id, data],
            )
            .map_err(storage)?;
        }
        if let Some(url) = url {
            self.feeds
                .lock()
                .map_err(|_| storage("the feeds lock is poisoned"))?
                .insert(source.id.clone(), url.to_string());
            save_feeds(&self.dir, &self.feeds())?;
        }
        self.bump(Some(&source.id))
    }

    pub(crate) fn delete_source(&self, id: &str) -> Result<(), CalendarError> {
        {
            let conn = self.lock()?;
            conn.execute("DELETE FROM events WHERE source_id=?1", [id])
                .map_err(storage)?;
            conn.execute("DELETE FROM sources WHERE id=?1", [id])
                .map_err(storage)?;
            conn.execute("DELETE FROM feed_http WHERE id=?1", [id])
                .map_err(storage)?;
        }
        self.feeds
            .lock()
            .map_err(|_| storage("the feeds lock is poisoned"))?
            .remove(id);
        save_feeds(&self.dir, &self.feeds())?;
        self.bump(Some(id))
    }

    pub(crate) fn feed_url(&self, id: &str) -> Option<String> {
        self.feeds.lock().ok()?.get(id).cloned()
    }

    pub(crate) fn feed_http(
        &self,
        id: &str,
    ) -> Result<(Option<String>, Option<String>), CalendarError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT etag, last_modified FROM feed_http WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(storage)
        .map(|value| value.unwrap_or((None, None)))
    }

    pub(crate) fn set_feed_http(
        &self,
        id: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<(), CalendarError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO feed_http (id, etag, last_modified) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET etag=excluded.etag, last_modified=excluded.last_modified",
            params![id, etag, last_modified],
        )
        .map_err(storage)?;
        Ok(())
    }

    pub(crate) fn events(
        &self,
        source_ids: Option<&[String]>,
    ) -> Result<Vec<Event>, CalendarError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT data FROM events").map_err(storage)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage)?;
        let mut events = Vec::new();
        for row in rows {
            let data: String = row.map_err(storage)?;
            let mut event: Event = serde_json::from_str(&data).map_err(storage)?;
            if source_ids.is_some_and(|ids| !ids.iter().any(|id| id == &event.source_id)) {
                continue;
            }
            event.prepare = prepare_for(&conn, &event.id)?;
            events.push(event);
        }
        Ok(events)
    }

    pub(crate) fn event(&self, id: &str) -> Result<Option<Event>, CalendarError> {
        let conn = self.lock()?;
        let data: Option<String> = conn
            .query_row("SELECT data FROM events WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(storage)?;
        let Some(data) = data else {
            return Ok(None);
        };
        let mut event: Event = serde_json::from_str(&data).map_err(storage)?;
        event.prepare = prepare_for(&conn, id)?;
        Ok(Some(event))
    }

    pub(crate) fn put_event(&self, event: &Event) -> Result<(), CalendarError> {
        {
            let conn = self.lock()?;
            let data = serde_json::to_string(event).map_err(storage)?;
            conn.execute(
                "INSERT INTO events (id, source_id, data) VALUES (?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET source_id=excluded.source_id, data=excluded.data",
                params![event.id, event.source_id, data],
            )
            .map_err(storage)?;
            conn.execute(
                "INSERT INTO annotations (event_id, prepare) VALUES (?1, ?2)
                 ON CONFLICT(event_id) DO UPDATE SET prepare=excluded.prepare",
                params![event.id, i64::from(event.prepare)],
            )
            .map_err(storage)?;
        }
        self.bump(Some(&event.source_id))
    }

    pub(crate) fn set_prepare(&self, id: &str, prepare: bool) -> Result<(), CalendarError> {
        let source_id = {
            let conn = self.lock()?;
            let source: Option<String> = conn
                .query_row("SELECT source_id FROM events WHERE id=?1", [id], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(storage)?;
            let Some(source) = source else {
                return Err(CalendarError::NotFound);
            };
            conn.execute(
                "INSERT INTO annotations (event_id, prepare) VALUES (?1, ?2)
                 ON CONFLICT(event_id) DO UPDATE SET prepare=excluded.prepare",
                params![id, i64::from(prepare)],
            )
            .map_err(storage)?;
            source
        };
        self.bump(Some(&source_id))
    }

    pub(crate) fn delete_event(&self, id: &str) -> Result<(), CalendarError> {
        let source_id = {
            let conn = self.lock()?;
            let source: Option<String> = conn
                .query_row("SELECT source_id FROM events WHERE id=?1", [id], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(storage)?;
            let Some(source) = source else {
                return Err(CalendarError::NotFound);
            };
            conn.execute("DELETE FROM events WHERE id=?1", [id])
                .map_err(storage)?;
            conn.execute("DELETE FROM annotations WHERE event_id=?1", [id])
                .map_err(storage)?;
            source
        };
        self.bump(Some(&source_id))
    }

    pub(crate) fn apply_feed(
        &self,
        source_id: &str,
        upserts: &[Event],
        removed: &[String],
        changes: &[FeedChange],
    ) -> Result<(), CalendarError> {
        if upserts.is_empty() && removed.is_empty() && changes.is_empty() {
            // Nothing the clients can see changed: no write, no revision bump.
            return Ok(());
        }
        {
            let mut guard = self.lock()?;
            // One transaction per sync: autocommit would fsync once per event.
            let conn = guard.transaction().map_err(storage)?;
            for event in upserts {
                let data = serde_json::to_string(event).map_err(storage)?;
                conn.execute(
                    "INSERT INTO events (id, source_id, data) VALUES (?1, ?2, ?3)
                     ON CONFLICT(id) DO UPDATE SET source_id=excluded.source_id, data=excluded.data",
                    params![event.id, event.source_id, data],
                )
                .map_err(storage)?;
            }
            for id in removed {
                conn.execute("DELETE FROM events WHERE id=?1", [id])
                    .map_err(storage)?;
            }
            for change in changes {
                let data = serde_json::to_string(change).map_err(storage)?;
                conn.execute(
                    "INSERT INTO changes (data, observed_at) VALUES (?1, ?2)",
                    params![data, change.observed_at.to_rfc3339()],
                )
                .map_err(storage)?;
            }
            let cutoff = (Utc::now() - CHANGE_RETENTION).to_rfc3339();
            conn.execute("DELETE FROM changes WHERE observed_at < ?1", [cutoff])
                .map_err(storage)?;
            conn.commit().map_err(storage)?;
        }
        self.bump(Some(source_id))
    }

    pub(crate) fn recent_changes(&self, since: DateTime<Utc>) -> Vec<FeedChange> {
        let Ok(conn) = self.lock() else {
            return Vec::new();
        };
        let mut stmt = match conn.prepare("SELECT data FROM changes WHERE observed_at >= ?1") {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        let rows = match stmt.query_map([since.to_rfc3339()], |row| row.get::<_, String>(0)) {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };
        rows.flatten()
            .filter_map(|data| serde_json::from_str(&data).ok())
            .collect()
    }

    pub(crate) fn preferences(&self) -> Result<Preferences, CalendarError> {
        let conn = self.lock()?;
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key='preferences'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage)?;
        match value {
            Some(value) => serde_json::from_str(&value).map_err(storage),
            None => Ok(default_preferences()),
        }
    }

    pub(crate) fn set_preferences(&self, prefs: &Preferences) -> Result<(), CalendarError> {
        {
            let conn = self.lock()?;
            let value = serde_json::to_string(prefs).map_err(storage)?;
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('preferences', ?1)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [value],
            )
            .map_err(storage)?;
        }
        self.bump(None)
    }

    fn feeds(&self) -> BTreeMap<String, String> {
        self.feeds
            .lock()
            .map(|feeds| feeds.clone())
            .unwrap_or_default()
    }

    fn bump(&self, source_id: Option<&str>) -> Result<(), CalendarError> {
        let revision = {
            let conn = self.lock()?;
            let next = read_revision(&conn) + 1;
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('revision', ?1)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [next.to_string()],
            )
            .map_err(storage)?;
            next
        };
        let _ = self.events.send(CalendarChanged {
            revision,
            source_id: source_id.map(str::to_string),
        });
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, CalendarError> {
        self.conn
            .lock()
            .map_err(|_| storage("the calendar store lock is poisoned"))
    }
}

fn read_revision(conn: &Connection) -> u64 {
    conn.query_row("SELECT value FROM meta WHERE key='revision'", [], |row| {
        row.get::<_, String>(0)
    })
    .optional()
    .ok()
    .flatten()
    .and_then(|value| value.parse().ok())
    .unwrap_or(0)
}

fn count_events(conn: &Connection, source_id: &str) -> Result<u32, CalendarError> {
    conn.query_row(
        "SELECT COUNT(*) FROM events WHERE source_id=?1",
        [source_id],
        |row| row.get::<_, u32>(0),
    )
    .map_err(storage)
}

fn prepare_for(conn: &Connection, id: &str) -> Result<bool, CalendarError> {
    let value: Option<i64> = conn
        .query_row(
            "SELECT prepare FROM annotations WHERE event_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    Ok(value.unwrap_or(0) != 0)
}

fn freshness(source: &Source, now: DateTime<Utc>) -> crate::contracts::Freshness {
    use crate::contracts::Freshness;
    if source.last_error.is_some() {
        return Freshness::Failed;
    }
    match source.last_sync_at {
        None => Freshness::Never,
        Some(last) => {
            let limit = Duration::minutes(i64::from(source.refresh_minutes.max(15)) * 2);
            if last + limit < now {
                Freshness::Stale
            } else {
                Freshness::Fresh
            }
        }
    }
}

fn default_preferences() -> Preferences {
    Preferences {
        time_zone: "Europe/Lisbon".into(),
        first_weekday: "monday".into(),
        default_view: "week".into(),
        day_start_hour: 7,
        day_end_hour: 23,
    }
}

fn load_feeds(dir: &Path) -> Result<BTreeMap<String, String>, CalendarError> {
    let path = dir.join("feeds.json");
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(storage),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(storage(error)),
    }
}

fn save_feeds(dir: &Path, feeds: &BTreeMap<String, String>) -> Result<(), CalendarError> {
    let path = dir.join("feeds.json");
    let tmp = dir.join(format!("feeds.json.tmp.{}", uuid::Uuid::new_v4()));
    let json = serde_json::to_vec_pretty(feeds).map_err(storage)?;
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(storage)?;
        file.write_all(&json).map_err(storage)?;
    }
    std::fs::rename(&tmp, &path).map_err(storage)?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

fn storage(error: impl std::fmt::Display) -> CalendarError {
    CalendarError::Storage(error.to_string())
}
