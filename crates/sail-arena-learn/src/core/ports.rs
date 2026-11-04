//! Ports: what the core needs from storage and from a model, as traits. Adapters live in
//! `store` (SQLite) and `agent` (Mind); tests supply in-memory or scripted ones.
use super::events::{Envelope, NewEvent};
use sail_protocol::{InferenceRequest, InferenceResponse};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// Another writer appended first; reload and retry.
    #[error("session {session} changed concurrently (expected seq {expected}, found {found})")]
    Conflict {
        session: String,
        expected: u64,
        found: u64,
    },
    #[error("stored data has schema version {found}; this build reads up to {supported}")]
    NewerVersion { found: u16, supported: u16 },
    #[error("session {0} not found")]
    NotFound(String),
    #[error("storage failed: {0}")]
    Backend(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub goal: String,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// An append-only, per-session event log.
pub trait EventStore: Send + Sync {
    /// Creates a session; fails if the id exists.
    fn create(&self, session: &str, goal: &str, at_ms: i64) -> Result<Envelope, StoreError>;
    /// Appends events atomically after `expected_seq` (the caller's last seen seq). Events
    /// whose idempotency key is already stored are skipped. Returns the newly stored events.
    fn append(
        &self,
        session: &str,
        expected_seq: u64,
        at_ms: i64,
        events: &[NewEvent],
    ) -> Result<Vec<Envelope>, StoreError>;
    fn events(&self, session: &str) -> Result<Vec<Envelope>, StoreError>;
    fn sessions(&self, limit: usize) -> Result<Vec<SessionSummary>, StoreError>;
    /// Deletes the session and all of its events.
    fn delete(&self, session: &str) -> Result<bool, StoreError>;
}

/// One chat completion with tools. The Mind adapter maps errors to text; nothing is
/// fabricated on failure.
#[async_trait::async_trait]
pub trait Model: Send + Sync {
    async fn complete(
        &self,
        request: InferenceRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<InferenceResponse, String>;
}
