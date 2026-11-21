//! The compute supervisor: one tick every 2 s turns the lending decision into a running
//! `ggml-rpc-server`, the tunnel gate and `status.json`.
use crate::Result;
use crate::compute::ComputeHandle;
use crate::compute::tunnel::LendGate;
use sail_protocol::mesh::{ComputeDevice, LendState, LendWindow, MeshBusy, MeshCompute};
use std::path::{Path, PathBuf};

/// Everything the supervisor needs to run `ggml-rpc-server` on this PC.
pub struct ComputeConfig {
    pub rpc_server: PathBuf,
    pub cache_dir: PathBuf,
    /// Devices passed as `-d`, e.g. ["Vulkan0", "CPU"].
    pub devices: Vec<String>,
    pub threads: usize,
    pub window: LendWindow,
    pub lend_vram_bytes: u64,
    pub lend_ram_bytes: u64,
    /// Loopback port for ggml-rpc-server. Fixed a priori: 50052 (llama.cpp default).
    pub rpc_port: u16,
}

/// Supervises the RPC server and publishes the worker's compute state.
pub struct ComputeSupervisor {
    config: ComputeConfig,
    handle: ComputeHandle,
    state: LendState,
    rpc_error: Option<String>,
    build: Option<String>,
    hibernator: Option<crate::hibernate::Hibernator>,
}

impl ComputeSupervisor {
    pub fn new(
        config: ComputeConfig,
        data_dir: &Path,
        gate: LendGate,
        hibernator: Option<crate::hibernate::Hibernator>,
    ) -> Self {
        let devices = described_devices(&config);
        let handle = ComputeHandle::new(
            gate,
            config.rpc_port,
            data_dir,
            config.window.clone(),
            devices,
        );
        Self {
            config,
            handle,
            state: LendState::Loading,
            rpc_error: None,
            build: None,
            hibernator,
        }
    }

    /// One tick (every 2 s): reload lend.json, call `decide`, start/stop the RPC server,
    /// hibernate before starting / resume after stopping, set the gate, write status.json.
    pub async fn tick(&mut self, busy: Option<&MeshBusy>) -> LendState {
        let _ = busy;
        self.state = LendState::Unavailable {
            reason: "the compute supervisor is not implemented yet".into(),
        };
        self.state.clone()
    }

    pub fn snapshot(&self) -> MeshCompute {
        self.handle.snapshot()
    }

    /// Set and save the mode (CLI and `POST /mesh/v1/lend`).
    pub fn set_mode(&mut self, mode: sail_protocol::mesh::LendMode) -> Result<()> {
        let _ = mode;
        Err(crate::MeshError::new(
            "setting the compute mode is not implemented yet",
        ))
    }

    /// Stop the RPC server and resume every hibernated app. Idempotent.
    pub async fn release(&mut self) {
        let _ = &self.hibernator;
    }

    /// The handle shared with the worker's HTTP layer.
    pub fn handle(&self) -> ComputeHandle {
        self.handle.clone()
    }
}

/// The devices the RPC server should expose, with the owner's per-device cap.
fn described_devices(config: &ComputeConfig) -> Vec<ComputeDevice> {
    config
        .devices
        .iter()
        .map(|name| ComputeDevice {
            name: name.clone(),
            description: name.clone(),
            lend_bytes: if name.eq_ignore_ascii_case("cpu") {
                config.lend_ram_bytes
            } else {
                config.lend_vram_bytes
            },
        })
        .collect()
}
