//! Pure tensor placement across the host GPU/RAM and the lending PC's RPC devices.
use crate::heavy::gguf::GgufSummary;

/// One device `llama-server --list-devices` reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedDevice {
    pub name: String,
    pub description: String,
    pub free_bytes: u64,
}

/// Parse `llama-server --list-devices`; unknown lines are skipped.
pub fn parse_list_devices(_stdout: &str) -> Vec<ListedDevice> {
    Vec::new()
}

/// What the owner and the worker allow, and the host's own share.
pub struct Budgets {
    /// Host GPU device name (e.g. "Vulkan0") and its cap; cap 0 = host GPU off.
    pub host_gpu: Option<(String, u64)>,
    pub host_ram_bytes: u64,
    /// Remote devices in preference order (PC GPU first, then PC CPU), name as listed, with their budget.
    pub remote: Vec<(String, u64)>,
    /// KV cache + compute buffers reserved on the device holding attention.
    pub kv_bytes: u64,
}

/// Where the planner put everything, plus the llama-server arguments.
pub struct Placement {
    pub args: Vec<String>,
    pub host_ram_bytes: u64,
    pub host_vram_bytes: u64,
    pub remote_bytes: u64,
    pub per_device: Vec<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("the model needs {needed} bytes but only {available} are available")]
    DoesNotFit { needed: u64, available: u64 },
    #[error("the model has no expert tensors")]
    NoExperts,
}

/// Plan where every tensor of `model` goes.
pub fn plan(_model: &GgufSummary, _budgets: &Budgets) -> Result<Placement, PlanError> {
    Err(PlanError::NoExperts)
}
