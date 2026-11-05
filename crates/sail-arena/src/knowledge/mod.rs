//! Structured long-term knowledge: entities, evidence-linked claims, procedures and project
//! state, stored in Arena's SQLite file next to (but separate from) the conversation archive.
//!
//! Long-term memory is this graph, not transcripts. Transcript records in
//! [`MemoryStore`](crate::store::MemoryStore) are temporary evidence; claims keep a content
//! hash and a bounded receipt of their sources, so a transcript can be released after its
//! knowledge is committed without losing why the knowledge is believed.
//!
//! Invariants the store enforces (not just documents):
//! - every claim has evidence, and its epistemic status must be supported by that evidence
//!   (`verified` needs a tool observation or artifact);
//! - restatements (`prior_memory`) share their source's origin and never add independence;
//! - project work cannot reach `implemented`/`tested`/`deployed` without direct evidence;
//! - an older observation cannot supersede a newer one;
//! - deleting writes a tombstone, so stale jobs cannot recreate forgotten knowledge, and
//!   bumps a deletion generation that in-flight jobs can be checked against;
//! - reads are filtered by [`Access`] in SQL, before rows are hydrated.

pub mod changeset;
pub mod consolidation;
pub mod jobs;
pub mod learning;
pub mod model;
mod read;
pub mod retrieval;
pub(crate) mod schema;
mod write;

pub use model::*;
pub use write::KnowledgeTx;

use crate::store::{MemoryStore, StoreError};

#[derive(Debug, Clone, PartialEq)]
pub enum KnowledgeError {
    /// The request is malformed or breaks a knowledge invariant.
    Invalid(String),
    NotFound(&'static str, i64),
    /// The requested status or state is stronger than the evidence supports.
    InsufficientEvidence(String),
    /// The content was deleted; only an explicit user action may teach it again.
    Tombstoned,
    /// A deletion committed after the caller started; its work may contain forgotten data.
    StaleGeneration {
        expected: i64,
        current: i64,
    },
    Storage(String),
}

impl std::fmt::Display for KnowledgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) => write!(f, "invalid knowledge change: {m}"),
            Self::NotFound(what, id) => write!(f, "{what} {id} not found"),
            Self::InsufficientEvidence(m) => write!(f, "insufficient evidence: {m}"),
            Self::Tombstoned => write!(f, "this knowledge was deleted and cannot be recreated"),
            Self::StaleGeneration { expected, current } => write!(
                f,
                "memory changed since this job started (deletion generation {expected} → {current})"
            ),
            Self::Storage(m) => write!(f, "knowledge storage: {m}"),
        }
    }
}

impl std::error::Error for KnowledgeError {}

impl From<rusqlite::Error> for KnowledgeError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(e.to_string())
    }
}

impl From<StoreError> for KnowledgeError {
    fn from(e: StoreError) -> Self {
        Self::Storage(e.0)
    }
}

pub type KnowledgeResult<T> = std::result::Result<T, KnowledgeError>;

/// Handle to the knowledge tables of one [`MemoryStore`].
#[derive(Clone, Copy)]
pub struct KnowledgeStore<'a> {
    store: &'a MemoryStore,
}

impl MemoryStore {
    pub fn knowledge(&self) -> KnowledgeStore<'_> {
        KnowledgeStore { store: self }
    }
}

impl<'a> KnowledgeStore<'a> {
    /// Run `f` in one write transaction. Any error rolls back every change made by `f`.
    pub fn write<T>(
        &self,
        actor: Actor,
        f: impl FnOnce(&mut KnowledgeTx<'_>) -> KnowledgeResult<T>,
    ) -> KnowledgeResult<T> {
        self.write_checked(None, actor, f)
    }

    /// Like [`write`](Self::write), but fails with `StaleGeneration` if a deletion committed
    /// since the caller read `expected` from [`deletion_generation`](Self::deletion_generation).
    /// Background jobs use this so work prepared before a deletion cannot land after it.
    pub fn write_at_generation<T>(
        &self,
        expected: i64,
        actor: Actor,
        f: impl FnOnce(&mut KnowledgeTx<'_>) -> KnowledgeResult<T>,
    ) -> KnowledgeResult<T> {
        self.write_checked(Some(expected), actor, f)
    }

    fn write_checked<T>(
        &self,
        expected: Option<i64>,
        actor: Actor,
        f: impl FnOnce(&mut KnowledgeTx<'_>) -> KnowledgeResult<T>,
    ) -> KnowledgeResult<T> {
        self.store
            .with_writer(|conn| {
                let outcome = (|| {
                    // IMMEDIATE takes the write lock up front, so the generation check below
                    // cannot race a concurrent deletion.
                    let tx =
                        conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                    if let Some(expected) = expected {
                        let current = write::generation(&tx)?;
                        if current != expected {
                            return Err(KnowledgeError::StaleGeneration { expected, current });
                        }
                    }
                    let value = {
                        let mut ktx = KnowledgeTx::new(&tx, actor);
                        f(&mut ktx)?
                    };
                    tx.commit()?;
                    Ok(value)
                })();
                Ok(outcome)
            })
            .map_err(KnowledgeError::from)?
    }

    pub(crate) fn read<T>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> KnowledgeResult<T>,
    ) -> KnowledgeResult<T> {
        self.store
            .with_reader(|conn| Ok(f(conn)))
            .map_err(KnowledgeError::from)?
    }

    /// Counter bumped by every deletion.
    pub fn deletion_generation(&self) -> KnowledgeResult<i64> {
        self.read(write::generation)
    }
}
