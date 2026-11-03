//! Measured host admission state. Estimates are reservations, never measurements.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkPriority {
    Voice,
    Interactive,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkKind {
    Inference,
    Model,
    Browser,
    Build,
    Speech,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostSample {
    pub available_ram_bytes: u64,
    pub total_ram_bytes: u64,
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
    pub logical_cpus: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceStatus {
    pub observed: Option<HostSample>,
    pub observation_error: Option<String>,
    pub voice_turns: usize,
    pub admitted_inference: usize,
    pub speech_workers: usize,
    pub ram_headroom_bytes: u64,
    pub max_swap_used_bytes: u64,
    pub resident_heavy_models: usize,
    pub active_browsers: usize,
    pub active_builds: usize,
    pub pending_ram_bytes: u64,
    pub reserved_cpu_slots: usize,
    pub rejected: u64,
    pub preemptions: u64,
}
