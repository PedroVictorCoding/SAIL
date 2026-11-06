//! Durable memory store for Arena.
//!
//! One SQLite file in WAL mode, tuned for a large rotational disk:
//! - 16 KiB pages and a large WAL auto-checkpoint, so writes are mostly sequential appends
//!   and the HDD sees few random seeks;
//! - bodies are LZ4-compressed BLOBs, and the FTS5 index is *contentless*, so text is stored
//!   once (compressed) and the index holds only postings;
//! - every record carries namespace tokens (`s<session>`, `k<kind>`, `c<scope>`) inside the
//!   index, so scoped recall is an FTS posting-list intersection instead of a table scan;
//! - a separate read connection lets recall run while a write is in flight.
//!
//! `embedding` is reserved for vectors from Mind; it stays NULL until an embedder exists.

use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const SCHEMA_VERSION: i64 = 6;
/// Bodies shorter than this are stored raw; compression does not pay off below it.
const COMPRESS_MIN_BYTES: usize = 512;
const CODEC_RAW: i64 = 0;
const CODEC_LZ4: i64 = 1;
/// Query terms kept per search; longer prompts mostly add noise and posting-list work.
const MAX_QUERY_TERMS: usize = 16;

#[derive(Debug)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "arena store: {}", self.0)
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self(e.to_string())
    }
}

pub type StoreResult<T> = std::result::Result<T, StoreError>;

/// A record to archive. `scope` is the owning subsystem area (e.g. `voice`), `session` the
/// conversation or task it belongs to, `kind` what it is (`turn`, `tool_result`, `fact`).
#[derive(Debug, Clone)]
pub struct NewRecord<'a> {
    pub scope: &'a str,
    pub session: &'a str,
    pub kind: &'a str,
    pub turn: Option<u64>,
    pub gist: &'a str,
    pub body: &'a str,
    pub provenance: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredRecord {
    pub id: i64,
    pub scope: String,
    pub session: String,
    pub kind: String,
    pub turn: Option<u64>,
    pub created_at_ms: i64,
    pub gist: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub id: i64,
    pub kind: String,
    pub turn: Option<u64>,
    pub gist: String,
    /// BM25 score; lower is better (SQLite convention).
    pub score: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoreStats {
    pub records: i64,
    pub body_bytes_stored: i64,
    pub body_chars_original: i64,
    pub page_size: i64,
    pub journal_mode: String,
}

pub struct MemoryStore {
    writer: Mutex<Connection>,
    /// `None` for in-memory stores, where a second connection would be a different database.
    reader: Option<Mutex<Connection>>,
    path: Option<PathBuf>,
}

impl MemoryStore {
    /// Open (or create) a durable store at `path`, creating parent directories.
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError(format!("create {}: {e}", parent.display())))?;
        }
        let writer = Connection::open(path)?;
        configure(&writer, true)?;
        migrate(&writer)?;
        let reader = Connection::open(path)?;
        configure(&reader, false)?;
        Ok(Self {
            writer: Mutex::new(writer),
            reader: Some(Mutex::new(reader)),
            path: Some(path.to_path_buf()),
        })
    }

    /// Ephemeral store with the same schema; used for tests and when persistence is off.
    pub fn open_in_memory() -> StoreResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA temp_store=MEMORY; PRAGMA foreign_keys=ON;")?;
        migrate(&conn)?;
        Ok(Self {
            writer: Mutex::new(conn),
            reader: None,
            path: None,
        })
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub(crate) fn with_writer<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> StoreResult<T>,
    ) -> StoreResult<T> {
        let mut conn = self
            .writer
            .lock()
            .map_err(|_| StoreError("writer lock poisoned".into()))?;
        f(&mut conn)
    }

    pub(crate) fn with_reader<T>(
        &self,
        f: impl FnOnce(&Connection) -> StoreResult<T>,
    ) -> StoreResult<T> {
        match &self.reader {
            Some(reader) => {
                let conn = reader
                    .lock()
                    .map_err(|_| StoreError("reader lock poisoned".into()))?;
                f(&conn)
            }
            None => self.with_writer(|c| f(c)),
        }
    }

    /// Archive one record and return its id. Identical content in the same
    /// scope/session/kind is stored once; the existing id is returned.
    pub fn put(&self, record: &NewRecord<'_>) -> StoreResult<i64> {
        self.put_batch(std::slice::from_ref(record))
            .map(|ids| ids[0])
    }

    /// Archive several records in one transaction (one WAL commit for the whole batch).
    pub fn put_batch(&self, records: &[NewRecord<'_>]) -> StoreResult<Vec<i64>> {
        self.with_writer(|conn| {
            let tx = conn.transaction()?;
            let mut ids = Vec::with_capacity(records.len());
            for r in records {
                let hash = content_hash(r);
                let existing: Option<i64> = tx
                    .query_row("SELECT id FROM records WHERE hash=?1", [&hash[..]], |row| {
                        row.get(0)
                    })
                    .optional()?;
                if let Some(id) = existing {
                    ids.push(id);
                    continue;
                }
                let (codec, blob) = encode_body(r.body);
                tx.execute(
                    "INSERT INTO records(scope,session,kind,turn,created_at,gist,codec,body,body_chars,hash,provenance)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        r.scope,
                        r.session,
                        r.kind,
                        r.turn.map(|t| t as i64),
                        now_ms(),
                        r.gist,
                        codec,
                        blob,
                        r.body.chars().count() as i64,
                        &hash[..],
                        r.provenance,
                    ],
                )?;
                let id = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO records_fts(rowid,ns,text) VALUES(?1,?2,?3)",
                    params![id, namespace(r.scope, r.session, r.kind), index_text(r)],
                )?;
                ids.push(id);
            }
            tx.commit()?;
            Ok(ids)
        })
    }

    pub fn get(&self, id: i64) -> StoreResult<Option<StoredRecord>> {
        self.with_reader(|conn| {
            conn.query_row(
                "SELECT id,scope,session,kind,turn,created_at,gist,codec,body FROM records WHERE id=?1",
                [id],
                |row| {
                    let codec: i64 = row.get(7)?;
                    let blob: Vec<u8> = row.get(8)?;
                    Ok(StoredRecord {
                        id: row.get(0)?,
                        scope: row.get(1)?,
                        session: row.get(2)?,
                        kind: row.get(3)?,
                        turn: row.get::<_, Option<i64>>(4)?.map(|t| t as u64),
                        created_at_ms: row.get(5)?,
                        gist: row.get(6)?,
                        body: decode_body(codec, &blob),
                    })
                },
            )
            .optional()
            .map_err(Into::into)
        })
    }

    /// Ranked full-text search inside one scope/session. `exclude` drops ids the caller
    /// already has in context. Returns an empty list for queries with no usable terms.
    pub fn search(
        &self,
        scope: &str,
        session: &str,
        query: &str,
        exclude: &[i64],
        limit: usize,
    ) -> StoreResult<Vec<SearchHit>> {
        let terms = query_terms(query);
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let ns = format!(
            "ns:{} AND ns:{}",
            fts_quote(&session_token(session)),
            fts_quote(&scope_token(scope))
        );
        let text = terms
            .iter()
            .map(|t| fts_quote(t))
            .collect::<Vec<_>>()
            .join(" OR ");
        let expr = format!("{ns} AND text:({text})");
        self.with_reader(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT r.id,r.kind,r.turn,r.gist,bm25(records_fts,0.0,1.0) AS score
                 FROM records_fts JOIN records r ON r.id=records_fts.rowid
                 WHERE records_fts MATCH ?1
                 ORDER BY score, r.id DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![expr, (limit + exclude.len()) as i64], |row| {
                Ok(SearchHit {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    turn: row.get::<_, Option<i64>>(2)?.map(|t| t as u64),
                    gist: row.get(3)?,
                    score: row.get(4)?,
                })
            })?;
            let mut hits = Vec::new();
            for hit in rows {
                let hit = hit?;
                if !exclude.contains(&hit.id) {
                    hits.push(hit);
                }
                if hits.len() == limit {
                    break;
                }
            }
            Ok(hits)
        })
    }

    /// Delete every record of a session from the table and the index. Returns the count.
    pub fn delete_session(&self, scope: &str, session: &str) -> StoreResult<usize> {
        self.with_writer(|conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM records_fts WHERE rowid IN (SELECT id FROM records WHERE scope=?1 AND session=?2)",
                params![scope, session],
            )?;
            let n = tx.execute(
                "DELETE FROM records WHERE scope=?1 AND session=?2",
                params![scope, session],
            )?;
            tx.commit()?;
            Ok(n)
        })
    }

    /// Problems reported by SQLite's integrity and foreign-key checks; empty when healthy.
    /// Cheap enough to run at startup after an unclean shutdown.
    pub fn check_integrity(&self) -> StoreResult<Vec<String>> {
        self.with_reader(|conn| {
            let mut problems = Vec::new();
            let mut stmt = conn.prepare("PRAGMA integrity_check")?;
            for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
                let row = row?;
                if row != "ok" {
                    problems.push(row);
                }
            }
            let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
            for row in stmt.query_map([], |r| {
                Ok(format!(
                    "foreign key violation in {} row {:?} -> {}",
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, String>(2)?
                ))
            })? {
                problems.push(row?);
            }
            Ok(problems)
        })
    }

    /// Merge FTS segments and checkpoint the WAL. Run when idle (e.g. nightly), not per turn.
    pub fn maintain(&self) -> StoreResult<()> {
        self.with_writer(|conn| {
            conn.execute_batch(
                "INSERT INTO records_fts(records_fts) VALUES('optimize');
                 PRAGMA optimize;",
            )?;
            if self.path.is_some() {
                conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            }
            Ok(())
        })
    }

    pub fn stats(&self) -> StoreResult<StoreStats> {
        self.with_reader(|conn| {
            let (records, stored, original) = conn.query_row(
                "SELECT count(*), coalesce(sum(length(body)),0), coalesce(sum(body_chars),0) FROM records",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            Ok(StoreStats {
                records,
                body_bytes_stored: stored,
                body_chars_original: original,
                page_size: conn.query_row("PRAGMA page_size", [], |r| r.get(0))?,
                journal_mode: conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?,
            })
        })
    }
}

fn configure(conn: &Connection, writer: bool) -> StoreResult<()> {
    if writer {
        // page_size only takes effect before the first table exists and before WAL is on.
        conn.execute_batch("PRAGMA page_size=16384;")?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
    }
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA foreign_keys=ON;
         PRAGMA synchronous=NORMAL;
         PRAGMA temp_store=MEMORY;
         PRAGMA cache_size=-65536;
         PRAGMA mmap_size=1073741824;
         PRAGMA wal_autocheckpoint=4096;
         PRAGMA journal_size_limit=134217728;",
    )?;
    Ok(())
}

/// Schema steps; `MIGRATIONS[i]` moves a database from version `i` to `i + 1`. Each step runs
/// in one transaction together with its `user_version` bump, so a failed step leaves the
/// previous version intact.
const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS records(
         id INTEGER PRIMARY KEY,
         scope TEXT NOT NULL,
         session TEXT NOT NULL,
         kind TEXT NOT NULL,
         turn INTEGER,
         created_at INTEGER NOT NULL,
         gist TEXT NOT NULL,
         codec INTEGER NOT NULL,
         body BLOB NOT NULL,
         body_chars INTEGER NOT NULL,
         hash BLOB NOT NULL UNIQUE,
         provenance TEXT,
         embedding BLOB
     ) STRICT;
     CREATE INDEX IF NOT EXISTS records_by_session ON records(scope, session, id);
     CREATE VIRTUAL TABLE IF NOT EXISTS records_fts USING fts5(
         ns, text,
         content='', contentless_delete=1,
         tokenize='unicode61 remove_diacritics 2'
     );",
    crate::knowledge::schema::V2,
    crate::knowledge::schema::V3,
    crate::knowledge::schema::V4,
    crate::knowledge::schema::V5,
    crate::knowledge::schema::V6,
];

pub(crate) fn migrate(conn: &Connection) -> StoreResult<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StoreError(format!(
            "database schema v{version} is newer than this build (v{SCHEMA_VERSION})"
        )));
    }
    for (step, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let target = step + 1;
        conn.execute_batch(&format!(
            "BEGIN;\n{sql}\nPRAGMA user_version={target};\nCOMMIT;"
        ))
        .map_err(|e| {
            let _ = conn.execute_batch("ROLLBACK;");
            StoreError(format!("migration to schema v{target} failed: {e}"))
        })?;
    }
    Ok(())
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn sha16(parts: &[&[u8]]) -> [u8; 16] {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    for part in parts {
        ctx.update(part);
        ctx.update(&[0]);
    }
    let mut out = [0u8; 16];
    out.copy_from_slice(&ctx.finish().as_ref()[..16]);
    out
}

fn content_hash(r: &NewRecord<'_>) -> [u8; 16] {
    sha16(&[
        r.scope.as_bytes(),
        r.session.as_bytes(),
        r.kind.as_bytes(),
        r.body.as_bytes(),
    ])
}

fn encode_body(body: &str) -> (i64, Vec<u8>) {
    if body.len() < COMPRESS_MIN_BYTES {
        return (CODEC_RAW, body.as_bytes().to_vec());
    }
    let packed = lz4_flex::compress_prepend_size(body.as_bytes());
    if packed.len() < body.len() {
        (CODEC_LZ4, packed)
    } else {
        (CODEC_RAW, body.as_bytes().to_vec())
    }
}

pub(crate) fn decode_body(codec: i64, blob: &[u8]) -> String {
    let bytes = match codec {
        CODEC_LZ4 => lz4_flex::decompress_size_prepended(blob).unwrap_or_default(),
        _ => blob.to_vec(),
    };
    String::from_utf8_lossy(&bytes).into_owned()
}

fn token(prefix: char, raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 1);
    out.push(prefix);
    out.extend(raw.chars().filter(|c| c.is_ascii_alphanumeric()));
    out.to_ascii_lowercase()
}

fn session_token(session: &str) -> String {
    token('s', session)
}

fn scope_token(scope: &str) -> String {
    token('c', scope)
}

fn namespace(scope: &str, session: &str, kind: &str) -> String {
    format!(
        "{} {} {}",
        session_token(session),
        scope_token(scope),
        token('k', kind)
    )
}

fn index_text(r: &NewRecord<'_>) -> String {
    format!("{}\n{}", r.gist, r.body)
}

fn fts_quote(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

/// Words that carry little retrieval signal in English and Portuguese prompts.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "you", "your", "are", "was", "were", "with", "that", "this", "what",
    "how", "can", "could", "would", "should", "about", "from", "have", "has", "had", "not", "but",
    "all", "any", "please", "tell", "me", "que", "para", "com", "uma", "um", "por", "como", "mais",
    "isso", "esse", "essa", "est", "voc", "nao", "sim", "the", "did", "does", "its", "it's",
];

/// Distinct lowercase terms worth searching for, in order of first appearance.
pub fn query_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for word in query
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(str::to_lowercase)
    {
        if word.chars().count() < 3 || STOPWORDS.contains(&word.as_str()) {
            continue;
        }
        if !terms.contains(&word) {
            terms.push(word);
        }
        if terms.len() == MAX_QUERY_TERMS {
            break;
        }
    }
    terms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec<'a>(session: &'a str, body: &'a str) -> NewRecord<'a> {
        NewRecord {
            scope: "voice",
            session,
            kind: "turn",
            turn: Some(1),
            gist: "g",
            body,
            provenance: None,
        }
    }

    #[test]
    fn compresses_large_bodies_and_round_trips() {
        let store = MemoryStore::open_in_memory().unwrap();
        let body = "the quick brown fox jumps over the lazy dog\n".repeat(200);
        let id = store.put(&rec("a", &body)).unwrap();
        assert_eq!(store.get(id).unwrap().unwrap().body, body);
        let stats = store.stats().unwrap();
        assert!(
            stats.body_bytes_stored * 4 < stats.body_chars_original,
            "{stats:?}"
        );
    }

    #[test]
    fn identical_content_is_stored_once() {
        let store = MemoryStore::open_in_memory().unwrap();
        let a = store.put(&rec("a", "same text")).unwrap();
        let b = store.put(&rec("a", "same text")).unwrap();
        let c = store.put(&rec("b", "same text")).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(store.stats().unwrap().records, 2);
    }

    #[test]
    fn search_is_scoped_to_the_session_and_skips_excluded() {
        let store = MemoryStore::open_in_memory().unwrap();
        let a1 = store.put(&rec("alice", "alice likes bicycles")).unwrap();
        let a2 = store.put(&rec("alice", "bicycles need tyres")).unwrap();
        store.put(&rec("bob", "bob likes bicycles")).unwrap();
        let hits = store.search("voice", "alice", "bicycles?", &[], 5).unwrap();
        assert_eq!(hits.len(), 2);
        let hits = store
            .search("voice", "alice", "bicycles", &[a2], 5)
            .unwrap();
        assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![a1]);
        assert!(
            store
                .search("voice", "alice", "a an", &[], 5)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn search_matches_accents_loosely() {
        let store = MemoryStore::open_in_memory().unwrap();
        store
            .put(&rec("s", "Reunião com a equipa às três"))
            .unwrap();
        assert_eq!(
            store.search("voice", "s", "reuniao", &[], 3).unwrap().len(),
            1
        );
    }

    #[test]
    fn delete_session_removes_records_and_index_entries() {
        let store = MemoryStore::open_in_memory().unwrap();
        store.put(&rec("a", "secret plan alpha")).unwrap();
        store.put(&rec("b", "public plan beta")).unwrap();
        assert_eq!(store.delete_session("voice", "a").unwrap(), 1);
        assert!(
            store
                .search("voice", "a", "plan", &[], 5)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.search("voice", "b", "plan", &[], 5).unwrap().len(), 1);
        // Re-archiving the same content after deletion works (hash no longer present).
        store.put(&rec("a", "secret plan alpha")).unwrap();
        assert_eq!(
            store.search("voice", "a", "alpha", &[], 5).unwrap().len(),
            1
        );
    }

    #[test]
    fn durable_store_survives_reopen_with_hdd_tuning() {
        let dir = std::env::temp_dir().join(format!("sail-arena-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("nested").join("arena.db");
        let id = {
            let store = MemoryStore::open(&path).unwrap();
            let stats = store.stats().unwrap();
            assert_eq!(stats.page_size, 16384);
            assert_eq!(stats.journal_mode, "wal");
            store.put(&rec("s", "persist me across restarts")).unwrap()
        };
        let store = MemoryStore::open(&path).unwrap();
        assert_eq!(
            store.get(id).unwrap().unwrap().body,
            "persist me across restarts"
        );
        assert_eq!(
            store.search("voice", "s", "restarts", &[], 1).unwrap()[0].id,
            id
        );
        store.maintain().unwrap();
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v1_archive_upgrades_to_v2_without_losing_records() {
        let dir = std::env::temp_dir().join(format!("sail-arena-migrate-{}", uuid::Uuid::new_v4()));
        let path = dir.join("arena.db");
        std::fs::create_dir_all(&dir).unwrap();
        {
            // A database exactly as the previous release left it.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!(
                "BEGIN;{}PRAGMA user_version=1;COMMIT;",
                MIGRATIONS[0]
            ))
            .unwrap();
            conn.execute(
                "INSERT INTO records(scope,session,kind,turn,created_at,gist,codec,body,body_chars,hash)
                 VALUES('voice','s','turn',1,0,'g',0,CAST('kept' AS BLOB),4,x'00')",
                [],
            )
            .unwrap();
        }
        let store = MemoryStore::open(&path).unwrap();
        let version: i64 = store
            .with_reader(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(store.get(1).unwrap().unwrap().body, "kept");
        assert_eq!(store.knowledge().deletion_generation().unwrap(), 0);
        assert!(store.check_integrity().unwrap().is_empty());
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn newer_schema_is_refused_rather_than_downgraded() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("PRAGMA user_version={};", SCHEMA_VERSION + 1))
            .unwrap();
        assert!(migrate(&conn).is_err());
    }

    #[test]
    fn query_terms_drop_noise_and_cap_length() {
        assert_eq!(
            query_terms("What is the Forge status of job-5?"),
            vec!["forge", "status", "job"]
        );
        let long = (0..40)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(query_terms(&long).len(), MAX_QUERY_TERMS);
    }
}
