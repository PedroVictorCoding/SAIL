//! Pooled compute: lend this PC's ggml devices to a SAIL host over the Mesh tunnel.
//!
//! The worker runs `ggml-rpc-server` (a bare ggml device, no model) on loopback and carries
//! its RPC traffic to the host through the same pinned-TLS, bearer-authenticated Mesh
//! channel as inference requests. [`lend`] decides when the PC lends, [`tunnel`] carries
//! the bytes, and [`supervisor`] starts and stops the RPC server and keeps `status.json`.
pub mod lend;
pub mod supervisor;
pub mod tunnel;

use crate::{Result, write_private};
use sail_protocol::mesh::{LendMode, LendState, LendWindow, MeshCompute};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicU32, Ordering},
    },
};
use tunnel::LendGate;

/// Worker-side handle shared by the HTTP layer and the compute supervisor.
///
/// The HTTP layer reads the latest published [`MeshCompute`] for `MeshHello.compute` and
/// writes `lend.json` through [`ComputeHandle::set_mode`]; the supervisor publishes each
/// tick. Cloning is cheap and shares one snapshot.
#[derive(Clone)]
pub struct ComputeHandle {
    gate: LendGate,
    rpc_port: u16,
    data_dir: PathBuf,
    tunnels: Arc<AtomicU32>,
    snapshot: Arc<RwLock<MeshCompute>>,
}

impl ComputeHandle {
    /// Build a handle for a worker started with `--compute`, publishing an initial
    /// `Loading` snapshot (the supervisor replaces it every tick).
    pub fn new(
        gate: LendGate,
        rpc_port: u16,
        data_dir: &Path,
        window: LendWindow,
        devices: Vec<sail_protocol::mesh::ComputeDevice>,
    ) -> Self {
        let snapshot = MeshCompute {
            mode: LendMode::Auto,
            state: LendState::Loading,
            window,
            devices,
            build: None,
            hibernated: Vec::new(),
            tunnels: 0,
        };
        Self {
            gate,
            rpc_port,
            data_dir: data_dir.to_path_buf(),
            tunnels: Arc::new(AtomicU32::new(0)),
            snapshot: Arc::new(RwLock::new(snapshot)),
        }
    }

    /// The gate the supervisor opens and the tunnel checks.
    pub fn gate(&self) -> &LendGate {
        &self.gate
    }

    /// Loopback port of `ggml-rpc-server`.
    pub fn rpc_port(&self) -> u16 {
        self.rpc_port
    }

    /// The worker's data folder (holds `lend.json` and `status.json`).
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Latest published snapshot, with the live tunnel count.
    pub fn snapshot(&self) -> MeshCompute {
        let mut snapshot = self
            .snapshot
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        snapshot.tunnels = self.tunnels.load(Ordering::SeqCst);
        snapshot
    }

    /// Save a new lend mode and return the updated snapshot.
    pub fn set_mode(&self, mode: LendMode) -> Result<MeshCompute> {
        lend::save_mode(&self.data_dir.join(lend::LEND_FILE), &mode)?;
        {
            let mut snapshot = self
                .snapshot
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            snapshot.mode = mode;
        }
        Ok(self.snapshot())
    }

    /// Replace the published snapshot (the supervisor calls this every tick).
    pub(crate) fn publish(&self, snapshot: MeshCompute) {
        *self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = snapshot;
    }

    /// One tunnel opened; the status snapshot counts it.
    pub(crate) fn tunnel_opened(&self) {
        self.tunnels.fetch_add(1, Ordering::SeqCst);
    }

    /// One tunnel closed (including a failed upgrade after it was counted).
    pub(crate) fn tunnel_closed(&self) {
        self.tunnels.fetch_sub(1, Ordering::SeqCst);
    }

    /// Persist the status file the `status` command reads.
    pub(crate) fn write_status(&self, at: &str) -> Result<()> {
        let status = StatusFile {
            at: at.to_owned(),
            compute: self.snapshot(),
        };
        let data = serde_json::to_vec(&status)
            .map_err(|e| crate::MeshError::new(format!("Cannot encode status: {e}")))?;
        write_private(&self.data_dir.join(STATUS_FILE), &data)
            .map_err(|e| crate::MeshError::new(format!("Cannot write status: {e}")))
    }
}

/// File the `status` command reads; written by the supervisor each tick.
pub(crate) const STATUS_FILE: &str = "status.json";

/// Shape of `status.json`: the snapshot plus when it was written.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct StatusFile {
    /// RFC3339 UTC.
    pub at: String,
    pub compute: MeshCompute,
}
