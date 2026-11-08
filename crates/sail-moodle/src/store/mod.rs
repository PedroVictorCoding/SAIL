//! Local SQLite store: sync state, the latest snapshot per course, the change log, the file
//! mirror index, extraction bookkeeping, items, grading rules and Moodle usage counters.
//!
//! Mirrored file bytes live next to the database in `files/<sha256>`; the database only
//! indexes them. Every listing is bounded by [`MAX_LIST`].
use crate::contracts::*;
use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Most rows any listing returns.
pub const MAX_LIST: usize = 2_000;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("Moodle storage: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Moodle storage: {0}")]
    Io(#[from] std::io::Error),
    #[error("Moodle storage holds an unreadable record: {0}")]
    Json(#[from] serde_json::Error),
}

/// Sync bookkeeping shown on the status page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    pub user_id: Option<i64>,
    pub site_name: Option<String>,
    pub last_ok: Option<DateTime<Utc>>,
    pub last_attempt: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
}

/// A change plus whether the owner has seen it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoredChange {
    #[serde(flatten)]
    pub change: Change,
    pub seen: bool,
}

/// One downloaded version of a course file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersion {
    pub key: FileKey,
    pub course_id: i64,
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
    pub sha256: String,
    pub downloaded_at: DateTime<Utc>,
}

/// When parts of a course were last read from Moodle. The service uses it to refresh
/// slow-moving parts (ended courses, grades) less often.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CourseMeta {
    pub contents_at: Option<DateTime<Utc>>,
    pub grades_at: Option<DateTime<Utc>>,
}

/// Moodle requests and downloaded bytes on one UTC day.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub requests: u64,
    pub bytes: u64,
}

pub struct Store {
    conn: Mutex<Connection>,
    dir: PathBuf,
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn ts(t: Option<DateTime<Utc>>) -> Option<i64> {
    t.map(|t| t.timestamp())
}

fn from_ts(t: Option<i64>) -> Option<DateTime<Utc>> {
    t.and_then(|t| DateTime::from_timestamp(t, 0))
}

fn limit(n: usize) -> i64 {
    n.min(MAX_LIST) as i64
}

impl Store {
    /// Open `dir/moodle.db` (and create `dir/files/`).
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir.join("files"))?;
        let conn = Connection::open(dir.join("moodle.db"))?;
        Self::init(conn, dir.to_path_buf())
    }

    /// An in-memory store for tests; blobs go to a per-process temp folder.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let dir = std::env::temp_dir().join(format!("sail-moodle-mem-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("files"))?;
        Self::init(Connection::open_in_memory()?, dir)
    }

    fn init(conn: Connection, dir: PathBuf) -> Result<Self, StoreError> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS moodle_state (id INTEGER PRIMARY KEY CHECK (id = 1), json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS moodle_snapshots (
                 course_id INTEGER PRIMARY KEY, json TEXT NOT NULL, fetched_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS moodle_course_meta (
                 course_id INTEGER PRIMARY KEY, contents_at INTEGER, grades_at INTEGER);
             CREATE TABLE IF NOT EXISTS moodle_changes (
                 id TEXT PRIMARY KEY, course_id INTEGER NOT NULL, json TEXT NOT NULL,
                 observed_at INTEGER NOT NULL, seen INTEGER NOT NULL DEFAULT 0);
             CREATE INDEX IF NOT EXISTS moodle_changes_time ON moodle_changes(observed_at);
             CREATE TABLE IF NOT EXISTS moodle_files (
                 course_id INTEGER NOT NULL, module_id INTEGER NOT NULL, filepath TEXT NOT NULL,
                 filename TEXT NOT NULL, size INTEGER NOT NULL, modified INTEGER,
                 sha256 TEXT NOT NULL, downloaded_at INTEGER NOT NULL,
                 PRIMARY KEY (module_id, filepath, filename, sha256, size));
             CREATE INDEX IF NOT EXISTS moodle_files_course ON moodle_files(course_id);
             CREATE TABLE IF NOT EXISTS moodle_docs (
                 doc_id TEXT PRIMARY KEY, course_id INTEGER NOT NULL, outcome TEXT NOT NULL,
                 at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS moodle_rejected (
                 doc_id TEXT NOT NULL, claim TEXT NOT NULL, reason TEXT NOT NULL, at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS moodle_items (
                 id TEXT PRIMARY KEY, course_id INTEGER NOT NULL, date TEXT NOT NULL,
                 time TEXT, json TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS moodle_items_date ON moodle_items(date);
             CREATE TABLE IF NOT EXISTS moodle_dismissed (id TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS moodle_assessment (
                 course_id INTEGER PRIMARY KEY, json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS moodle_usage (
                 day TEXT PRIMARY KEY, requests INTEGER NOT NULL, bytes INTEGER NOT NULL);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            dir,
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // --- sync state ------------------------------------------------------------------

    pub fn sync_state(&self) -> Result<SyncState, StoreError> {
        let json: Option<String> = self
            .conn()
            .query_row("SELECT json FROM moodle_state WHERE id = 1", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(match json {
            Some(j) => serde_json::from_str(&j)?,
            None => SyncState::default(),
        })
    }

    pub fn set_sync_state(&self, state: &SyncState) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO moodle_state (id, json) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET json = excluded.json",
            params![serde_json::to_string(state)?],
        )?;
        Ok(())
    }

    // --- snapshots -------------------------------------------------------------------

    /// Replace a course's latest snapshot.
    pub fn save_snapshot(&self, snapshot: &CourseSnapshot) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO moodle_snapshots (course_id, json, fetched_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(course_id) DO UPDATE SET json = excluded.json, fetched_at = excluded.fetched_at",
            params![
                snapshot.course.id,
                serde_json::to_string(snapshot)?,
                snapshot.fetched_at.timestamp()
            ],
        )?;
        Ok(())
    }

    pub fn snapshot(&self, course_id: i64) -> Result<Option<CourseSnapshot>, StoreError> {
        let json: Option<String> = self
            .conn()
            .query_row(
                "SELECT json FROM moodle_snapshots WHERE course_id = ?1",
                params![course_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(match json {
            Some(j) => Some(serde_json::from_str(&j)?),
            None => None,
        })
    }

    /// Courses with a stored snapshot, by full name.
    pub fn courses(&self) -> Result<Vec<Course>, StoreError> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT json_extract(json, '$.course') FROM moodle_snapshots LIMIT ?1")?;
        let rows = stmt.query_map(params![limit(MAX_COURSES)], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str::<Course>(&row?)?);
        }
        out.sort_by(|a, b| a.full_name.cmp(&b.full_name));
        Ok(out)
    }

    /// Drop a course's snapshot, items and grading rules (changes and files stay).
    pub fn forget_course(&self, course_id: i64) -> Result<(), StoreError> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM moodle_snapshots WHERE course_id = ?1",
            params![course_id],
        )?;
        conn.execute(
            "DELETE FROM moodle_course_meta WHERE course_id = ?1",
            params![course_id],
        )?;
        conn.execute(
            "DELETE FROM moodle_items WHERE course_id = ?1",
            params![course_id],
        )?;
        conn.execute(
            "DELETE FROM moodle_assessment WHERE course_id = ?1",
            params![course_id],
        )?;
        Ok(())
    }

    pub fn course_meta(&self, course_id: i64) -> Result<CourseMeta, StoreError> {
        let row: Option<(Option<i64>, Option<i64>)> = self
            .conn()
            .query_row(
                "SELECT contents_at, grades_at FROM moodle_course_meta WHERE course_id = ?1",
                params![course_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row
            .map(|(c, g)| CourseMeta {
                contents_at: from_ts(c),
                grades_at: from_ts(g),
            })
            .unwrap_or_default())
    }

    pub fn set_course_meta(&self, course_id: i64, meta: &CourseMeta) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO moodle_course_meta (course_id, contents_at, grades_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(course_id) DO UPDATE SET contents_at = excluded.contents_at, grades_at = excluded.grades_at",
            params![course_id, ts(meta.contents_at), ts(meta.grades_at)],
        )?;
        Ok(())
    }

    // --- changes ---------------------------------------------------------------------

    /// Insert new changes; known ids are ignored. Returns how many were new.
    pub fn record_changes(&self, changes: &[Change]) -> Result<usize, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut added = 0;
        for c in changes {
            added += tx.execute(
                "INSERT OR IGNORE INTO moodle_changes (id, course_id, json, observed_at) VALUES (?1, ?2, ?3, ?4)",
                params![c.id, c.course_id, serde_json::to_string(c)?, c.observed_at.timestamp()],
            )?;
        }
        tx.commit()?;
        Ok(added)
    }

    /// Changes, newest first.
    pub fn changes(
        &self,
        since: Option<DateTime<Utc>>,
        course: Option<i64>,
        unseen_only: bool,
        max: usize,
    ) -> Result<Vec<StoredChange>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT json, seen FROM moodle_changes
             WHERE observed_at >= ?1 AND (?2 IS NULL OR course_id = ?2) AND (?3 = 0 OR seen = 0)
             ORDER BY observed_at DESC, id LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                since.map(|s| s.timestamp()).unwrap_or(i64::MIN),
                course,
                unseen_only as i64,
                limit(max)
            ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (json, seen) = row?;
            out.push(StoredChange {
                change: serde_json::from_str(&json)?,
                seen: seen != 0,
            });
        }
        Ok(out)
    }

    pub fn mark_seen(&self, ids: &[String]) -> Result<usize, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut n = 0;
        for id in ids.iter().take(MAX_LIST) {
            n += tx.execute(
                "UPDATE moodle_changes SET seen = 1 WHERE id = ?1 AND seen = 0",
                params![id],
            )?;
        }
        tx.commit()?;
        Ok(n)
    }

    // --- files -----------------------------------------------------------------------

    /// `dir/files/<sha256>` for a well-formed hash; the file need not exist.
    pub fn blob_path(&self, sha256: &str) -> Option<PathBuf> {
        is_sha256(sha256).then(|| self.dir.join("files").join(sha256))
    }

    fn versions(&self, course_id: i64) -> Result<Vec<FileVersion>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT module_id, filepath, filename, size, modified, sha256, downloaded_at
             FROM moodle_files WHERE course_id = ?1 ORDER BY downloaded_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![course_id, limit(MAX_LIST)], |r| {
            Ok(FileVersion {
                key: FileKey {
                    module_id: r.get(0)?,
                    filepath: r.get(1)?,
                    filename: r.get(2)?,
                },
                course_id,
                size: r.get::<_, i64>(3)?.max(0) as u64,
                modified: from_ts(r.get(4)?),
                sha256: r.get(5)?,
                downloaded_at: from_ts(Some(r.get(6)?)).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The newest version of each file of a course.
    pub fn known_files(&self, course_id: i64) -> Result<HashMap<FileKey, FileVersion>, StoreError> {
        let mut out = HashMap::new();
        for v in self.versions(course_id)? {
            out.entry(v.key.clone()).or_insert(v);
        }
        Ok(out)
    }

    /// Every downloaded version, newest first.
    pub fn file_versions(&self, course_id: i64) -> Result<Vec<FileVersion>, StoreError> {
        self.versions(course_id)
    }

    pub fn record_file_version(
        &self,
        file: &FileRef,
        done: &DownloadedFile,
    ) -> Result<FileVersion, StoreError> {
        let version = FileVersion {
            key: file.key(),
            course_id: file.course_id,
            size: file.size,
            modified: file.modified,
            sha256: done.sha256.clone(),
            downloaded_at: Utc::now(),
        };
        self.conn().execute(
            "INSERT INTO moodle_files (course_id, module_id, filepath, filename, size, modified, sha256, downloaded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT DO UPDATE SET modified = excluded.modified, downloaded_at = excluded.downloaded_at",
            params![
                version.course_id,
                version.key.module_id,
                version.key.filepath,
                version.key.filename,
                version.size as i64,
                ts(version.modified),
                version.sha256,
                version.downloaded_at.timestamp()
            ],
        )?;
        Ok(version)
    }

    // --- extraction bookkeeping --------------------------------------------------------

    pub fn doc_done(&self, doc_id: &str) -> Result<bool, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM moodle_docs WHERE doc_id = ?1",
                params![doc_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn mark_doc(&self, doc_id: &str, course_id: i64, outcome: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO moodle_docs (doc_id, course_id, outcome, at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(doc_id) DO UPDATE SET outcome = excluded.outcome, at = excluded.at",
            params![doc_id, course_id, outcome, Utc::now().timestamp()],
        )?;
        Ok(())
    }

    /// Keep refused model claims for review. Only the newest [`MAX_LIST`] are kept.
    pub fn save_rejected(&self, claims: &[RejectedClaim]) -> Result<(), StoreError> {
        if claims.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let now = Utc::now().timestamp();
        for c in claims.iter().take(100) {
            tx.execute(
                "INSERT INTO moodle_rejected (doc_id, claim, reason, at) VALUES (?1, ?2, ?3, ?4)",
                params![c.doc_id, c.claim, c.reason, now],
            )?;
        }
        tx.execute(
            "DELETE FROM moodle_rejected WHERE rowid NOT IN
             (SELECT rowid FROM moodle_rejected ORDER BY at DESC LIMIT ?1)",
            params![limit(MAX_LIST)],
        )?;
        tx.commit()?;
        Ok(())
    }

    // --- items -----------------------------------------------------------------------

    /// Items by date then time.
    pub fn items(
        &self,
        course: Option<i64>,
        from: Option<NaiveDate>,
        include_dismissed: bool,
    ) -> Result<Vec<AcademicItem>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT json FROM moodle_items
             WHERE (?1 IS NULL OR course_id = ?1) AND date >= ?2
               AND (?3 = 1 OR id NOT IN (SELECT id FROM moodle_dismissed))
             ORDER BY date, COALESCE(time, ''), id LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                course,
                from.map(|d| d.to_string()).unwrap_or_default(),
                include_dismissed as i64,
                limit(MAX_LIST)
            ],
            |r| r.get::<_, String>(0),
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        Ok(out)
    }

    /// Replace a course's items with the merged list, in one transaction. Dismissals
    /// are kept by id.
    pub fn replace_items(&self, course_id: i64, items: &[AcademicItem]) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM moodle_items WHERE course_id = ?1",
            params![course_id],
        )?;
        for item in items.iter().take(MAX_LIST) {
            tx.execute(
                "INSERT OR REPLACE INTO moodle_items (id, course_id, date, time, json) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    item.id,
                    course_id,
                    item.when.date.to_string(),
                    item.when.time.map(|t| t.format("%H:%M").to_string()),
                    serde_json::to_string(item)?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Hide an item. Returns false for an unknown id.
    pub fn dismiss_item(&self, id: &str) -> Result<bool, StoreError> {
        let conn = self.conn();
        let exists = conn
            .query_row(
                "SELECT 1 FROM moodle_items WHERE id = ?1",
                params![id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if exists {
            conn.execute(
                "INSERT OR IGNORE INTO moodle_dismissed (id) VALUES (?1)",
                params![id],
            )?;
        }
        Ok(exists)
    }

    // --- assessment ------------------------------------------------------------------

    pub fn assessment(&self, course_id: i64) -> Result<Vec<AssessmentComponent>, StoreError> {
        let json: Option<String> = self
            .conn()
            .query_row(
                "SELECT json FROM moodle_assessment WHERE course_id = ?1",
                params![course_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(match json {
            Some(j) => serde_json::from_str(&j)?,
            None => Vec::new(),
        })
    }

    pub fn replace_assessment(
        &self,
        course_id: i64,
        components: &[AssessmentComponent],
    ) -> Result<(), StoreError> {
        let capped: Vec<&AssessmentComponent> = components.iter().take(100).collect();
        self.conn().execute(
            "INSERT INTO moodle_assessment (course_id, json) VALUES (?1, ?2)
             ON CONFLICT(course_id) DO UPDATE SET json = excluded.json",
            params![course_id, serde_json::to_string(&capped)?],
        )?;
        Ok(())
    }

    // --- usage -----------------------------------------------------------------------

    /// Requests and bytes used today (UTC).
    pub fn usage_today(&self) -> Result<Usage, StoreError> {
        let day = Utc::now().date_naive().to_string();
        let row: Option<(i64, i64)> = self
            .conn()
            .query_row(
                "SELECT requests, bytes FROM moodle_usage WHERE day = ?1",
                params![day],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row
            .map(|(r, b)| Usage {
                requests: r.max(0) as u64,
                bytes: b.max(0) as u64,
            })
            .unwrap_or_default())
    }

    /// Add to today's counters; keeps 60 days.
    pub fn add_usage(&self, requests: u64, bytes: u64) -> Result<(), StoreError> {
        let conn = self.conn();
        let today = Utc::now().date_naive();
        conn.execute(
            "INSERT INTO moodle_usage (day, requests, bytes) VALUES (?1, ?2, ?3)
             ON CONFLICT(day) DO UPDATE SET requests = requests + excluded.requests, bytes = bytes + excluded.bytes",
            params![today.to_string(), requests as i64, bytes as i64],
        )?;
        conn.execute(
            "DELETE FROM moodle_usage WHERE day < ?1",
            params![(today - chrono::Duration::days(60)).to_string()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::diff::tests::{file, snapshot, t};

    fn item(id: &str, date: &str) -> AcademicItem {
        let doc = "d".to_string();
        AcademicItem {
            id: id.into(),
            course_id: 7,
            kind: ItemKind::Test,
            title: "Teste".into(),
            when: ItemWhen {
                date: date.parse().unwrap(),
                time: None,
                end_date: None,
                end_time: None,
            },
            location: None,
            weight_percent: None,
            origin: ItemOrigin::Pattern,
            confidence: Confidence::Likely,
            evidence: vec![Evidence {
                doc_id: doc,
                source: DocSource::Event { event_id: 1 },
                label: "x".into(),
                quote: "Teste 12/12".into(),
            }],
        }
    }

    #[test]
    fn snapshot_round_trip_and_forget() {
        let s = Store::open_in_memory().unwrap();
        let snap = snapshot(t(0));
        s.save_snapshot(&snap).unwrap();
        assert_eq!(s.snapshot(7).unwrap().unwrap(), snap);
        assert_eq!(s.courses().unwrap(), vec![snap.course.clone()]);
        s.forget_course(7).unwrap();
        assert!(s.snapshot(7).unwrap().is_none());
    }

    #[test]
    fn changes_are_idempotent_and_seen_is_tracked() {
        let s = Store::open_in_memory().unwrap();
        let a = snapshot(t(0));
        let mut b = snapshot(t(1));
        b.assignments[0].due = Some(t(30));
        let changes = crate::sync::diff::diff(Some(&a), &b);
        assert_eq!(s.record_changes(&changes).unwrap(), 1);
        assert_eq!(s.record_changes(&changes).unwrap(), 0);
        let unseen = s.changes(None, Some(7), true, 10).unwrap();
        assert_eq!(unseen.len(), 1);
        assert_eq!(s.mark_seen(&[unseen[0].change.id.clone()]).unwrap(), 1);
        assert!(s.changes(None, None, true, 10).unwrap().is_empty());
        assert_eq!(s.changes(Some(t(5)), None, false, 10).unwrap().len(), 0);
        let json = serde_json::to_value(&s.changes(None, None, false, 1).unwrap()[0]).unwrap();
        assert_eq!(json["seen"], true);
        assert!(json["kind"].is_string(), "change fields are flattened");
    }

    #[test]
    fn dismissal_survives_replace() {
        let s = Store::open_in_memory().unwrap();
        s.replace_items(7, &[item("a", "2026-12-12"), item("b", "2026-12-01")])
            .unwrap();
        let ids: Vec<String> = s
            .items(None, None, false)
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(ids, vec!["b", "a"], "sorted by date");
        assert!(s.dismiss_item("a").unwrap());
        assert!(!s.dismiss_item("nope").unwrap());
        s.replace_items(7, &[item("a", "2026-12-12")]).unwrap();
        assert!(s.items(Some(7), None, false).unwrap().is_empty());
        assert_eq!(s.items(Some(7), None, true).unwrap().len(), 1);
        assert!(
            s.items(None, Some("2026-12-13".parse().unwrap()), true)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn file_versions_and_blob_paths() {
        let s = Store::open_in_memory().unwrap();
        let f = file(10, "a.pdf", 5);
        let done = DownloadedFile {
            path: PathBuf::from("/x"),
            bytes: 5,
            sha256: "a".repeat(64),
        };
        s.record_file_version(&f, &done).unwrap();
        let mut g = f.clone();
        g.size = 6;
        s.record_file_version(
            &g,
            &DownloadedFile {
                sha256: "b".repeat(64),
                ..done
            },
        )
        .unwrap();
        let known = s.known_files(7).unwrap();
        assert_eq!(known.len(), 1);
        assert_eq!(s.file_versions(7).unwrap().len(), 2);
        assert!(s.blob_path(&"a".repeat(64)).is_some());
        assert!(s.blob_path("../x").is_none());
        assert!(s.blob_path(&"A".repeat(64)).is_none());
    }

    #[test]
    fn state_usage_docs_and_meta() {
        let s = Store::open_in_memory().unwrap();
        let mut st = s.sync_state().unwrap();
        st.consecutive_failures = 2;
        s.set_sync_state(&st).unwrap();
        assert_eq!(s.sync_state().unwrap().consecutive_failures, 2);
        s.add_usage(3, 100).unwrap();
        s.add_usage(2, 50).unwrap();
        assert_eq!(
            s.usage_today().unwrap(),
            Usage {
                requests: 5,
                bytes: 150
            }
        );
        assert!(!s.doc_done("d1").unwrap());
        s.mark_doc("d1", 7, "done").unwrap();
        assert!(s.doc_done("d1").unwrap());
        let meta = CourseMeta {
            contents_at: Some(t(0)),
            grades_at: None,
        };
        s.set_course_meta(7, &meta).unwrap();
        assert_eq!(s.course_meta(7).unwrap(), meta);
    }
}
