//! Versioned knowledge retrieval contracts. Access comes from Guardian, never the prompt.
use serde::{Deserialize, Serialize};
pub const VERSION: u32 = 1;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryQuery {
    pub version: u32,
    pub text: String,
    pub limit: usize,
    pub token_budget: usize,
    pub expand_graph: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHit {
    pub claim_id: i64,
    pub entity_id: i64,
    pub entity_name: String,
    pub predicate: String,
    pub value: String,
    pub status: String,
    pub scope: String,
    pub observed_at: i64,
    pub score: f64,
    pub evidence_ids: Vec<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryReply {
    pub version: u32,
    pub hits: Vec<MemoryHit>,
    /// Untrusted data, never a system instruction. Budget excludes the surrounding prompt.
    pub context: String,
    pub generation: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Embedding {
    pub model: String,
    /// Immutable artifact hash or deployment revision; vectors never mix revisions.
    pub revision: String,
    pub values: Vec<f32>,
}
impl Embedding {
    pub fn valid(&self) -> bool {
        !self.model.trim().is_empty()
            && !self.revision.trim().is_empty()
            && !self.values.is_empty()
            && self.values.len() <= 8192
            && self.values.iter().all(|v| v.is_finite())
            && self.values.iter().any(|v| *v != 0.0)
    }
}

/// Why memory is being read. Guardian maps a purpose to a [`MemoryGrant`]; a model or a
/// prompt never chooses its own scopes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "purpose", rename_all = "snake_case")]
pub enum MemoryPurpose {
    /// The owner's own voice/text conversation.
    Dialogue,
    /// A Forge coding task for one repository (`owner/name`).
    Forge { repo: String },
    /// Background consolidation and retention.
    Maintenance,
}

/// Highest sensitivity a grant may read. Ordered: `Normal < Private < Sensitive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySensitivity {
    Normal,
    Private,
    Sensitive,
}

/// Memory visibility issued by Guardian for one purpose. An empty scope list sees nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryGrant {
    pub version: u32,
    pub purpose: MemoryPurpose,
    pub scopes: Vec<String>,
    pub max_sensitivity: MemorySensitivity,
}
