//! Version 1 execution protocol. No host paths, mount controls or arbitrary environment.
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandLimits {
    pub wall_ms: u64,
    pub cpu_seconds: u64,
    pub memory_bytes: u64,
    pub pids: u32,
    pub fds: u32,
    pub scratch_bytes: u64,
    pub output_bytes: usize,
}
impl Default for CommandLimits {
    fn default() -> Self {
        Self {
            wall_ms: 30_000,
            cpu_seconds: 10,
            memory_bytes: 512 * 1024 * 1024,
            pids: 32,
            fds: 64,
            scratch_bytes: 32 * 1024 * 1024,
            output_bytes: 64 * 1024,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandPlan {
    pub version: u32,
    pub request_id: Uuid,
    pub correlation_id: Uuid,
    pub executable: String,
    pub argv: Vec<String>,
    pub limits: CommandLimits,
}
impl CommandPlan {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("unsupported command protocol".into());
        }
        if !self.executable.starts_with('/')
            || self.executable.contains('\0')
            || self.executable.len() > 4096
        {
            return Err("executable must be an absolute sandbox path".into());
        }
        if self.argv.len() > 128
            || self.argv.iter().any(|arg| arg.contains('\0'))
            || self.argv.iter().map(String::len).sum::<usize>() > 32 * 1024
        {
            return Err("argument limit".into());
        }
        let l = self.limits;
        if l.wall_ms == 0
            || l.wall_ms > 120_000
            || l.cpu_seconds == 0
            || l.cpu_seconds > 60
            || !(16 * 1024 * 1024..=1024 * 1024 * 1024).contains(&l.memory_bytes)
            || l.pids < 2
            || l.pids > 64
            || l.fds < 8
            || l.fds > 128
            || l.scratch_bytes == 0
            || l.scratch_bytes > 64 * 1024 * 1024
            || l.output_bytes < 2
            || l.output_bytes > 256 * 1024
        {
            return Err("command budget outside service ceilings".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    OutputLimit,
    Unavailable,
    Denied,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub version: u32,
    pub request_id: Uuid,
    pub correlation_id: Uuid,
    pub status: CommandStatus,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub output_truncated: bool,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolEffect {
    ReadOnly,
    Mutating,
    Sandbox,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDescriptor {
    pub id: String,
    pub version: u32,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub security_effect: ToolEffect,
    pub required_capabilities: Vec<String>,
    pub timeout_ms: u64,
    pub resource_class: String,
    pub concurrency_limit: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolState {
    Queued,
    PolicyPending,
    Executing,
    Completed,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolEvent {
    pub version: u32,
    pub request_id: Uuid,
    pub correlation_id: Uuid,
    pub sequence: u64,
    pub tool: String,
    pub state: ToolState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_session: Option<Uuid>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolObservation {
    pub version: u32,
    pub id: Uuid,
    pub request_id: Uuid,
    pub correlation_id: Uuid,
    pub source: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub success: bool,
    pub data: serde_json::Value,
    pub evidence_refs: Vec<String>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolStep {
    pub request_id: Uuid,
    pub tool: String,
    pub input: serde_json::Value,
    pub depends_on: Vec<Uuid>,
    pub approval_token: Option<Uuid>,
    pub foreground: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Requested,
    Executing,
    Completed,
    Failed,
    Cancelled,
}

impl From<ToolState> for ToolStatus {
    fn from(state: ToolState) -> Self {
        match state {
            ToolState::Queued | ToolState::PolicyPending => ToolStatus::Requested,
            ToolState::Executing => ToolStatus::Executing,
            ToolState::Completed => ToolStatus::Completed,
            ToolState::Failed => ToolStatus::Failed,
            ToolState::Cancelled => ToolStatus::Cancelled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub id: String,
    pub artifact_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolPayload {
    CameraCapture {
        source: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        thumbnail_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        artifact_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        question: Option<String>,
    },
    Visualization {
        viz_type: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        total_count: usize,
    },
    Document {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes: Option<usize>,
        operation: String,
    },
    ExecutionResult {
        summary: String,
        success: bool,
        output: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    InteractiveControls {
        title: String,
        summary: String,
        controls: Vec<InteractiveControl>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractiveControl {
    pub id: String,
    pub label: String,
    pub action: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultMessage {
    pub message_id: String,
    pub conversation_id: String,
    pub invocation_id: String,
    pub tool_name: String,
    pub status: ToolStatus,
    pub payload: ToolPayload,
    pub artifact_refs: Vec<ArtifactRef>,
    pub created_at: i64,
    pub updated_at: i64,
}

