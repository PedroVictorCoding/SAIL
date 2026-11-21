//! The compute tunnel: `ggml-rpc-server` bytes carried inside the Mesh TLS channel.
//!
//! Worker side: `GET /mesh/v1/compute` with `Upgrade: sail-rpc/1` returns 101 and then moves
//! raw bytes to `127.0.0.1:<rpc_port>`. Host side: [`HostTunnel`] listens on loopback and
//! forwards each connection to the worker's tunnel over pinned TLS.
use crate::{Result, registry::WorkerRecord};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::watch;

/// Lending flag shared by the supervisor and the tunnel handlers.
#[derive(Clone)]
pub struct LendGate {
    sender: Arc<watch::Sender<bool>>,
}

impl LendGate {
    pub fn new() -> Self {
        let (sender, _receiver) = watch::channel(false);
        Self {
            sender: Arc::new(sender),
        }
    }

    pub fn set(&self, lending: bool) {
        let _ = self.sender.send(lending);
    }

    pub fn lending(&self) -> bool {
        *self.sender.borrow()
    }

    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.sender.subscribe()
    }
}

impl Default for LendGate {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for LendGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LendGate")
            .field("lending", &self.lending())
            .finish()
    }
}

/// Host side: a loopback listener forwarding every connection to one worker's tunnel.
pub struct HostTunnel {
    address: SocketAddr,
}

impl HostTunnel {
    /// Listens on 127.0.0.1:0.
    pub async fn bind(record: &WorkerRecord) -> Result<HostTunnel> {
        let _ = record;
        Err(crate::MeshError::new(
            "the compute host tunnel is not implemented yet",
        ))
    }

    /// Pass this to llama-server `--rpc`.
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    pub fn stats(&self) -> TunnelStats {
        TunnelStats::default()
    }

    pub async fn close(self) {}
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelStats {
    pub open: u32,
    pub opened_total: u64,
    pub refused_total: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
}
