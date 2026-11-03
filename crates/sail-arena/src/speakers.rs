//! Enrolled speaker profiles are opaque ciphertext; no raw audio or plaintext vectors.
use crate::store::{MemoryStore, StoreError, StoreResult, now_ms};
use rusqlite::params;
pub(crate) const SCHEMA:&str="
CREATE TABLE speaker_profiles(id TEXT PRIMARY KEY,sealed BLOB NOT NULL,created_at INTEGER NOT NULL) STRICT;
CREATE TABLE speaker_profile_meta(id INTEGER PRIMARY KEY CHECK(id=1),generation INTEGER NOT NULL) STRICT;
INSERT INTO speaker_profile_meta VALUES(1,0);
";
pub struct SealedSpeakerProfile {
    pub id: String,
    pub sealed: Vec<u8>,
    pub created_at: i64,
}
impl MemoryStore {
    pub fn speaker_profiles(&self) -> StoreResult<(i64, Vec<SealedSpeakerProfile>)> {
        self.with_reader(|conn| {
            conn.execute_batch("BEGIN DEFERRED")?;
            let result = (|| {
                let generation = conn.query_row(
                    "SELECT generation FROM speaker_profile_meta WHERE id=1",
                    [],
                    |r| r.get(0),
                )?;
                let mut stmt = conn.prepare(
                    "SELECT id,sealed,created_at FROM speaker_profiles ORDER BY created_at,id",
                )?;
                let profiles = stmt
                    .query_map([], |r| {
                        Ok(SealedSpeakerProfile {
                            id: r.get(0)?,
                            sealed: r.get(1)?,
                            created_at: r.get(2)?,
                        })
                    })?
                    .collect::<Result<_, _>>()?;
                Ok((generation, profiles))
            })();
            conn.execute_batch("ROLLBACK")?;
            result
        })
    }
    pub fn enroll_speaker(&self, id: &str, sealed: &[u8], generation: i64) -> StoreResult<()> {
        if uuid::Uuid::parse_str(id).is_err() || sealed.is_empty() || sealed.len() > 64_000 {
            return Err(StoreError("Invalid enrolled profile".into()));
        }
        self.with_writer(|conn| {
            let tx = conn.transaction()?;
            let current: i64 = tx.query_row(
                "SELECT generation FROM speaker_profile_meta WHERE id=1",
                [],
                |r| r.get(0),
            )?;
            if current != generation {
                return Err(StoreError(
                    "Speaker profiles changed; submit a fresh enrollment".into(),
                ));
            }
            let count: i64 =
                tx.query_row("SELECT count(*) FROM speaker_profiles", [], |r| r.get(0))?;
            if count >= 64 {
                return Err(StoreError("Speaker profile limit reached".into()));
            }
            tx.execute(
                "INSERT INTO speaker_profiles(id,sealed,created_at) VALUES(?1,?2,?3)",
                params![id, sealed, now_ms()],
            )?;
            tx.commit()?;
            Ok(())
        })
    }
    pub fn forget_speaker(&self, id: &str) -> StoreResult<bool> {
        self.with_writer(|conn| {
            let tx = conn.transaction()?;
            let count = tx.execute("DELETE FROM speaker_profiles WHERE id=?1", [id])?;
            // Even a repeated delete invalidates work started before this owner action.
            tx.execute(
                "UPDATE speaker_profile_meta SET generation=generation+1 WHERE id=1",
                [],
            )?;
            tx.commit()?;
            Ok(count > 0)
        })
    }
}
