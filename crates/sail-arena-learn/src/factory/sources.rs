//! Revisioned source storage. Blobs are addressed by SHA-256; citations resolve only at
//! the current active revision. Historical metadata remains available for invalidation.
use super::contracts::{
    EducationalSource, FragmentLookup, IngestionStatus, SourceAnchorSet, SourceFragment,
    SourceRights, sha256_hex,
};
use super::ingest::{self, Extracted, IngestError, MAX_FILE_BYTES};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Bound owner-supplied metadata before storing it or returning it to clients.
pub const MAX_METADATA_BYTES: usize = 16 * 1024;
/// Bound one registry listing; callers can paginate with `list_page`.
pub const MAX_LIST_SOURCES: usize = 1000;

/// Storage and parsing failures remain observable to the importing job.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// Parsing errors preserve limit and DRM distinctions.
    #[error(transparent)]
    Ingest(#[from] IngestError),
    /// IO and SQLite errors never include source text.
    #[error("source storage: {0}")]
    Store(String),
    /// Invalid ids, conflicting imports and invalid metadata do not mutate storage.
    #[error("source rejected: {0}")]
    Invalid(String),
}
fn store(error: impl std::fmt::Display) -> SourceError {
    SourceError::Store(error.to_string())
}

/// Thread-safe registry using a caller-owned connection and local blob directory.
pub struct SourceRegistry {
    conn: Mutex<Connection>,
    blob_dir: PathBuf,
}
impl SourceRegistry {
    /// Initialize only factory-owned tables on the connection supplied by the caller.
    pub fn new(conn: Connection, blob_dir: PathBuf) -> Result<Self, SourceError> {
        std::fs::create_dir_all(&blob_dir).map_err(store)?;
        conn.execute_batch("PRAGMA busy_timeout=5000;
            CREATE TABLE IF NOT EXISTS learn_factory_sources_heads (
                source_id TEXT PRIMARY KEY, revision INTEGER NOT NULL, content_hash TEXT NOT NULL UNIQUE);
            CREATE TABLE IF NOT EXISTS learn_factory_sources_revisions (
                source_id TEXT NOT NULL, revision INTEGER NOT NULL, record TEXT NOT NULL,
                PRIMARY KEY(source_id, revision));
            CREATE TABLE IF NOT EXISTS learn_factory_sources_fragments (
                fragment_id TEXT PRIMARY KEY, source_id TEXT NOT NULL, revision INTEGER NOT NULL,
                position INTEGER NOT NULL, record TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS learn_factory_sources_fragment_order
                ON learn_factory_sources_fragments(source_id, revision, position);").map_err(store)?;
        Ok(Self {
            conn: Mutex::new(conn),
            blob_dir,
        })
    }
    /// Open a durable registry without choosing a global data directory.
    pub fn open(db_path: &Path, blob_dir: PathBuf) -> Result<Self, SourceError> {
        if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(store)?;
        }
        Self::new(Connection::open(db_path).map_err(store)?, blob_dir)
    }
    /// Use in-memory SQLite with real local blobs for tests or ephemeral callers.
    pub fn open_in_memory(blob_dir: PathBuf) -> Result<Self, SourceError> {
        Self::new(Connection::open_in_memory().map_err(store)?, blob_dir)
    }
    fn lock(&self) -> Result<MutexGuard<'_, Connection>, SourceError> {
        self.conn
            .lock()
            .map_err(|_| SourceError::Store("source connection lock poisoned".into()))
    }
    /// Import locally and extract immediately. Identical bytes return the original active
    /// record, retaining its owner-chosen rights and scope rather than widening them.
    #[allow(clippy::too_many_arguments)] // Keep the owner-specified import port.
    pub fn import(
        &self,
        file_name: &str,
        bytes: &[u8],
        title: Option<&str>,
        authors: Option<&[String]>,
        rights: SourceRights,
        scope: &str,
    ) -> Result<EducationalSource, SourceError> {
        if bytes.len() > MAX_FILE_BYTES {
            return Err(IngestError::Limit("file bytes").into());
        }
        validate_metadata(
            file_name,
            title,
            authors.unwrap_or_default(),
            &rights,
            scope,
        )?;
        let hash = sha256_hex(bytes);
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store)?;
        if let Some(existing) = by_hash(&tx, &hash)? {
            if matches!(existing.status, IngestionStatus::Removed) {
                return Err(SourceError::Invalid(
                    "source was removed; use replace to restore its id".into(),
                ));
            }
            self.write_blob(&hash, bytes)?;
            return Ok(existing);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let extracted = ingest::extract(file_name, bytes, &id, 1)?;
        let source = EducationalSource {
            source_id: id,
            title: title.unwrap_or(file_name).to_string(),
            authors: authors.unwrap_or_default().to_vec(),
            source_type: extracted.source_type,
            content_hash: hash,
            revision: 1,
            language: "und".into(),
            access_scope: scope.into(),
            rights,
            status: extracted.status.clone(),
            imported_ms: chrono::Utc::now().timestamp_millis(),
        };
        self.write_blob(&source.content_hash, bytes)?;
        persist(&tx, &source, &extracted)?;
        tx.commit().map_err(store)?;
        Ok(source)
    }
    /// Replace bytes under one source id, preserving rights, scope and prior revision history.
    /// Identical active bytes are a no-op; restoring a removed source creates a new revision.
    pub fn replace(
        &self,
        source_id: &str,
        file_name: &str,
        bytes: &[u8],
    ) -> Result<EducationalSource, SourceError> {
        if bytes.len() > MAX_FILE_BYTES {
            return Err(IngestError::Limit("file bytes").into());
        }
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store)?;
        let mut source = get_on(&tx, source_id)?
            .ok_or_else(|| SourceError::Invalid("source not found".into()))?;
        let hash = sha256_hex(bytes);
        if hash == source.content_hash && !matches!(source.status, IngestionStatus::Removed) {
            return Ok(source);
        }
        if by_hash(&tx, &hash)?.is_some_and(|s| s.source_id != source_id) {
            return Err(SourceError::Invalid(
                "replacement bytes already belong to another source".into(),
            ));
        }
        if source.revision >= MAX_REVISIONS {
            return Err(SourceError::Invalid("source revision cap reached".into()));
        }
        source.revision = source
            .revision
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| SourceError::Invalid("revision exhausted".into()))?;
        let extracted = ingest::extract(file_name, bytes, source_id, source.revision)?;
        source.content_hash = hash;
        source.source_type = extracted.source_type;
        source.status = extracted.status.clone();
        self.write_blob(&source.content_hash, bytes)?;
        persist(&tx, &source, &extracted)?;
        tx.commit().map_err(store)?;
        Ok(source)
    }
    /// Fetch current metadata, including a removal tombstone for invalidation consumers.
    pub fn get(&self, id: &str) -> Result<Option<EducationalSource>, SourceError> {
        let conn = self.lock()?;
        get_on(&conn, id)
    }
    /// List the first bounded page of active sources in stable id order.
    pub fn list(&self) -> Result<Vec<EducationalSource>, SourceError> {
        self.list_page(None, MAX_LIST_SOURCES)
    }
    /// Continue a stable source-id listing without loading the entire library.
    pub fn list_page(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EducationalSource>, SourceError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT r.record FROM learn_factory_sources_heads h
            JOIN learn_factory_sources_revisions r ON r.source_id=h.source_id AND r.revision=h.revision
            WHERE h.source_id > ?1 AND json_extract(r.record, '$.status.state') != 'removed'
            ORDER BY h.source_id LIMIT ?2").map_err(store)?;
        let rows = stmt
            .query_map(
                params![after.unwrap_or(""), limit.min(MAX_LIST_SOURCES) as i64],
                |r| r.get::<_, String>(0),
            )
            .map_err(store)?;
        rows.map(|r| serde_json::from_str(&r.map_err(store)?).map_err(store))
            .collect()
    }
    /// Tombstone a source. Old citations stop resolving, while revision metadata and blobs
    /// remain for invalidation/history; this is removal from the learning library, not erasure.
    pub fn remove(&self, id: &str) -> Result<Option<EducationalSource>, SourceError> {
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store)?;
        let Some(mut source) = get_on(&tx, id)? else {
            return Ok(None);
        };
        source.status = IngestionStatus::Removed;
        tx.execute("UPDATE learn_factory_sources_revisions SET record=?1 WHERE source_id=?2 AND revision=?3",
            params![serde_json::to_string(&source).map_err(store)?, id, source.revision as i64]).map_err(store)?;
        tx.commit().map_err(store)?;
        Ok(Some(source))
    }
    /// Return every known source anchor set, including the current revision. Invalidation
    /// callers may call before or after replacement/removal without losing the old hash.
    pub fn affected_by(&self, id: &str) -> Result<Vec<SourceAnchorSet>, SourceError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT record FROM learn_factory_sources_revisions WHERE source_id=?1 ORDER BY revision LIMIT ?2").map_err(store)?;
        let rows = stmt
            .query_map(params![id, MAX_REVISIONS as i64], |r| r.get::<_, String>(0))
            .map_err(store)?;
        rows.map(|row| {
            let source: EducationalSource =
                serde_json::from_str(&row.map_err(store)?).map_err(store)?;
            Ok(SourceAnchorSet {
                source_id: source.source_id,
                source_revision: source.revision,
                content_hash: source.content_hash,
            })
        })
        .collect()
    }
    /// Return current active fragments in reading order for retrieval/indexing.
    pub fn fragments(&self, id: &str) -> Result<Vec<SourceFragment>, SourceError> {
        let conn = self.lock()?;
        let Some(source) = get_on(&conn, id)? else {
            return Ok(Vec::new());
        };
        if matches!(source.status, IngestionStatus::Removed) {
            return Ok(Vec::new());
        }
        let mut stmt = conn.prepare("SELECT record FROM learn_factory_sources_fragments WHERE source_id=?1 AND revision=?2 ORDER BY position LIMIT ?3").map_err(store)?;
        let rows = stmt
            .query_map(
                params![id, source.revision as i64, ingest::MAX_FRAGMENTS as i64],
                |r| r.get::<_, String>(0),
            )
            .map_err(store)?;
        rows.map(|row| serde_json::from_str(&row.map_err(store)?).map_err(store))
            .collect()
    }
    fn write_blob(&self, hash: &str, bytes: &[u8]) -> Result<(), SourceError> {
        let destination = self.blob_dir.join(hash);
        if destination.try_exists().map_err(store)? {
            return verify_blob(&destination, hash);
        }
        let temporary = self
            .blob_dir
            .join(format!(".import-{}", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(store)?;
            file.write_all(bytes).map_err(store)?;
            file.sync_all().map_err(store)?;
            // hard_link is atomic and cannot overwrite a blob from another importer.
            match std::fs::hard_link(&temporary, &destination) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    return verify_blob(&destination, hash);
                }
                Err(e) => return Err(store(e)),
            }
            std::fs::File::open(&self.blob_dir)
                .map_err(store)?
                .sync_all()
                .map_err(store)?;
            Ok(())
        })();
        let _ = std::fs::remove_file(&temporary);
        result
    }
}
fn verify_blob(path: &Path, hash: &str) -> Result<(), SourceError> {
    let metadata = std::fs::symlink_metadata(path).map_err(store)?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES as u64 {
        return Err(SourceError::Invalid("invalid existing source blob".into()));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(store)?
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(store)?;
    if sha256_hex(&bytes) != hash {
        return Err(SourceError::Invalid(
            "existing source blob hash mismatch".into(),
        ));
    }
    Ok(())
}

/// Bound historical revisions so invalidation never silently truncates a source's history.
pub const MAX_REVISIONS: u64 = 10_000;
fn validate_metadata(
    file: &str,
    title: Option<&str>,
    authors: &[String],
    rights: &SourceRights,
    scope: &str,
) -> Result<(), SourceError> {
    if file.is_empty()
        || file.len() > MAX_METADATA_BYTES
        || title.is_some_and(|s| s.len() > MAX_METADATA_BYTES)
        || scope.is_empty()
        || scope.len() > MAX_METADATA_BYTES
        || authors.len() > 100
        || authors.iter().map(String::len).sum::<usize>() > MAX_METADATA_BYTES
        || matches!(rights, SourceRights::Open { license } if license.len() > MAX_METADATA_BYTES || license.is_empty())
    {
        return Err(SourceError::Invalid("metadata empty or exceeds cap".into()));
    }
    Ok(())
}
fn get_on(conn: &Connection, id: &str) -> Result<Option<EducationalSource>, SourceError> {
    let row: Option<String> = conn.query_row("SELECT r.record FROM learn_factory_sources_heads h
        JOIN learn_factory_sources_revisions r ON h.source_id=r.source_id AND h.revision=r.revision WHERE h.source_id=?1", [id], |r| r.get(0)).optional().map_err(store)?;
    row.map(|json| serde_json::from_str(&json).map_err(store))
        .transpose()
}
fn by_hash(conn: &Connection, hash: &str) -> Result<Option<EducationalSource>, SourceError> {
    let id: Option<String> = conn
        .query_row(
            "SELECT source_id FROM learn_factory_sources_heads WHERE content_hash=?1",
            [hash],
            |r| r.get(0),
        )
        .optional()
        .map_err(store)?;
    id.map(|id| get_on(conn, &id))
        .transpose()
        .map(Option::flatten)
}
fn persist(
    conn: &Connection,
    source: &EducationalSource,
    extracted: &Extracted,
) -> Result<(), SourceError> {
    if source.revision > MAX_REVISIONS {
        return Err(SourceError::Invalid("source revision cap reached".into()));
    }
    conn.execute(
        "INSERT INTO learn_factory_sources_revisions VALUES (?1, ?2, ?3)",
        params![
            source.source_id,
            source.revision as i64,
            serde_json::to_string(source).map_err(store)?
        ],
    )
    .map_err(store)?;
    conn.execute("INSERT INTO learn_factory_sources_heads VALUES (?1, ?2, ?3)
        ON CONFLICT(source_id) DO UPDATE SET revision=excluded.revision, content_hash=excluded.content_hash",
        params![source.source_id, source.revision as i64, source.content_hash]).map_err(store)?;
    let mut stmt = conn
        .prepare("INSERT INTO learn_factory_sources_fragments VALUES (?1, ?2, ?3, ?4, ?5)")
        .map_err(store)?;
    for (position, fragment) in extracted.fragments.iter().enumerate() {
        stmt.execute(params![
            fragment.fragment_id,
            source.source_id,
            source.revision as i64,
            position as i64,
            serde_json::to_string(fragment).map_err(store)?
        ])
        .map_err(store)?;
    }
    Ok(())
}
impl FragmentLookup for SourceRegistry {
    fn source(&self, id: &str) -> Option<EducationalSource> {
        self.get(id)
            .ok()
            .flatten()
            .filter(|s| !matches!(s.status, IngestionStatus::Removed))
    }
    fn fragment(&self, id: &str) -> Option<SourceFragment> {
        let conn = self.lock().ok()?;
        let row: Option<String> = conn.query_row("SELECT f.record FROM learn_factory_sources_fragments f
            JOIN learn_factory_sources_heads h ON h.source_id=f.source_id AND h.revision=f.revision
            JOIN learn_factory_sources_revisions r ON r.source_id=h.source_id AND r.revision=h.revision
            WHERE f.fragment_id=?1 AND json_extract(r.record, '$.status.state') != 'removed'", [id], |r| r.get(0)).optional().ok()?;
        row.and_then(|json| serde_json::from_str(&json).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("sail-source-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn import(registry: &SourceRegistry, bytes: &[u8]) -> EducationalSource {
        registry
            .import(
                "notes.md",
                bytes,
                Some("Signals"),
                None,
                SourceRights::Restricted,
                "learning:test",
            )
            .unwrap()
    }
    #[test]
    fn idempotence_revisions_lookup_and_invalidation() {
        let dir = Directory::new();
        let registry = SourceRegistry::open_in_memory(dir.0.clone()).unwrap();
        let source = import(&registry, b"# Signals\n\nDefinition: a pulse is an event.");
        assert_eq!(
            source,
            import(&registry, b"# Signals\n\nDefinition: a pulse is an event.")
        );
        let fragment = registry
            .fragments(&source.source_id)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            registry.fragment(&fragment.fragment_id),
            Some(fragment.clone())
        );
        let replacement = registry
            .replace(
                &source.source_id,
                "notes.md",
                b"# New signals\n\nExample: a bell.",
            )
            .unwrap();
        assert_eq!(replacement.revision, 2);
        assert_eq!(registry.affected_by(&source.source_id).unwrap().len(), 2);
        assert!(registry.fragment(&fragment.fragment_id).is_none());
        assert_eq!(registry.list().unwrap().len(), 1);
        registry.remove(&source.source_id).unwrap();
        assert!(registry.source(&source.source_id).is_none());
        assert!(registry.fragments(&source.source_id).unwrap().is_empty());
        assert!(registry.list().unwrap().is_empty());
        assert_eq!(registry.affected_by(&source.source_id).unwrap().len(), 2);
        assert!(dir.0.join(source.content_hash).exists());
    }
    #[test]
    fn rejected_replacement_is_atomic_and_restart_preserves_sources() {
        let dir = Directory::new();
        let db = dir.0.join("source.sqlite");
        let blobs = dir.0.join("blobs");
        let registry = SourceRegistry::open(&db, blobs.clone()).unwrap();
        let source = import(&registry, b"Original notes.");
        assert!(
            registry
                .replace(&source.source_id, "bad.pdf", b"%PDF-1.4 /Encrypt 1 0 R")
                .is_err()
        );
        assert_eq!(
            registry.get(&source.source_id).unwrap(),
            Some(source.clone())
        );
        drop(registry);
        let registry = SourceRegistry::open(&db, blobs).unwrap();
        assert_eq!(registry.get(&source.source_id).unwrap(), Some(source));
    }
    #[test]
    fn duplicate_import_cannot_widen_rights_or_scope() {
        let dir = Directory::new();
        let registry = SourceRegistry::open_in_memory(dir.0.clone()).unwrap();
        let first = import(&registry, b"Private notes.");
        let second = registry
            .import(
                "new.txt",
                b"Private notes.",
                None,
                None,
                SourceRights::Open {
                    license: "MIT".into(),
                },
                "public",
            )
            .unwrap();
        assert_eq!(first, second);
        assert!(registry.list_page(None, 0).unwrap().is_empty());
    }
    #[test]
    fn restoring_removed_source_bumps_revision_and_corrupt_blobs_fail_closed() {
        let dir = Directory::new();
        let registry = SourceRegistry::open_in_memory(dir.0.clone()).unwrap();
        let original = import(&registry, b"Persistent notes.");
        registry.remove(&original.source_id).unwrap();
        let restored = registry
            .replace(&original.source_id, "notes.txt", b"Persistent notes.")
            .unwrap();
        assert_eq!(restored.revision, 2);
        assert_eq!(
            registry.affected_by(&original.source_id).unwrap()[0].content_hash,
            sha256_hex(b"Persistent notes.")
        );
        std::fs::write(dir.0.join(original.content_hash), b"corrupt").unwrap();
        assert!(
            registry
                .import(
                    "notes.txt",
                    b"Persistent notes.",
                    None,
                    None,
                    SourceRights::Restricted,
                    "learning:test"
                )
                .is_err()
        );
    }
    #[test]
    fn concurrent_connections_deduplicate_one_import() {
        let dir = Directory::new();
        let db = dir.0.join("sources.sqlite");
        let blobs = dir.0.join("blobs");
        let first = SourceRegistry::open(&db, blobs.clone()).unwrap();
        let second = SourceRegistry::open(&db, blobs).unwrap();
        let a = std::thread::spawn(move || import(&first, b"Concurrent notes."));
        let b = std::thread::spawn(move || import(&second, b"Concurrent notes."));
        assert_eq!(a.join().unwrap(), b.join().unwrap());
    }
}
