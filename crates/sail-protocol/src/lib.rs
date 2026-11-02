//! Versioned, transport-independent contracts for local model inference.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
pub const VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnsupportedVersion,
    InvalidRequest,
    InvalidConfiguration,
    ForbiddenEndpoint,
    UnknownModel,
    UnsupportedCapability,
    ResourceLimit,
    Busy,
    QueueTimeout,
    Cancelled,
    DeadlineExceeded,
    BackendUnavailable,
    BackendProtocol,
    ConsumerClosed,
    ShuttingDown,
}
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct InferenceError {
    pub version: u16,
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}
impl InferenceError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            version: VERSION,
            code,
            message: message.into(),
            retryable: matches!(
                code,
                ErrorCode::Busy
                    | ErrorCode::QueueTimeout
                    | ErrorCode::BackendUnavailable
                    | ErrorCode::DeadlineExceeded
            ),
        }
    }
}
pub type Result<T> = std::result::Result<T, InferenceError>;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}
impl Content {
    pub fn text(&self) -> Option<&str> {
        if let Self::Text(s) = self {
            Some(s)
        } else {
            None
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageUrl {
    pub url: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub role: Role,
    pub content: Content,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: Content::Text(text.into()),
            tool_calls: vec![],
            tool_call_id: None,
            name: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionDefinition {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub parameters: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionDefinition,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generation {
    pub max_tokens: u32,
    pub temperature: f32,
}
impl Default for Generation {
    fn default() -> Self {
        Self {
            max_tokens: 384,
            temperature: 0.4,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceRequest {
    pub version: u16,
    pub request_id: Uuid,
    #[serde(default)]
    pub model: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(default)]
    pub tool_choice: ToolChoice,
    #[serde(default)]
    pub generation: Generation,
}
impl InferenceRequest {
    pub fn text(prompt: impl Into<String>) -> Self {
        Self {
            version: VERSION,
            request_id: Uuid::new_v4(),
            model: None,
            messages: vec![Message::text(Role::User, prompt)],
            tools: vec![],
            tool_choice: ToolChoice::None,
            generation: Generation::default(),
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Timings {
    pub queue_ms: u64,
    pub first_token_ms: Option<u64>,
    pub inference_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceResponse {
    pub version: u16,
    pub request_id: Uuid,
    pub model: String,
    pub message: Message,
    pub finish_reason: String,
    pub usage: Usage,
    pub timings: Timings,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceEvent {
    pub version: u16,
    pub request_id: Uuid,
    #[serde(flatten)]
    pub payload: InferenceEventPayload,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InferenceEventPayload {
    Started { model: String },
    TextDelta { text: String },
    ToolDelta { index: usize, delta: Value },
    Completed { response: InferenceResponse },
    Failed { error: InferenceError },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeBackend {
    Cpu,
    Vulkan,
    #[serde(alias = "gpu+cpu", alias = "gpu_cpu")]
    Hybrid,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub model: String,
    pub text: bool,
    pub vision: bool,
    pub tools: bool,
    pub context_tokens: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelState {
    Uninitialized,
    Starting,
    Ready,
    Unavailable,
    Stopped,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatus {
    pub version: u16,
    pub state: ModelState,
    pub model: String,
    pub capabilities: Option<ModelCapabilities>,
    pub requested_backend: ComputeBackend,
    pub observed_backend: Option<ComputeBackend>,
    pub owned_process: bool,
    pub process_id: Option<u32>,
    pub last_error: Option<InferenceError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<InferenceProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware: Option<HardwareDiagnostics>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InferenceMetrics {
    pub submitted: u64,
    pub completed: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub rejected: u64,
    pub active: usize,
    pub queued: usize,
    pub total_inference_ms: u64,
    pub last_first_token_ms: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MindStatus {
    pub version: u16,
    pub accepting_requests: bool,
    pub models: Vec<ModelStatus>,
    pub metrics: InferenceMetrics,
    pub limits: ResourceLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware: Option<HardwareDiagnostics>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferenceProfile {
    pub profile_id: String,
    pub backend: ComputeBackend,
    pub threads: usize,
    pub threads_batch: usize,
    pub context_tokens: usize,
    pub cache_type_k: String,
    pub cache_type_v: String,
    pub flash_attn: bool,
    pub cache_reuse: usize,
    pub batch_size: usize,
    pub ubatch_size: usize,
    pub mlock: bool,
    pub no_mmap: bool,
    pub gpu_layers: u32,
    pub device: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareDiagnostics {
    pub cpu_cores: usize,
    pub cpu_threads: usize,
    pub total_ram_mb: u64,
    pub free_ram_mb: u64,
    pub gpu_detected: bool,
    pub gpu_name: Option<String>,
    pub render_device_accessible: bool,
    pub permission_remediation: Option<String>,
    pub supported_devices: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkRecord {
    pub scenario: String,
    pub prompt_tokens: usize,
    pub output_tokens: usize,
    pub ttft_ms: u64,
    pub prompt_eval_tokens_per_sec: f32,
    pub decode_tokens_per_sec: f32,
    pub peak_rss_mb: u64,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkSummary {
    pub model: String,
    pub profile: InferenceProfile,
    pub records: Vec<BenchmarkRecord>,
    pub p50_ttft_ms: u64,
    pub p95_ttft_ms: u64,
    pub avg_decode_tok_per_sec: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StagedSpeechPhase {
    ToolPlanned,
    AckStarted,
    ToolStarted,
    ToolCompleted,
    GroundedAnswerStarted,
    SpeechFinished,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedSpeechEvent {
    pub turn_id: u64,
    pub phase: StagedSpeechPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    pub max_parallel: usize,
    pub max_queued: usize,
    pub max_input_bytes: usize,
    pub max_response_bytes: usize,
    pub max_images: usize,
    pub max_image_bytes: usize,
    pub max_output_tokens: u32,
    pub queue_timeout_ms: u64,
    pub inference_timeout_ms: u64,
}
impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_parallel: 1,
            max_queued: 8,
            max_input_bytes: 128 * 1024,
            max_response_bytes: 128 * 1024,
            max_images: 2,
            max_image_bytes: 4_000_000,
            max_output_tokens: 1024,
            queue_timeout_ms: 30_000,
            inference_timeout_ms: 180_000,
        }
    }
}

pub mod execution;

pub mod memory;
