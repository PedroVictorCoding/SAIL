//! Lifecycle of the managed pooled model. This adapter owns only its own server and tunnel.
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
    time::{Duration, SystemTime},
};

use sail_common::resources::{self, ResourceScheduler};
use sail_mesh::{
    HELLO_ROUTE, compute::tunnel::HostTunnel, registry::WorkerRecord, tls::pinned_client,
};
use sail_protocol::mesh::{HeavyJob, HeavyState, HeavyStatus, LendState, MeshCompute, MeshHello};
use tokio::{process::Command, time};
use tokio_util::sync::CancellationToken;

use super::{
    admission::admit,
    gguf::{GgufSummary, read_summary},
    placement::{Budgets, Placement, parse_list_devices, plan},
};
use crate::{
    backend::InferenceBackend, config::LocalModelConfig, hardware::HardwareProbe,
    local::LocalGemmaBackend, mesh::MeshPool,
};

pub use crate::config::HeavyConfig;

/// The handoff fixes discovery of lending workers at ten seconds.
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(10);
/// Fixed a priori: a one-second probe and one-second deadline leave time for teardown.
const PROBE_INTERVAL: Duration = Duration::from_secs(1);
/// Fixed by the handoff: update measured throughput every ten seconds while running.
const METRICS_INTERVAL: Duration = Duration::from_secs(10);
/// Fixed a priori: bound each loopback or pinned-worker probe to one second.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// Fixed a priori: allow five seconds for RPC device enumeration to finish.
const DEVICE_TIMEOUT: Duration = Duration::from_secs(5);
/// Fixed a priori in the handoff: reserve 1 GiB on each lending device.
const REMOTE_RESERVE: u64 = 1 << 30;
/// Measured from the configured gpt-oss-120b header: eight KV heads, width 64,
/// two planes and two bytes per f16 element, per block and token.
const GPT_OSS_KV_BYTES_PER_BLOCK_TOKEN: u64 = 8 * 64 * 2 * 2;
/// Fixed a priori: reserve a further 512 MiB for the model's compute buffers.
const COMPUTE_BUFFER_BYTES: u64 = 512 << 20;

/// A running pooled model reachable on the host's loopback interface.
pub struct HeavyEndpoint {
    pub url: String,
    pub model: String,
    pub worker: String,
}

/// Supervise the separate model server that uses a lending PC for expert compute.
pub struct HeavyLane {
    config: HeavyConfig,
    mesh: Arc<MeshPool>,
    scheduler: Arc<ResourceScheduler>,
    status: Mutex<HeavyStatus>,
    summary: Mutex<Option<(PathBuf, SystemTime, Arc<GgufSummary>)>>,
    started: OnceLock<()>,
}

impl HeavyLane {
    /// Create a stopped lane with no inference server or tunnel.
    pub fn new(config: HeavyConfig, mesh: Arc<MeshPool>) -> Arc<Self> {
        Self::with_scheduler(config, mesh, resources::global())
    }

    /// Create the lane with the same scheduler the owning Mind service uses.
    pub fn with_scheduler(
        config: HeavyConfig,
        mesh: Arc<MeshPool>,
        scheduler: Arc<ResourceScheduler>,
    ) -> Arc<Self> {
        let model = config
            .model_path
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Arc::new(Self {
            config,
            mesh,
            scheduler,
            status: Mutex::new(HeavyStatus {
                model,
                state: HeavyState::Stopped {
                    reason: "PC not lending".into(),
                },
                worker: None,
                host_ram_bytes: 0,
                host_vram_bytes: 0,
                remote_bytes: 0,
                tokens_per_second: None,
                job: None,
            }),
            summary: Mutex::new(None),
            started: OnceLock::new(),
        })
    }

    /// Start supervision once. Cancellation stops the owned server and closes its tunnel.
    pub fn spawn(self: &Arc<Self>, lifecycle: CancellationToken) {
        if self.started.set(()).is_err() {
            return;
        }
        let lane = self.clone();
        tokio::spawn(async move { lane.run(lifecycle).await });
    }

    /// Snapshot of the lane for Settings.
    pub fn status(&self) -> HeavyStatus {
        self.snapshot().clone()
    }

    /// Expose the local chat endpoint only once the owned model is ready.
    pub fn endpoint(&self) -> Option<HeavyEndpoint> {
        let status = self.snapshot();
        if status.state != HeavyState::Running {
            return None;
        }
        Some(HeavyEndpoint {
            url: self.url(),
            model: status.model.clone(),
            worker: status.worker.clone()?,
        })
    }

    /// Set or clear the Night Shift job shown in Settings.
    pub fn set_job(&self, job: Option<HeavyJob>) {
        let mut status = self.snapshot();
        status.job = job.filter(|_| status.state == HeavyState::Running);
    }

    fn snapshot(&self) -> MutexGuard<'_, HeavyStatus> {
        self.status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1/chat/completions", self.config.port)
    }

    fn publish(&self, state: HeavyState, worker: Option<&str>, placement: Option<&Placement>) {
        let mut status = self.snapshot();
        status.state = state;
        status.worker = worker.map(str::to_owned);
        status.host_ram_bytes = placement.map_or(0, |p| p.host_ram_bytes);
        status.host_vram_bytes = placement.map_or(0, |p| p.host_vram_bytes);
        status.remote_bytes = placement.map_or(0, |p| p.remote_bytes);
        if status.state != HeavyState::Running {
            status.tokens_per_second = None;
            status.job = None;
        }
    }

    fn stopped(&self, reason: &str) {
        self.publish(
            HeavyState::Stopped {
                reason: reason.into(),
            },
            None,
            None,
        );
    }

    async fn run(&self, lifecycle: CancellationToken) {
        // One failed startup per continuous lending session. Observing a different record
        // or observing the worker stop lending clears the latch.
        let mut failed = None;
        loop {
            tokio::select! {
                biased;
                _ = lifecycle.cancelled() => break,
                _ = self.mesh.refresh() => {}
            }
            match self.mesh.lending_worker() {
                Some((name, record)) if failed.as_ref() != Some(&record) => {
                    if let Err(reason) = self.run_worker(&name, &record, &lifecycle).await {
                        self.publish(HeavyState::Failed { reason }, Some(&name), None);
                        failed = Some(record);
                    }
                }
                Some(_) => {}
                None => {
                    failed = None;
                    self.stopped("PC not lending");
                }
            }
            tokio::select! {
                _ = lifecycle.cancelled() => break,
                _ = time::sleep(DISCOVERY_INTERVAL) => {}
            }
        }
        self.stopped("Mind stopped");
    }

    async fn run_worker(
        &self,
        name: &str,
        record: &WorkerRecord,
        lifecycle: &CancellationToken,
    ) -> Result<(), String> {
        let watch = LendingWatch::new(record)?;
        let compute = tokio::select! {
            biased;
            _ = lifecycle.cancelled() => return Ok(()),
            compute = watch.read() => compute,
        };
        let Some(compute) = compute else {
            self.stopped("PC not lending");
            return Ok(());
        };
        self.publish(HeavyState::Starting, Some(name), None);
        let tunnel = HostTunnel::bind(record)
            .await
            .map_err(|error| error.to_string())?;

        let result = {
            let lost = async {
                loop {
                    time::sleep(PROBE_INTERVAL).await;
                    if tunnel.stats().refused_total > 0 || watch.read().await.is_none() {
                        return;
                    }
                }
            };
            tokio::select! {
                biased;
                _ = lifecycle.cancelled() => Ok(()),
                _ = lost => Ok(()),
                result = self.prepare_and_serve(name, &tunnel, &compute) => result,
            }
        };
        self.stopped("PC not lending");
        // Cancelling prepare_and_serve drops the non-persisting backend and its child.
        tunnel.close().await;
        result
    }

    async fn prepare_and_serve(
        &self,
        worker: &str,
        tunnel: &HostTunnel,
        compute: &MeshCompute,
    ) -> Result<(), String> {
        let address = tunnel.local_addr().to_string();
        let output = time::timeout(
            DEVICE_TIMEOUT,
            Command::new(&self.config.server_binary)
                .args(["--rpc", &address, "--list-devices"])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| "Pooled device enumeration timed out".to_string())?
        .map_err(|error| format!("Could not list pooled devices: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "Pooled device enumeration exited with {}",
                output.status
            ));
        }
        let devices = parse_list_devices(&format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
        let mut remote = Vec::new();
        for (index, cap) in compute.devices.iter().enumerate() {
            let name = format!("RPC{index}");
            let device = devices
                .iter()
                .find(|device| device.name == name)
                .ok_or_else(|| format!("Lending PC did not expose {name}"))?;
            let available = device.free_bytes.saturating_sub(REMOTE_RESERVE);
            let budget = if cap.lend_bytes == 0 {
                available
            } else {
                cap.lend_bytes.min(available)
            };
            remote.push((name, budget));
        }
        if remote.is_empty() {
            return Err("Lending PC did not expose an RPC device".into());
        }
        // The supervisor reports devices in its -d order; preserve their RPC indices
        // while preferring GPU memory over CPU memory.
        let mut preferred: Vec<_> = compute.devices.iter().zip(remote).collect();
        preferred.sort_by_key(|(device, _)| device.name.eq_ignore_ascii_case("CPU"));
        let gpu = devices
            .iter()
            .find(|device| device.name.starts_with("Vulkan"));
        let free_vram = gpu.map_or(0, |device| device.free_bytes);
        let summary = self.model_summary().await?;
        if summary.architecture != "gpt-oss" {
            return Err(format!(
                "No pooled memory budget for GGUF architecture {}",
                summary.architecture
            ));
        }
        let kv_bytes = (self.config.context_tokens as u64)
            .saturating_mul(u64::from(summary.block_count))
            .saturating_mul(GPT_OSS_KV_BYTES_PER_BLOCK_TOKEN)
            .saturating_add(COMPUTE_BUFFER_BYTES);
        let budgets = Budgets {
            host_gpu: gpu
                .filter(|_| self.config.caps.vram_bytes > 0)
                .map(|device| (device.name.clone(), self.config.caps.vram_bytes)),
            host_ram_bytes: self.config.caps.ram_bytes,
            remote: preferred.into_iter().map(|(_, budget)| budget).collect(),
            kv_bytes,
        };
        let placement = plan(&summary, &budgets).map_err(|error| error.to_string())?;
        let (_, free_ram_mb) = HardwareProbe::read_ram_mb();
        admit(free_ram_mb << 20, free_vram, &placement, &self.config.caps)
            .map_err(|error| error.to_string())?;
        self.publish(HeavyState::Starting, Some(worker), Some(&placement));

        let mut extra_args = vec!["--rpc".into(), address, "--metrics".into()];
        extra_args.extend(placement.args.iter().cloned());
        if placement.host_vram_bytes != 0 {
            // KV follows the layer device, not weight overrides. The planner lists
            // the host GPU first, then one RPC device for each remote assignment.
            let remote_count = placement
                .per_device
                .iter()
                .filter(|(device, _)| device.starts_with("RPC"))
                .count();
            let split = format!("1{}", ",0".repeat(remote_count));
            extra_args.extend([
                "--split-mode".into(),
                "layer".into(),
                "--tensor-split".into(),
                split,
            ]);
        }
        let config = LocalModelConfig {
            name: self.snapshot().model.clone(),
            aliases: Vec::new(),
            model_path: self.config.model_path.clone(),
            server_binary: self.config.server_binary.clone(),
            endpoint: self.url(),
            projector: None,
            managed: true,
            persist: false,
            mlock: false,
            no_mmap: false,
            compute: sail_protocol::ComputeBackend::Cpu,
            context_tokens: self.config.context_tokens,
            cpu_moe: crate::config::CpuMoe::Off,
            cache_type_k: "f16".into(),
            cache_type_v: "f16".into(),
            extra_args,
            ..LocalModelConfig::default()
        };
        let backend =
            LocalGemmaBackend::pooled(config, self.scheduler.clone(), placement.host_ram_bytes)
                .map_err(|error| error.message)?;
        let ready = backend.start().await.map_err(|error| error.message)?;
        if !ready.owned_process {
            return Err("Pooled port is already in use".into());
        }
        let metrics_client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(PROBE_TIMEOUT)
            .build()
            .map_err(|error| error.to_string())?;
        let metrics_url = format!("http://127.0.0.1:{}/metrics", self.config.port);
        self.publish(HeavyState::Running, Some(worker), Some(&placement));
        let mut probe = time::interval_at(time::Instant::now() + PROBE_INTERVAL, PROBE_INTERVAL);
        let mut metrics =
            time::interval_at(time::Instant::now() + METRICS_INTERVAL, METRICS_INTERVAL);
        probe.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        metrics.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = probe.tick() => {
                    if backend.status().await.state != sail_protocol::ModelState::Ready {
                        return Err("Pooled llama-server stopped responding".into());
                    }
                }
                _ = metrics.tick() => {
                    let speed = read_throughput(&metrics_client, &metrics_url).await;
                    self.snapshot().tokens_per_second = speed;
                }
            }
        }
    }

    async fn model_summary(&self) -> Result<Arc<GgufSummary>, String> {
        let path = self.config.model_path.clone();
        let modified = tokio::fs::metadata(&path)
            .await
            .and_then(|metadata| metadata.modified())
            .map_err(|error| format!("Could not stat heavy model: {error}"))?;
        if let Some((_, _, summary)) = self
            .summary
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .filter(|(cached, stamp, _)| *cached == path && *stamp == modified)
        {
            return Ok(summary.clone());
        }
        let read_path = path.clone();
        let summary = tokio::task::spawn_blocking(move || read_summary(&read_path))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let summary = Arc::new(summary);
        *self
            .summary
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some((path, modified, summary.clone()));
        Ok(summary)
    }
}

struct LendingWatch {
    client: reqwest::Client,
    hello_url: String,
}

impl LendingWatch {
    fn new(record: &WorkerRecord) -> Result<Self, String> {
        Ok(Self {
            client: pinned_client(&record.fingerprint, &record.secret, PROBE_TIMEOUT)
                .map_err(|error| error.to_string())?,
            hello_url: format!("{}{HELLO_ROUTE}", record.base_url()),
        })
    }

    async fn read(&self) -> Option<MeshCompute> {
        let hello = self
            .client
            .get(&self.hello_url)
            .timeout(PROBE_TIMEOUT)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<MeshHello>()
            .await
            .ok()?;
        hello
            .compute
            .filter(|compute| compute.state == LendState::Lending)
    }
}

async fn read_throughput(client: &reqwest::Client, url: &str) -> Option<f64> {
    let body = client
        .get(url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .text()
        .await
        .ok()?;
    body.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next()? != "llamacpp:predicted_tokens_seconds" {
            return None;
        }
        fields
            .next()?
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value > 0.0)
    })
}
