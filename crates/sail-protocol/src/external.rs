//! Metadata-only contracts for external inference. Never persist prompt or reply text here.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Reserved,
    Dispatched,
    Completed,
    RateLimited,
    Failed,
    Cancelled,
    /// The provider may have accepted the request; automatic replay is forbidden.
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressReceipt {
    pub version: u16,
    pub request_id: Uuid,
    pub provider: String,
    pub model: String,
    pub purpose: String,
    pub request_sha256: String,
    pub input_bytes: usize,
    pub redactions: usize,
    pub reserved_at: i64,
    pub finished_at: Option<i64>,
    pub outcome: AttemptOutcome,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}
