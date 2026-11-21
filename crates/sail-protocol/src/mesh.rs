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
    /// Set while the worker is resting because its owner is using the PC. Older workers
    /// never send it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy: Option<MeshBusy>,
    /// Present only when the worker was started with `--compute`: how it lends its compute
    /// to the host. Older workers never send it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compute: Option<MeshCompute>,
}

/// Why a worker refuses new work for now (`MeshHello::busy`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MeshBusy {
    /// Plain-language reason, for example `gaming (full-screen game)` or
    /// `other programs use 72% CPU`.
    pub reason: String,
    /// The worker stepped aside completely (gaming, making music): its model server is
    /// stopped to free the GPU, CPU and memory, and it comes back on its own afterwards.
    pub full: bool,
    /// Seconds until the worker checks again whether it can take work, while backing off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
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
    /// Reachable, but resting while its owner uses the PC (see `MeshWorkerStatus::busy`).
    Busy,
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
    /// Why the worker is resting, when its state is `busy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy: Option<MeshBusy>,
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

/// How the owner set lending. serde, internally tagged:
/// `{"mode":"auto"}` | `{"mode":"play","until":"<RFC3339 UTC>"}` |
/// `{"mode":"play","until":null}` | `{"mode":"pause"}`.
/// `play` with `until: null` lends until the owner says otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LendMode {
    Auto,
    Play { until: Option<String> },
    Pause,
}

/// What the worker is doing with its compute now. serde, internally tagged:
/// `{"state":"lending"}` | `{"state":"loading"}` | `{"state":"paused"}` |
/// `{"state":"outside_window","next_start":"<RFC3339 UTC>"}` |
/// `{"state":"resting","reason":"REAPER is open","full":true}` |
/// `{"state":"unavailable","reason":"ggml-rpc-server exited (code 1)"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LendState {
    /// RPC server up and the tunnel accepting.
    Lending,
    /// Decided to lend; RPC server starting or hibernation in progress.
    Loading,
    /// Owner said pause.
    Paused,
    /// Auto mode, outside the lending window.
    OutsideWindow { next_start: String },
    /// Rest rules say step aside (game, music app, busy PC). `full` as in `MeshBusy`.
    Resting { reason: String, full: bool },
    /// Compute is configured but cannot run (binary missing, crashed, wrong build).
    Unavailable { reason: String },
}

/// Local daily window in the worker's own time zone, "HH:MM". `start > end` wraps midnight
/// (01:00–08:00 does not wrap; 22:00–07:00 does). `start == end` means always.
/// Owner decision: 01:00–08:00.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LendWindow {
    pub start: String,
    pub end: String,
}

/// One ggml device the RPC server exposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeDevice {
    /// "Vulkan0" | "CPU" …
    pub name: String,
    /// Human description, e.g. "AMD Radeon RX 7800 XT".
    pub description: String,
    /// Bytes the owner allows the host to use on this device (`--lend-vram-gib`/`--lend-ram-gib`).
    pub lend_bytes: u64,
}

/// One hibernated program, for display. Never includes command lines or window titles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HibernatedApp {
    /// Executable file name, e.g. "chrome.exe".
    pub name: String,
    pub processes: u32,
    /// Working-set bytes released when trimmed (0 on Unix).
    pub released_bytes: u64,
    /// RFC3339 UTC.
    pub since: String,
}

/// `MeshHello.compute`: present only when the worker was started with `--compute`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshCompute {
    pub mode: LendMode,
    pub state: LendState,
    pub window: LendWindow,
    pub devices: Vec<ComputeDevice>,
    /// llama.cpp build of the RPC server, e.g. "b12014". The host refuses a mismatch.
    pub build: Option<String>,
    pub hibernated: Vec<HibernatedApp>,
    /// Connections open through the tunnel now.
    pub tunnels: u32,
}

/// Body of `POST /mesh/v1/lend` (host → worker).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LendRequest {
    pub mode: LendMode,
}

/// Route of the compute tunnel (HTTP Upgrade `sail-rpc/1`).
pub const COMPUTE_ROUTE: &str = "/mesh/v1/compute";
/// Route the host uses to set the lend mode.
pub const LEND_ROUTE: &str = "/mesh/v1/lend";
/// Upgrade protocol token.
pub const COMPUTE_UPGRADE: &str = "sail-rpc/1";
