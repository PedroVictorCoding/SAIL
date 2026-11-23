//! The heavy model lane: Codex fills this. K6 provides the public contract only.
use crate::mesh::MeshPool;
use sail_protocol::mesh::{HeavyJob, HeavyState, HeavyStatus};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::admission::HostCaps;

/// Everything the lane needs to run the second llama-server.
#[derive(Debug, Clone)]
pub struct HeavyConfig {
    /// SAIL_HEAVY_MODEL_PATH (lane disabled when unset).
    pub model_path: PathBuf,
    /// SAIL_HEAVY_SERVER_BINARY, default /srv/sail-data/llama-build-rpc/bin/llama-server.
    pub server_binary: PathBuf,
    /// SAIL_HEAVY_PORT, default 8083.
    pub port: u16,
    /// SAIL_HEAVY_CTX, default 16384.
    pub context_tokens: usize,
    pub caps: HostCaps,
}

impl HeavyConfig {
    /// The lane's configuration, or `None` when no heavy model is configured.
    pub fn from_env() -> Option<Self> {
        None
    }
}

/// Chat-completions endpoint of a running heavy model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeavyEndpoint {
    pub url: String,
    pub model: String,
    pub worker: String,
}

/// The heavy model lane. Codex replaces this stub body with the real supervisor.
pub struct HeavyLane {
    config: HeavyConfig,
    mesh: Arc<MeshPool>,
}

impl HeavyLane {
    pub fn new(config: HeavyConfig, mesh: Arc<MeshPool>) -> Arc<Self> {
        Arc::new(Self { config, mesh })
    }

    /// Start watching the Mesh pool; the task stops with `lifecycle`.
    pub fn spawn(self: &Arc<Self>, lifecycle: CancellationToken) {
        drop(Arc::clone(&self.mesh));
        drop(lifecycle);
    }

    pub fn status(&self) -> HeavyStatus {
        HeavyStatus {
            model: model_stem(&self.config.model_path),
            state: HeavyState::Stopped {
                reason: "the heavy lane is not implemented yet".into(),
            },
            worker: None,
            host_ram_bytes: 0,
            host_vram_bytes: 0,
            remote_bytes: 0,
            tokens_per_second: None,
            job: None,
        }
    }

    /// Some only while the heavy model is running.
    pub fn endpoint(&self) -> Option<HeavyEndpoint> {
        None
    }

    /// Night Shift sets or clears the job line shown in Settings.
    pub fn set_job(&self, job: Option<HeavyJob>) {
        drop(job);
    }
}

fn model_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_owned()
}
