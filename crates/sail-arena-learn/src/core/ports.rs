//! Ports: what the core needs from storage and from a model, as traits. Adapters live in
//! `store` (SQLite) and `agent` (Mind); tests supply in-memory or scripted ones.
use super::events::{Envelope, NewEvent};
use super::graph::{
    GraphNodePosition, GraphProposition, GraphReasoningChain, GraphRenderSettings, GraphSnapshot,
};
use sail_protocol::{InferenceRequest, InferenceResponse};
use serde::Serialize;
use std::collections::BTreeMap;

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

    /// Reads the persistent Knowledge Graph snapshot.
    fn graph_snapshot(&self) -> Result<GraphSnapshot, StoreError> {
        Ok(GraphSnapshot::default())
    }

    /// Upserts propositions and reasoning chains into the persistent Knowledge Graph.
    fn upsert_graph_items(
        &self,
        _propositions: &[GraphProposition],
        _chains: &[GraphReasoningChain],
    ) -> Result<(), StoreError> {
        Ok(())
    }

    /// Deletes propositions by name/id and any chains that reference them.
    fn delete_graph_nodes(&self, _names_or_ids: &[String]) -> Result<usize, StoreError> {
        Ok(0)
    }

    /// Deletes all propositions/chains whose sole provenance comes from `source_id`.
    fn delete_graph_source(&self, _source_id: &str) -> Result<usize, StoreError> {
        Ok(0)
    }

    /// Saves 2D canvas positions for Knowledge Graph nodes.
    fn save_graph_positions(
        &self,
        _positions: &BTreeMap<String, GraphNodePosition>,
    ) -> Result<(), StoreError> {
        Ok(())
    }

    /// Saves visual render settings for the Knowledge Graph canvas.
    fn save_graph_settings(&self, _settings: &GraphRenderSettings) -> Result<(), StoreError> {
        Ok(())
    }
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

/// One proposition extracted from a session or an imported note.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeItem {
    pub statement: String,
    /// Verbatim text from the source. Ungrounded propositions are rejected.
    pub quote: String,
}

/// A chain over indexes in the same extraction batch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeChain {
    pub conclusion: usize,
    pub premises: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeHit {
    pub id: i64,
    pub statement: String,
    pub topic: String,
    pub quote: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeSource {
    pub id: i64,
    pub name: String,
    pub topic: String,
}

/// Arena's learning graph, as the agent layer needs it. Implemented in `agent::knowledge`
/// over `sail-arena`; tests use an in-memory double.
pub trait KnowledgeGraph: Send + Sync {
    /// Gated, bounded retrieval (#251). The caller labels results untrusted.
    fn search(
        &self,
        topic: Option<&str>,
        query: &str,
        limit: usize,
        budget_bytes: usize,
    ) -> Result<Vec<KnowledgeHit>, String>;
    /// Deterministic merge of one extraction batch. Returns how many propositions were
    /// stored or merged. Chains refer to indexes in `items`.
    fn store_extraction(
        &self,
        topic: &str,
        session: Option<&str>,
        source_id: Option<i64>,
        items: &[KnowledgeItem],
        chains: &[KnowledgeChain],
    ) -> Result<usize, String>;
    fn covered_through(&self, session: &str) -> Result<i64, String>;
    fn set_covered_through(&self, session: &str, seq: i64) -> Result<(), String>;
    fn add_source(&self, topic: &str, name: &str, content: &str)
    -> Result<KnowledgeSource, String>;
    fn delete_source(&self, id: i64) -> Result<(), String>;
    fn sources(&self, topic: Option<&str>) -> Result<Vec<KnowledgeSource>, String>;
    /// Number of active propositions, for honest import/extraction progress.
    fn proposition_count(&self, topic: Option<&str>) -> Result<usize, String>;
}
