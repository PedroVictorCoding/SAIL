//! SAIL Mesh contracts: trusted peer workers that run inference for the host.
//!
//! A worker is another machine (Windows or Linux) that the owner paired with the host. It
//! serves its own llama-server behind an authenticated, certificate-pinned TLS endpoint. The
//! host's Mind decides which requests may run there; everything else stays local.
use serde::{Deserialize, Serialize};

/// Version of the worker hello/status contract. Bumped on incompatible changes.
pub const MESH_VERSION: u16 = 1;

/// What a worker says about itself (`GET /mesh/v1/hello`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeshHello {
    pub version: u16,
    /// Stable random identity created when the worker was initialised.
    pub worker_id: String,
    /// Owner-chosen display name, for example `desktop-4090`.
    pub name: String,
    /// `std::env::consts::OS` / `ARCH` of the worker.
    pub os: String,
    pub arch: String,
    /// How many generations the worker accepts at the same time.
    pub max_parallel: usize,
    /// Generations running on the worker right now.
    pub active: usize,
    /// The model its llama-server has loaded, when that server answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<MeshModel>,
    /// Why the worker's model server is not usable, when it is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_error: Option<String>,
}

/// The model a worker serves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeshModel {
    /// File name of the GGUF (never the full path on the worker).
    pub file_name: String,
    pub context_tokens: usize,
    pub vision: bool,
    pub tools: bool,
}

/// Which work the host sends to workers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MeshMode {
    /// Never use workers.
    Off,
    /// Background work goes to a free worker first; the host runs it when none is free.
    #[default]
    Offload,
    /// Background work uses a worker only while the host's own model is busy.
    Overflow,
}

/// Health of one paired worker as the host last saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshWorkerState {
    /// Not checked yet since the host started or since pairing.
    Unknown,
    /// Reachable, authenticated, and its model server answered.
    Ready,
    /// Reachable, but its model server is down or loading.
    ModelUnavailable,
    /// Not reachable, or the TLS pin/secret did not match.
    Unreachable,
    /// Paired but switched off by the owner.
    Disabled,
}

/// One paired worker in [`MeshStatus`]. Never contains the shared secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshWorkerStatus {
    pub name: String,
    pub address: String,
    pub state: MeshWorkerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<MeshModel>,
    pub max_parallel: usize,
    /// Generations the host is running on this worker now.
    pub active: usize,
    pub completed: u64,
    pub failed: u64,
    /// Round trip of the last health check, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rtt_ms: Option<u64>,
    /// RFC 3339 time of the last successful health check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Mesh section of Mind's status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshStatus {
    pub version: u16,
    pub mode: MeshMode,
    pub workers: Vec<MeshWorkerStatus>,
    /// Requests that ran on a worker.
    pub offloaded: u64,
    /// Requests that were tried on a worker, failed before any output, and ran on the host.
    pub fell_back: u64,
}
