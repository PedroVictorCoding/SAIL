//! SAIL Mesh on the host: which paired workers are healthy, and which requests may run on
//! them instead of the host's own model.
//!
//! Policy (see `docs/SAIL_MESH.md`):
//! - Only workers the owner paired (`sail-core mesh pair`) are used. Nothing is discovered.
//! - Voice never leaves the host. Background work goes to workers by default; interactive
//!   work only when `SAIL_MESH_PRIORITIES` includes it.
//! - A worker failure before any output reaches the caller falls back to the host model.
//!   After output has streamed, the error is returned as is: Mind never splices two models'
//!   answers together or invents a success.
use crate::{config::LocalModelConfig, local::LocalGemmaBackend};
use sail_mesh::{HELLO_ROUTE, Registry, WorkerRecord, tls::pinned_client};
use sail_protocol::{
    Content, ContentPart, ErrorCode, InferenceError, InferenceRequest, ToolChoice,
    mesh::{
        LEND_ROUTE, LendMode, LendRequest, LendState, MESH_VERSION, MeshCompute, MeshHello,
        MeshMode, MeshStatus, MeshWorkerState, MeshWorkerStatus,
    },
    resources::WorkPriority,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};
use tokio_util::sync::CancellationToken;

/// How often the host checks every worker (and re-reads the pairing file).
pub const HEALTH_INTERVAL: Duration = Duration::from_secs(15);
/// After a failed request, how long a worker is skipped before it is tried again.
const FAILURE_BACKOFF: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// `<data root>/mesh/workers.json`: the owner's paired workers (secrets, mode 0600).
pub fn default_registry_path() -> PathBuf {
    Registry::path_in(&sail_common::storage::data_path("mesh"))
}

fn parse_mode(value: &str) -> Option<MeshMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" | "0" | "false" | "none" => Some(MeshMode::Off),
        "" | "offload" | "on" | "1" | "true" => Some(MeshMode::Offload),
        "overflow" => Some(MeshMode::Overflow),
        _ => None,
    }
}

fn parse_priorities(value: &str) -> Vec<WorkPriority> {
    value
        .split(',')
        .filter_map(|p| match p.trim().to_ascii_lowercase().as_str() {
            "background" => Some(WorkPriority::Background),
            "interactive" => Some(WorkPriority::Interactive),
            // Voice is never offloaded: a LAN hop and a sleeping PC are not acceptable
            // failure modes for a live conversation.
            _ => None,
        })
        .collect()
}

/// Host-side settings.
#[derive(Debug, Clone)]
pub struct MeshSettings {
    pub mode: MeshMode,
    pub priorities: Vec<WorkPriority>,
    pub registry_path: PathBuf,
}

impl MeshSettings {
    /// `SAIL_MESH` (`offload` default, `overflow`, `off`) and `SAIL_MESH_PRIORITIES`
    /// (`background` default; add `interactive` to offload chat turns too).
    pub fn from_env() -> Self {
        let mode = std::env::var("SAIL_MESH")
            .ok()
            .map(|v| {
                parse_mode(&v).unwrap_or_else(|| {
                    tracing::warn!(value = %v, "Unknown SAIL_MESH value; Mesh is off");
                    MeshMode::Off
                })
            })
            .unwrap_or_default();
        let priorities = std::env::var("SAIL_MESH_PRIORITIES")
            .map(|v| parse_priorities(&v))
            .unwrap_or_else(|_| vec![WorkPriority::Background]);
        Self {
            mode,
            priorities,
            registry_path: default_registry_path(),
        }
    }
}

struct WorkerState {
    status: MeshWorkerState,
    hello: Option<MeshHello>,
    /// The compute snapshot from the last hello or lend request.
    compute: Option<MeshCompute>,
    backend: Option<Arc<LocalGemmaBackend>>,
    /// Model file the backend was built for.
    backend_model: Option<String>,
    active: usize,
    completed: u64,
    failed: u64,
    last_rtt_ms: Option<u64>,
    last_seen: Option<String>,
    last_error: Option<String>,
    skip_until: Option<Instant>,
}

struct RemoteWorker {
    record: WorkerRecord,
    client: Result<reqwest::Client, String>,
    state: Mutex<WorkerState>,
}

impl RemoteWorker {
    fn new(record: WorkerRecord) -> Self {
        let client = pinned_client(&record.fingerprint, &record.secret, CONNECT_TIMEOUT)
            .map_err(|e| e.to_string());
        let status = if record.enabled {
            MeshWorkerState::Unknown
        } else {
            MeshWorkerState::Disabled
        };
        Self {
            record,
            client,
            state: Mutex::new(WorkerState {
                status,
                hello: None,
                compute: None,
                backend: None,
                backend_model: None,
                active: 0,
                completed: 0,
                failed: 0,
                last_rtt_ms: None,
                last_seen: None,
                last_error: None,
                skip_until: None,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WorkerState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Ask the worker who it is and whether its model is up; rebuild the inference client
    /// when the worker's model changed.
    async fn check(&self, base: &LocalModelConfig) {
        if !self.record.enabled {
            self.lock().status = MeshWorkerState::Disabled;
            return;
        }
        let started = Instant::now();
        let result = hello(&self.record, self.client.as_ref().map_err(Clone::clone)).await;
        let rtt = started.elapsed().as_millis() as u64;
        let mut state = self.lock();
        match result {
            Ok(hello) => {
                state.last_rtt_ms = Some(rtt);
                state.last_seen = Some(chrono::Utc::now().to_rfc3339());
                state.compute = hello.compute.clone();
                if hello.busy.is_some() {
                    // Its owner is using the PC (gaming, music, or just busy): leave it
                    // alone. The model may even be stopped; keep the backend for later.
                    state.status = MeshWorkerState::Busy;
                    state.hello = Some(hello);
                    return;
                }
                match &hello.model {
                    Some(model) => {
                        if state.backend_model.as_deref() != Some(&model.file_name) {
                            match self.backend_for(base, model) {
                                Ok(backend) => {
                                    state.backend = Some(Arc::new(backend));
                                    state.backend_model = Some(model.file_name.clone());
                                }
                                Err(e) => {
                                    state.backend = None;
                                    state.backend_model = None;
                                    state.last_error = Some(e.message);
                                }
                            }
                        }
                        if state.backend.is_some() {
                            state.status = MeshWorkerState::Ready;
                            state.last_error = None;
                        } else {
                            state.status = MeshWorkerState::ModelUnavailable;
                        }
                    }
                    None => {
                        state.status = MeshWorkerState::ModelUnavailable;
                        state.last_error = hello.upstream_error.clone();
                    }
                }
                state.hello = Some(hello);
            }
            Err(e) => {
                state.status = MeshWorkerState::Unreachable;
                state.last_error = Some(e);
            }
        }
    }

    fn backend_for(
        &self,
        base: &LocalModelConfig,
        model: &sail_protocol::mesh::MeshModel,
    ) -> sail_protocol::Result<LocalGemmaBackend> {
        let client = self
            .client
            .clone()
            .map_err(|e| InferenceError::new(ErrorCode::InvalidConfiguration, e))?;
        let mut config = base.clone();
        let stem = model.file_name.trim_end_matches(".gguf");
        config.name = format!("{stem}@{}", self.record.name);
        config.aliases = vec![];
        config.model_path = PathBuf::from(&model.file_name);
        config.projector = None;
        // The worker reports its context; keep it inside the bounds Mind accepts.
        config.context_tokens = if model.context_tokens == 0 {
            base.context_tokens
        } else {
            model.context_tokens.clamp(512, 262_144)
        };
        config.probe_timeout = HELLO_TIMEOUT;
        LocalGemmaBackend::remote(config, client, &self.record.base_url())
    }

    fn status(&self) -> MeshWorkerStatus {
        let state = self.lock();
        let hello = state.hello.as_ref();
        MeshWorkerStatus {
            name: self.record.name.clone(),
            address: self.record.address.clone(),
            state: state.status,
            os: hello.map(|h| format!("{} {}", h.os, h.arch)),
            model: hello.and_then(|h| h.model.clone()),
            max_parallel: hello.map_or(1, |h| h.max_parallel),
            active: state.active,
            completed: state.completed,
            failed: state.failed,
            last_rtt_ms: state.last_rtt_ms,
            last_seen: state.last_seen.clone(),
            last_error: state.last_error.clone(),
            busy: (state.status == MeshWorkerState::Busy)
                .then(|| hello.and_then(|h| h.busy.clone()))
                .flatten(),
            compute: state
                .compute
                .clone()
                .or_else(|| hello.and_then(|h| h.compute.clone())),
        }
    }
}

/// `GET /mesh/v1/hello` over the pinned client.
async fn hello(
    record: &WorkerRecord,
    client: Result<&reqwest::Client, String>,
) -> Result<MeshHello, String> {
    let client = client?;
    let url = format!("{}{HELLO_ROUTE}", record.base_url());
    let response = client
        .get(url)
        .timeout(HELLO_TIMEOUT)
        .send()
        .await
        .map_err(|e| describe_transport(&e))?;
    match response.status().as_u16() {
        200 => {}
        401 => {
            return Err(
                "the worker rejected the pairing secret (it was re-initialised or rotated; pair again)"
                    .into(),
            );
        }
        status => return Err(format!("worker answered HTTP {status}")),
    }
    let hello: MeshHello = response
        .json()
        .await
        .map_err(|e| format!("malformed hello: {e}"))?;
    if hello.version != MESH_VERSION {
        return Err(format!(
            "worker speaks Mesh v{}, this host v{MESH_VERSION}; update the older side",
            hello.version
        ));
    }
    Ok(hello)
}

fn describe_transport(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        text = format!("{text}: {inner}");
        source = inner.source();
    }
    if text.contains("does not match the paired fingerprint") {
        "the worker's certificate does not match the one paired (a different machine, or it was re-initialised)".into()
    } else if error.is_connect() || error.is_timeout() {
        "not reachable (is the worker running, on the same network, and allowed through its firewall?)".into()
    } else {
        text
    }
}

/// A worker slot reserved for one request. Dropping it releases the slot.
pub struct MeshLease {
    worker: Arc<RemoteWorker>,
    backend: Arc<LocalGemmaBackend>,
    released: bool,
}

impl MeshLease {
    pub fn backend(&self) -> &LocalGemmaBackend {
        &self.backend
    }
    pub fn worker_name(&self) -> &str {
        &self.worker.record.name
    }
    fn release(&mut self, failure: Option<&InferenceError>) {
        if self.released {
            return;
        }
        self.released = true;
        let mut state = self.worker.lock();
        state.active = state.active.saturating_sub(1);
        match failure {
            None => state.completed += 1,
            Some(error)
                if error.code == ErrorCode::Cancelled
                    || error.code == ErrorCode::ConsumerClosed => {}
            // The worker started resting between its last health check and this request
            // (it answers 429). That is not a failure: skip it until the next check.
            Some(error)
                if error.code == ErrorCode::BackendUnavailable
                    && (error.message.contains("429")
                        || error.message.contains("worker is resting")) =>
            {
                state.skip_until = Some(Instant::now() + FAILURE_BACKOFF);
            }
            Some(error) => {
                state.failed += 1;
                state.last_error = Some(error.message.clone());
                if error.code == ErrorCode::BackendUnavailable
                    || error.code == ErrorCode::BackendProtocol
                {
                    // Skip it until the backoff ends; the health check decides when it is
                    // really back.
                    state.skip_until = Some(Instant::now() + FAILURE_BACKOFF);
                }
            }
        }
    }
    pub fn succeeded(mut self) {
        self.release(None);
    }
    pub fn failed(mut self, error: &InferenceError) {
        self.release(Some(error));
    }
}
impl Drop for MeshLease {
    fn drop(&mut self) {
        self.release(Some(&InferenceError::new(ErrorCode::Cancelled, "dropped")));
    }
}

/// Should a worker failure be retried on the host? Only failures that say nothing about
/// the request itself, and only while nothing has been delivered to the caller.
pub fn falls_back(error: &InferenceError) -> bool {
    matches!(
        error.code,
        ErrorCode::BackendUnavailable
            | ErrorCode::BackendProtocol
            | ErrorCode::Busy
            | ErrorCode::QueueTimeout
            | ErrorCode::UnknownModel
            | ErrorCode::UnsupportedCapability
            | ErrorCode::ResourceLimit
    )
}

/// The host's paired workers and its offload policy.
pub struct MeshPool {
    settings: MeshSettings,
    base: LocalModelConfig,
    workers: Mutex<Vec<Arc<RemoteWorker>>>,
    registry_stamp: Mutex<Option<Option<SystemTime>>>,
    offloaded: AtomicU64,
    fell_back: AtomicU64,
    monitoring: AtomicBool,
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl MeshPool {
    /// `base` is the host's model config: remote backends copy its sampling-independent
    /// settings and replace the model name, file and context with the worker's.
    pub fn new(settings: MeshSettings, base: LocalModelConfig) -> Arc<Self> {
        let pool = Arc::new(Self {
            settings,
            base,
            workers: Mutex::new(vec![]),
            registry_stamp: Mutex::new(None),
            offloaded: AtomicU64::new(0),
            fell_back: AtomicU64::new(0),
            monitoring: AtomicBool::new(false),
        });
        pool.reload_if_changed();
        pool
    }

    pub fn mode(&self) -> MeshMode {
        self.settings.mode
    }

    fn workers(&self) -> Vec<Arc<RemoteWorker>> {
        self.workers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Re-read the pairing file when it changed. Unchanged records keep their state.
    fn reload_if_changed(&self) {
        let path = &self.settings.registry_path;
        let stamp = modified(path);
        {
            let mut last = self
                .registry_stamp
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if *last == Some(stamp) {
                return;
            }
            *last = Some(stamp);
        }
        let registry = match Registry::load(path) {
            Ok(registry) => registry,
            Err(error) => {
                tracing::error!(%error, "SAIL Mesh pairing file unreadable; keeping the workers already loaded");
                return;
            }
        };
        let mut workers = self.workers.lock().unwrap_or_else(|p| p.into_inner());
        let old = std::mem::take(&mut *workers);
        *workers = registry
            .workers
            .into_iter()
            .map(|record| {
                old.iter()
                    .find(|w| w.record == record)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(RemoteWorker::new(record)))
            })
            .collect();
        tracing::info!(workers = workers.len(), "SAIL Mesh workers loaded");
    }

    /// Check every worker now.
    pub async fn refresh(&self) {
        self.reload_if_changed();
        let checks = self.workers().into_iter().map(|worker| {
            let base = &self.base;
            async move { worker.check(base).await }
        });
        futures_util::future::join_all(checks).await;
    }

    /// Start the periodic health check (once per pool). It stops with `lifecycle`.
    pub fn spawn_monitor(self: &Arc<Self>, lifecycle: CancellationToken) {
        if self.settings.mode == MeshMode::Off
            || self.monitoring.swap(true, Ordering::SeqCst)
            || tokio::runtime::Handle::try_current().is_err()
        {
            return;
        }
        let pool = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let Some(strong) = pool.upgrade() else { return };
                strong.refresh().await;
                drop(strong);
                tokio::select! {
                    _ = lifecycle.cancelled() => return,
                    _ = tokio::time::sleep(HEALTH_INTERVAL) => {}
                }
            }
        });
    }

    /// Reserve a worker for `request`, which the host resolved to its model `model`
    /// (`aliases` are that model's other names). `None` means run it on the host.
    pub fn acquire(
        &self,
        model: &str,
        aliases: &[String],
        request: &InferenceRequest,
        priority: WorkPriority,
        host_busy: bool,
    ) -> Option<MeshLease> {
        match self.settings.mode {
            MeshMode::Off => return None,
            MeshMode::Overflow if !host_busy => return None,
            _ => {}
        }
        if !self.settings.priorities.contains(&priority) {
            return None;
        }
        let images = request
            .messages
            .iter()
            .filter_map(|m| match &m.content {
                Content::Parts(parts) => Some(parts),
                Content::Text(_) => None,
            })
            .flatten()
            .any(|p| matches!(p, ContentPart::ImageUrl { .. }));
        let tools = !request.tools.is_empty() && request.tool_choice != ToolChoice::None;
        let now = Instant::now();
        let mut best: Option<(Arc<RemoteWorker>, (usize, u64))> = None;
        for worker in self.workers() {
            if !worker.record.enabled || !worker.record.serves_model(model, aliases) {
                continue;
            }
            let state = worker.lock();
            let Some(hello) = state.hello.as_ref() else {
                continue;
            };
            let Some(remote_model) = hello.model.as_ref() else {
                continue;
            };
            if state.status != MeshWorkerState::Ready
                || state.backend.is_none()
                || state.skip_until.is_some_and(|t| t > now)
                || state.active >= hello.max_parallel.max(1)
                || (images && !remote_model.vision)
                || (tools && !remote_model.tools)
            {
                continue;
            }
            // Least loaded first, then the fastest to answer.
            let rank = (
                state.active * 1000 / hello.max_parallel.max(1),
                state.last_rtt_ms.unwrap_or(u64::MAX),
            );
            drop(state);
            if best.as_ref().is_none_or(|(_, r)| rank < *r) {
                best = Some((worker, rank));
            }
        }
        let (worker, _) = best?;
        let backend = {
            let mut state = worker.lock();
            let backend = state.backend.clone()?;
            state.active += 1;
            backend
        };
        Some(MeshLease {
            worker,
            backend,
            released: false,
        })
    }

    /// POST a `LendRequest` to the worker and return its status with the new compute snapshot.
    ///
    /// An unknown name is `NotFound`; a worker without `--compute` is `NoCompute`; a
    /// transport failure, a 401 or a malformed reply is `Unverified`.
    pub async fn set_lend(
        &self,
        name: &str,
        mode: LendMode,
    ) -> Result<MeshWorkerStatus, MeshOpError> {
        let worker = self
            .workers()
            .into_iter()
            .find(|worker| worker.record.name == name)
            .ok_or_else(|| MeshOpError::NotFound(format!("no worker named {name}")))?;
        if !worker.record.enabled {
            return Err(MeshOpError::NoCompute(format!(
                "worker {name} is switched off"
            )));
        }
        let client = worker
            .client
            .as_ref()
            .map_err(|error| MeshOpError::Unverified(error.clone()))?;
        let response = client
            .post(format!("{}{LEND_ROUTE}", worker.record.base_url()))
            .timeout(HELLO_TIMEOUT)
            .json(&LendRequest { mode })
            .send()
            .await
            .map_err(|error| MeshOpError::Unverified(describe_transport(&error)))?;
        match response.status().as_u16() {
            200 => {
                let compute: MeshCompute = response.json().await.map_err(|error| {
                    MeshOpError::Unverified(format!("malformed compute status: {error}"))
                })?;
                let mut state = worker.lock();
                state.compute = Some(compute);
                drop(state);
                Ok(worker.status())
            }
            404 | 409 => Err(MeshOpError::NoCompute(format!(
                "worker {name} does not lend compute"
            ))),
            401 => Err(MeshOpError::Unverified(
                "the worker rejected the pairing secret (it was re-initialised or rotated; pair again)"
                    .into(),
            )),
            status => Err(MeshOpError::Unverified(format!(
                "worker answered HTTP {status}"
            ))),
        }
    }

    /// The first enabled, reachable worker whose last hello says `LendState::Lending`, with
    /// the pairing record [`sail_mesh::compute::HostTunnel::bind`] needs.
    pub fn lending_worker(&self) -> Option<(String, WorkerRecord)> {
        self.workers().into_iter().find_map(|worker| {
            if !worker.record.enabled {
                return None;
            }
            let state = worker.lock();
            if state.status == MeshWorkerState::Unreachable {
                return None;
            }
            let lending = state
                .hello
                .as_ref()
                .and_then(|hello| hello.compute.as_ref())
                .is_some_and(|compute| compute.state == LendState::Lending);
            lending.then(|| (worker.record.name.clone(), worker.record.clone()))
        })
    }

    pub fn record_offloaded(&self) {
        self.offloaded.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_fallback(&self) {
        self.fell_back.fetch_add(1, Ordering::Relaxed);
    }

    pub fn status(&self) -> MeshStatus {
        MeshStatus {
            version: MESH_VERSION,
            mode: self.settings.mode,
            workers: self.workers().iter().map(|w| w.status()).collect(),
            offloaded: self.offloaded.load(Ordering::Relaxed),
            fell_back: self.fell_back.load(Ordering::Relaxed),
            heavy: None,
        }
    }

    /// A worker that can take a whole Night Shift coding job right now (round 27): enabled,
    /// `Ready` (so not resting for a game or music), not backing off, below its parallel limit
    /// and serving a model. Least loaded first, then the fastest to answer. `None` when Mesh is
    /// off or background work isn't offloaded (`SAIL_MESH_PRIORITIES`).
    ///
    /// Forge sends its own OpenAI-style requests, so this returns the worker's pinned client
    /// (certificate pin + bearer secret) instead of a lease on a [`LocalGemmaBackend`].
    pub fn night_worker(&self) -> Option<NightWorker> {
        if self.settings.mode == MeshMode::Off
            || !self.settings.priorities.contains(&WorkPriority::Background)
        {
            return None;
        }
        let now = Instant::now();
        let mut best: Option<(NightWorker, (usize, u64))> = None;
        for worker in self.workers() {
            if !worker.record.enabled {
                continue;
            }
            let Ok(client) = worker.client.as_ref() else {
                continue;
            };
            let state = worker.lock();
            let Some(hello) = state.hello.as_ref() else {
                continue;
            };
            let Some(model) = hello.model.as_ref() else {
                continue;
            };
            if state.status != MeshWorkerState::Ready
                || state.skip_until.is_some_and(|t| t > now)
                || state.active >= hello.max_parallel.max(1)
            {
                continue;
            }
            let rank = (
                state.active * 1000 / hello.max_parallel.max(1),
                state.last_rtt_ms.unwrap_or(u64::MAX),
            );
            let candidate = NightWorker {
                name: worker.record.name.clone(),
                url: format!("{}/v1/chat/completions", worker.record.base_url()),
                model: model.file_name.clone(),
                client: client.clone(),
            };
            drop(state);
            if best.as_ref().is_none_or(|(_, r)| rank < *r) {
                best = Some((candidate, rank));
            }
        }
        best.map(|(worker, _)| worker)
    }
}

/// A paired worker chosen for a Night Shift job: where to send chat completions and the
/// pinned client to send them with. `Debug` never prints the client (it carries the secret).
#[derive(Clone)]
pub struct NightWorker {
    pub name: String,
    /// `https://host:port/v1/chat/completions` on the worker.
    pub url: String,
    /// GGUF file name the worker serves.
    pub model: String,
    pub client: reqwest::Client,
}

impl std::fmt::Debug for NightWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NightWorker")
            .field("name", &self.name)
            .field("url", &self.url)
            .field("model", &self.model)
            .field("client", &"<pinned>")
            .finish()
    }
}

/// Why an owner operation on the pairing file failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshOpError {
    /// Malformed pairing code or argument (HTTP 400).
    BadInput(String),
    /// No paired worker by that name (HTTP 404).
    NotFound(String),
    /// The code is well formed but no running worker proved it (HTTP 502).
    Unverified(String),
    /// The worker was started without `--compute` (HTTP 409 `no_compute`).
    NoCompute(String),
    /// The pairing file could not be read or written (HTTP 500).
    Storage(String),
}
impl std::fmt::Display for MeshOpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadInput(m)
            | Self::NotFound(m)
            | Self::Unverified(m)
            | Self::NoCompute(m)
            | Self::Storage(m) => f.write_str(m),
        }
    }
}

fn load_registry(path: &Path) -> Result<Registry, MeshOpError> {
    Registry::load(path).map_err(|e| MeshOpError::Storage(e.to_string()))
}

/// Pair (or re-pair) the worker in `code`. The worker must be running: its certificate pin
/// and secret are both proved before anything is saved. Returns the saved record, what the
/// worker said, and whether an earlier pairing was replaced.
pub async fn pair_at(
    path: &Path,
    code: &str,
    serves: Vec<String>,
) -> Result<(WorkerRecord, MeshHello, bool), MeshOpError> {
    let code =
        sail_mesh::PairingCode::parse(code).map_err(|e| MeshOpError::BadInput(e.to_string()))?;
    let serves: Vec<String> = serves
        .into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    if serves.len() > 16 || serves.iter().any(|s| s.len() > 128) {
        return Err(MeshOpError::BadInput(
            "serves takes at most 16 model names of up to 128 characters".into(),
        ));
    }
    let record = WorkerRecord::from_code(&code, serves, chrono::Utc::now().to_rfc3339());
    let client = pinned_client(&record.fingerprint, &record.secret, CONNECT_TIMEOUT)
        .map_err(|e| MeshOpError::BadInput(e.to_string()))?;
    let hello = hello(&record, Ok(&client)).await.map_err(|e| {
        MeshOpError::Unverified(format!("could not verify worker {}: {e}", record.address))
    })?;
    let mut registry = load_registry(path)?;
    let replaced = registry.upsert(record.clone());
    registry
        .save(path)
        .map_err(|e| MeshOpError::Storage(e.to_string()))?;
    Ok((record, hello, replaced))
}

/// Switch a paired worker on or off without forgetting it.
pub fn set_enabled_at(path: &Path, name: &str, enabled: bool) -> Result<(), MeshOpError> {
    let mut registry = load_registry(path)?;
    registry
        .get_mut(name)
        .ok_or_else(|| MeshOpError::NotFound(format!("no worker named {name}")))?
        .enabled = enabled;
    registry
        .save(path)
        .map_err(|e| MeshOpError::Storage(e.to_string()))
}

/// Forget a worker and its secret.
pub fn remove_at(path: &Path, name: &str) -> Result<(), MeshOpError> {
    let mut registry = load_registry(path)?;
    if !registry.remove(name) {
        return Err(MeshOpError::NotFound(format!("no worker named {name}")));
    }
    registry
        .save(path)
        .map_err(|e| MeshOpError::Storage(e.to_string()))
}

impl MeshPool {
    /// The pairing file this pool reads.
    pub fn registry_path(&self) -> &Path {
        &self.settings.registry_path
    }
    /// Status of one worker after checking all of them now.
    pub async fn refreshed_worker(&self, name: &str) -> Option<MeshWorkerStatus> {
        self.refresh().await;
        self.workers()
            .iter()
            .find(|w| w.record.name == name)
            .map(|w| w.status())
    }
}

/// `sail-core mesh …`: the owner's pairing commands. Returns the process exit status.
pub mod cli {
    use super::*;

    const USAGE: &str = "\
sail-core mesh — share inference with paired worker PCs

  sail-core mesh pair '<sailmesh://… code>' [--serves NAME[,NAME…]]
  sail-core mesh list
  sail-core mesh discover [--seconds N]
  sail-core mesh test <name> [--prompt TEXT]
  sail-core mesh enable <name> | disable <name> | remove <name>

Workers run `sail-mesh-worker serve`, which prints the pairing code.
--serves limits a worker to host model names/aliases (default: any).
Policy: SAIL_MESH=offload|overflow|off, SAIL_MESH_PRIORITIES=background[,interactive].
A running SAIL picks up changes within 15 seconds.";

    fn take(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, String> {
        let Some(i) = args.iter().position(|a| a == flag) else {
            return Ok(None);
        };
        if i + 1 >= args.len() {
            return Err(format!("{flag} needs a value"));
        }
        let value = args.remove(i + 1);
        args.remove(i);
        Ok(Some(value))
    }

    fn one_name(args: &[String]) -> Result<&str, String> {
        match args {
            [name] => Ok(name),
            _ => Err(format!("expected one worker name\n\n{USAGE}")),
        }
    }

    pub async fn run(mut args: Vec<String>) -> i32 {
        let path = default_registry_path();
        match run_at(&path, &mut args).await {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("error: {error}");
                1
            }
        }
    }

    pub async fn run_at(path: &Path, args: &mut Vec<String>) -> Result<(), String> {
        let command = if args.is_empty() {
            "help".to_owned()
        } else {
            args.remove(0)
        };
        let registry = Registry::load(path).map_err(|e| e.to_string())?;
        match command.as_str() {
            "pair" => {
                let serves = take(args, "--serves")?
                    .map(|s| s.split(',').map(str::to_owned).collect())
                    .unwrap_or_default();
                let (record, hello, replaced) = pair_at(path, one_name(args)?, serves)
                    .await
                    .map_err(|e| e.to_string())?;
                println!(
                    "{} worker \"{}\" at {} ({} {}).",
                    if replaced { "Re-paired" } else { "Paired" },
                    record.name,
                    record.address,
                    hello.os,
                    hello.arch
                );
                match hello.model {
                    Some(m) => println!(
                        "Model: {} ({} context tokens{}{}), {} at a time.",
                        m.file_name,
                        m.context_tokens,
                        if m.tools { ", tools" } else { "" },
                        if m.vision { ", vision" } else { "" },
                        hello.max_parallel
                    ),
                    None => println!(
                        "Its model server is not ready yet: {}",
                        hello.upstream_error.unwrap_or_default()
                    ),
                }
                println!(
                    "Saved to {}. A running SAIL starts using it within 15 seconds.",
                    path.display()
                );
                Ok(())
            }
            "list" => {
                if registry.workers.is_empty() {
                    println!(
                        "No workers paired. Run `sail-mesh-worker serve` on a PC, then `sail-core mesh pair '<code>'`."
                    );
                    return Ok(());
                }
                for record in &registry.workers {
                    let worker = RemoteWorker::new(record.clone());
                    worker.check(&LocalModelConfig::default()).await;
                    let s = worker.status();
                    let model = s
                        .model
                        .as_ref()
                        .map_or("-".to_owned(), |m| m.file_name.clone());
                    println!(
                        "{:<16} {:<22} {:<18} {:<40} {}",
                        s.name,
                        s.address,
                        format!("{:?}", s.state).to_lowercase(),
                        model,
                        s.busy
                            .map(|b| format!("resting: {}", b.reason))
                            .or(s.last_error)
                            .unwrap_or_default()
                    );
                }
                Ok(())
            }
            "discover" => {
                let seconds: u64 = take(args, "--seconds")?
                    .map(|s| {
                        s.parse()
                            .map_err(|_| format!("--seconds: invalid value {s:?}"))
                    })
                    .transpose()?
                    .unwrap_or(6)
                    .clamp(1, 60);
                if !args.is_empty() {
                    return Err(format!("unexpected argument {:?}\n\n{USAGE}", args[0]));
                }
                println!(
                    "Listening {seconds} s for SAIL Mesh workers on UDP {}…",
                    sail_mesh::discovery::DISCOVERY_PORT
                );
                let found = sail_mesh::discovery::listen(
                    std::net::SocketAddr::from((
                        [0, 0, 0, 0],
                        sail_mesh::discovery::DISCOVERY_PORT,
                    )),
                    Duration::from_secs(seconds),
                )
                .await
                .map_err(|e| {
                    format!(
                        "cannot listen on UDP {}: {e}",
                        sail_mesh::discovery::DISCOVERY_PORT
                    )
                })?;
                if found.is_empty() {
                    println!(
                        "No workers heard. Is `sail-mesh-worker serve` running on the same network, without --no-announce, and allowed through its firewall?"
                    );
                }
                for d in found {
                    let paired = registry
                        .workers
                        .iter()
                        .any(|w| w.fingerprint == d.announcement.fingerprint);
                    println!(
                        "{:<16} {:<22} {}  {}",
                        d.announcement.name,
                        d.address,
                        &d.announcement.fingerprint[..16],
                        if paired {
                            "paired"
                        } else {
                            "not paired: run `sail-mesh-worker pairing-code` on it and paste the code here"
                        }
                    );
                }
                Ok(())
            }
            "test" => {
                let prompt = take(args, "--prompt")?.unwrap_or_else(|| {
                    "Reply with one short sentence confirming you are reachable.".into()
                });
                let name = one_name(args)?;
                let record = registry
                    .workers
                    .iter()
                    .find(|w| w.name == name)
                    .ok_or_else(|| format!("no worker named {name}"))?;
                let worker = RemoteWorker::new(record.clone());
                worker.check(&LocalModelConfig::default()).await;
                let backend = {
                    let state = worker.lock();
                    if let Some(busy) = state.hello.as_ref().and_then(|h| h.busy.as_ref()) {
                        return Err(format!(
                            "worker is resting while its owner uses the PC: {}",
                            busy.reason
                        ));
                    }
                    if state.status != MeshWorkerState::Ready {
                        return Err(format!(
                            "worker is {:?}: {}",
                            state.status,
                            state.last_error.clone().unwrap_or_default()
                        ));
                    }
                    state.backend.clone().expect("ready worker has a backend")
                };
                let mut request = InferenceRequest::text(prompt);
                request.generation = sail_protocol::Generation::direct(64, 0.2);
                let started = Instant::now();
                let response = crate::backend::InferenceBackend::infer(
                    backend.as_ref(),
                    &request,
                    None,
                    &CancellationToken::new(),
                    &sail_protocol::ResourceLimits::default(),
                )
                .await
                .map_err(|e| e.to_string())?;
                let elapsed = started.elapsed();
                println!(
                    "{}: {}",
                    response.model,
                    response.message.content.text().unwrap_or_default().trim()
                );
                let tokens = response.usage.output_tokens.unwrap_or(0);
                println!(
                    "{} output tokens in {:.1} s (first token after {} ms, including prompt processing on the worker)",
                    tokens,
                    elapsed.as_secs_f32(),
                    response.timings.first_token_ms.unwrap_or(0)
                );
                Ok(())
            }
            "enable" | "disable" => {
                let name = one_name(args)?;
                set_enabled_at(path, name, command == "enable").map_err(|e| e.to_string())?;
                println!("Worker \"{name}\" {}d.", command);
                Ok(())
            }
            "remove" => {
                let name = one_name(args)?;
                remove_at(path, name).map_err(|e| e.to_string())?;
                println!("Removed worker \"{name}\" and its secret.");
                Ok(())
            }
            "help" | "--help" | "-h" => {
                println!("{USAGE}");
                Ok(())
            }
            other => Err(format!("unknown mesh command {other:?}\n\n{USAGE}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_parse() {
        assert_eq!(parse_mode("overflow"), Some(MeshMode::Overflow));
        assert_eq!(parse_mode("OFF"), Some(MeshMode::Off));
        assert_eq!(parse_mode(""), Some(MeshMode::Offload));
        assert_eq!(parse_mode("bogus"), None);
        assert_eq!(
            parse_priorities("background, interactive,voice"),
            vec![WorkPriority::Background, WorkPriority::Interactive]
        );
    }

    #[test]
    fn only_request_independent_failures_fall_back() {
        assert!(falls_back(&InferenceError::new(
            ErrorCode::BackendUnavailable,
            "x"
        )));
        assert!(falls_back(&InferenceError::new(ErrorCode::Busy, "x")));
        assert!(!falls_back(&InferenceError::new(ErrorCode::Cancelled, "x")));
        assert!(!falls_back(&InferenceError::new(
            ErrorCode::InvalidRequest,
            "x"
        )));
        assert!(!falls_back(&InferenceError::new(
            ErrorCode::ConsumerClosed,
            "x"
        )));
        assert!(!falls_back(&InferenceError::new(
            ErrorCode::DeadlineExceeded,
            "x"
        )));
    }
}
