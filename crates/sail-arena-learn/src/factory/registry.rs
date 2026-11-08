//! Published course revisions: immutable, append-only, deduplicated by package hash.
//!
//! The registry is the only place a generated course becomes visible to learners. A revision
//! is written once and never edited; a new revision supersedes it for new sessions while the
//! learner's evidence (keyed by course, lesson and step ids) carries over. At startup the
//! daemon loads the latest revision of every course into `CourseEngine`.
use super::contracts::{
    ContentHash, CourseId, PackageRevision, ReceiptId, SourceAnchorSet, SourceRights, sha256_hex,
};
use crate::courses::content::{CoursePackage, Track};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Largest package JSON the registry stores. A course far beyond this is a generation bug.
pub const MAX_PACKAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("package rejected: {0}")]
    Invalid(String),
    #[error("registry storage: {0}")]
    Store(String),
}

fn store_err(e: impl std::fmt::Display) -> RegistryError {
    RegistryError::Store(e.to_string())
}

/// What the caller asks to publish. Revision number and time are assigned here.
pub struct PublishRequest<'a> {
    pub package: &'a CoursePackage,
    pub receipts: Vec<ReceiptId>,
    pub built_from: Vec<SourceAnchorSet>,
    pub rights: SourceRights,
    pub now_ms: i64,
}

/// Whether a publish created a new revision or found the same package already published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Published {
    New(u64),
    Unchanged(u64),
}

pub struct PackageRegistry {
    conn: Mutex<Connection>,
}

impl PackageRegistry {
    pub fn open(path: &Path) -> Result<Self, RegistryError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(store_err)?;
        }
        Self::init(Connection::open(path).map_err(store_err)?)
    }

    pub fn open_in_memory() -> Result<Self, RegistryError> {
        Self::init(Connection::open_in_memory().map_err(store_err)?)
    }

    fn init(conn: Connection) -> Result<Self, RegistryError> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS learn_factory_registry_revisions (
                 course_id TEXT NOT NULL,
                 revision INTEGER NOT NULL,
                 package_hash TEXT NOT NULL,
                 package_json TEXT NOT NULL,
                 receipts TEXT NOT NULL,
                 built_from TEXT NOT NULL,
                 rights TEXT NOT NULL,
                 published_ms INTEGER NOT NULL,
                 PRIMARY KEY (course_id, revision),
                 UNIQUE (course_id, package_hash));",
        )
        .map_err(store_err)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Stores a validated package as the course's next revision. Publishing a package whose
    /// hash is already stored for the course is a no-op, so a retried publish job can't
    /// create a duplicate. The package must pass `CoursePackage::validate`.
    pub fn publish(&self, req: PublishRequest<'_>) -> Result<Published, RegistryError> {
        req.package.validate().map_err(RegistryError::Invalid)?;
        let json = serde_json::to_string(req.package).map_err(store_err)?;
        if json.len() > MAX_PACKAGE_BYTES {
            return Err(RegistryError::Invalid(format!(
                "package is {} bytes, above the {MAX_PACKAGE_BYTES}-byte cap",
                json.len()
            )));
        }
        let hash = sha256_hex(json.as_bytes());
        let course = &req.package.id;
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(store_err)?;
        let existing: Option<u64> = tx
            .query_row(
                "SELECT revision FROM learn_factory_registry_revisions
                 WHERE course_id = ?1 AND package_hash = ?2",
                params![course, hash],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(store_err)?
            .map(|r| r as u64);
        if let Some(revision) = existing {
            return Ok(Published::Unchanged(revision));
        }
        let next: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(revision), 0) + 1 FROM learn_factory_registry_revisions
                 WHERE course_id = ?1",
                params![course],
                |r| r.get(0),
            )
            .map_err(store_err)?;
        tx.execute(
            "INSERT INTO learn_factory_registry_revisions
             (course_id, revision, package_hash, package_json, receipts, built_from, rights,
              published_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                course,
                next,
                hash,
                json,
                serde_json::to_string(&req.receipts).map_err(store_err)?,
                serde_json::to_string(&req.built_from).map_err(store_err)?,
                serde_json::to_string(&req.rights).map_err(store_err)?,
                req.now_ms,
            ],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(Published::New(next as u64))
    }

    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
        Ok(RawRow {
            course_id: r.get(0)?,
            revision: r.get(1)?,
            package_hash: r.get(2)?,
            package_json: r.get(3)?,
            receipts: r.get(4)?,
            built_from: r.get(5)?,
            rights: r.get(6)?,
            published_ms: r.get(7)?,
        })
    }

    /// The newest revision of one course.
    pub fn latest(&self, course_id: &str) -> Result<Option<PackageRevision>, RegistryError> {
        let conn = self.lock();
        let raw = conn
            .query_row(
                "SELECT course_id, revision, package_hash, package_json, receipts, built_from,
                        rights, published_ms
                 FROM learn_factory_registry_revisions
                 WHERE course_id = ?1 ORDER BY revision DESC LIMIT 1",
                params![course_id],
                Self::row,
            )
            .optional()
            .map_err(store_err)?;
        raw.map(RawRow::decode).transpose()
    }

    /// The newest revision of every course, for loading into `CourseEngine` at startup.
    pub fn latest_all(&self) -> Result<Vec<PackageRevision>, RegistryError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT r.course_id, r.revision, r.package_hash, r.package_json, r.receipts,
                        r.built_from, r.rights, r.published_ms
                 FROM learn_factory_registry_revisions r
                 JOIN (SELECT course_id, MAX(revision) AS m
                       FROM learn_factory_registry_revisions GROUP BY course_id) l
                   ON r.course_id = l.course_id AND r.revision = l.m
                 ORDER BY r.course_id",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], Self::row)
            .map_err(store_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(store_err)?;
        rows.into_iter().map(RawRow::decode).collect()
    }

    /// Every revision of one course, oldest first (the inspector's history).
    pub fn history(
        &self,
        course_id: &CourseId,
    ) -> Result<Vec<(u64, ContentHash, i64)>, RegistryError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT revision, package_hash, published_ms FROM learn_factory_registry_revisions
                 WHERE course_id = ?1 ORDER BY revision",
            )
            .map_err(store_err)?;
        stmt.query_map(params![course_id], |r| {
            Ok((r.get::<_, i64>(0)? as u64, r.get(1)?, r.get(2)?))
        })
        .map_err(store_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(store_err)
    }
}

struct RawRow {
    course_id: String,
    revision: i64,
    package_hash: String,
    package_json: String,
    receipts: String,
    built_from: String,
    rights: String,
    published_ms: i64,
}

impl RawRow {
    fn decode(self) -> Result<PackageRevision, RegistryError> {
        Ok(PackageRevision {
            course_id: self.course_id,
            revision: self.revision as u64,
            package_hash: self.package_hash,
            package_json: self.package_json,
            receipts: serde_json::from_str(&self.receipts).map_err(store_err)?,
            built_from: serde_json::from_str(&self.built_from).map_err(store_err)?,
            rights: serde_json::from_str(&self.rights).map_err(store_err)?,
            published_ms: self.published_ms,
        })
    }
}

impl PackageRevision {
    /// The engine track for this revision. Fails if the stored JSON no longer parses or
    /// validates (a newer build tightened the rules): the course is then not served.
    pub fn to_track(&self) -> Result<Track, RegistryError> {
        let package: CoursePackage = serde_json::from_str(&self.package_json)
            .map_err(|e| RegistryError::Invalid(e.to_string()))?;
        package.validate().map_err(RegistryError::Invalid)?;
        Ok(Track {
            id: package.id.clone(),
            title: package.title.clone(),
            domain: package.domain,
            summary: package.summary.clone(),
            package: Some(Arc::new(package)),
            generated: Some(self.revision),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(title: &str) -> CoursePackage {
        serde_json::from_value(serde_json::json!({
            "id": "gen-c", "version": "1", "title": title, "domain": "rust", "summary": "s",
            "objectives": [{ "id": "o", "title": "O" }],
            "chapters": [{ "id": "c", "title": "C", "lessons": ["l"] }],
            "lessons": [{ "id": "l", "title": "L", "minutes": 4, "objective_ids": ["o"],
                "steps": [{ "id": "s", "kind": "choice", "family": "f", "title": "T",
                    "objective_ids": ["o"],
                    "options": [{ "id": "a", "text": "A" }, { "id": "b", "text": "B" }],
                    "key": { "choice": ["a"] } }] }],
            "diagnostic": null,
            "reviews": [{ "id": "r", "kind": "choice", "family": "g", "title": "R",
                "objective_ids": ["o"],
                "options": [{ "id": "a", "text": "A" }, { "id": "b", "text": "B" }],
                "key": { "choice": ["b"] } }]
        }))
        .unwrap()
    }

    fn request(p: &CoursePackage) -> PublishRequest<'_> {
        PublishRequest {
            package: p,
            receipts: vec!["rc1".into()],
            built_from: vec![],
            rights: SourceRights::Restricted,
            now_ms: 1,
        }
    }

    #[test]
    fn same_package_twice_is_one_revision() {
        let reg = PackageRegistry::open_in_memory().unwrap();
        let p = package("A");
        assert_eq!(reg.publish(request(&p)).unwrap(), Published::New(1));
        assert_eq!(reg.publish(request(&p)).unwrap(), Published::Unchanged(1));
        assert_eq!(reg.history(&"gen-c".to_string()).unwrap().len(), 1);
    }

    #[test]
    fn changed_package_is_a_new_revision_and_old_ones_stay() {
        let reg = PackageRegistry::open_in_memory().unwrap();
        reg.publish(request(&package("A"))).unwrap();
        assert_eq!(
            reg.publish(request(&package("B"))).unwrap(),
            Published::New(2)
        );
        let latest = reg.latest("gen-c").unwrap().unwrap();
        assert_eq!(latest.revision, 2);
        assert_eq!(latest.rights, SourceRights::Restricted);
        let track = latest.to_track().unwrap();
        assert_eq!(track.title, "B");
        assert_eq!(track.generated, Some(2));
        assert_eq!(reg.latest_all().unwrap().len(), 1);
        assert_eq!(reg.history(&"gen-c".to_string()).unwrap().len(), 2);
    }

    #[test]
    fn invalid_package_is_never_stored() {
        let reg = PackageRegistry::open_in_memory().unwrap();
        let mut p = package("A");
        p.reviews.clear();
        assert!(matches!(
            reg.publish(request(&p)),
            Err(RegistryError::Invalid(_))
        ));
        assert!(reg.latest("gen-c").unwrap().is_none());
    }
}
