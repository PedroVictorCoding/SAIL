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
