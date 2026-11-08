//! Flow v1: bounded text DAGs. All ports carry UTF-8 text; condition nodes also
//! emit a boolean routing decision. No code, credentials, connectors or inference.
use serde::{Deserialize, Serialize};
use uuid::Uuid;
pub const VERSION: u16 = 1;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub version: u16,
    pub id: Uuid,
    pub revision: u32,
    pub name: String,
    pub nodes: Vec<Node>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    pub source: Option<String>,
    #[serde(default)]
    pub branch: Branch,
    pub operation: Operation,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Branch {
    #[default]
    Always,
    True,
    False,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Trigger {},
    Trim {},
    Uppercase {},
    Prefix { text: String },
    Contains { text: String },
    Output {},
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    Paused,
    Completed,
    Cancelled,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeReceipt {
    pub node_id: String,
    pub skipped: bool,
    pub output: String,
    pub decision: Option<bool>,
    pub completed_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: Uuid,
    pub workflow: Workflow,
    pub input: String,
    pub state: RunState,
    pub receipts: Vec<NodeReceipt>,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveWorkflow {
    pub expected_revision: u32,
    pub workflow: Workflow,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartRun {
    pub id: Uuid,
    pub workflow_id: Uuid,
    pub revision: u32,
    pub input: String,
}

/// Lightweight status for the global shell; excludes workflow inputs and receipts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub version: u16,
    pub workflows: usize,
    pub running: usize,
    pub paused: usize,
}
