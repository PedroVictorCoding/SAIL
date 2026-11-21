//! The compute supervisor: one tick every 2 s turns the lending decision into a running
//! `ggml-rpc-server`, the tunnel gate and `status.json`.
use crate::{
    Result,
    compute::ComputeHandle,
    compute::lend::{self, LendInputs},
    compute::tunnel::LendGate,
    hibernate::Hibernator,
};
use chrono::{DateTime, Local, SecondsFormat, Utc};
use sail_protocol::mesh::{ComputeDevice, LendMode, LendState, LendWindow, MeshBusy, MeshCompute};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::AsyncBufReadExt,
    net::TcpStream,
    process::{Child, Command},
};

/// A lending session is never allowed to keep programs suspended longer than the window end
/// plus this grace. Owner decision (round 29 invariant 3): 15 minutes.
const WATCHDOG_GRACE: chrono::Duration = chrono::Duration::minutes(15);

/// How long a start waits for the RPC port before reporting `Loading` and trying again.
/// Fixed a priori: the real server opens its port in well under a second.
const RPC_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// The supervisor's nominal tick; the CLI documents that a change applies within 2 s.
/// Fixed a priori by the round 29 contract.
pub const TICK_INTERVAL: Duration = Duration::from_secs(2);

/// Most startup lines kept from the RPC server (build string and devices come from here).
/// Fixed a priori: the server prints a handful of lines.
const MAX_STARTUP_LINES: usize = 200;

/// How long one loopback port probe may take. Fixed a priori: local connect.
const PORT_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// Pause between port probes while the RPC server starts. Fixed a priori: fast enough to
/// report `Lending` in the same tick as the first successful start.
const PORT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Fallback session length when neither the window nor a Play `until` gives an end.
/// Fixed a priori: a safety net, not a lending policy.
const FALLBACK_SESSION: chrono::Duration = chrono::Duration::hours(12);

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
    data_dir: PathBuf,
    handle: ComputeHandle,
    gate: LendGate,
    hibernator: Option<Hibernator>,
    mode: LendMode,
    state: LendState,
    rpc: Option<Child>,
    rpc_ready: bool,
    rpc_error: Option<String>,
    build: Option<String>,
    devices: Vec<ComputeDevice>,
    watchdog_deadline: Option<DateTime<Utc>>,
    watchdog_tripped: bool,
    was_lending: bool,
}

impl ComputeSupervisor {
    pub fn new(
        config: ComputeConfig,
        data_dir: &Path,
        gate: LendGate,
        hibernator: Option<Hibernator>,
    ) -> Self {
        let mode = lend::load_mode(&data_dir.join(lend::LEND_FILE));
        let devices = described_devices(&config);
        let handle = ComputeHandle::new(
            gate.clone(),
            config.rpc_port,
            data_dir,
            config.window.clone(),
            devices.clone(),
        );
        let supervisor = Self {
            config,
            data_dir: data_dir.to_path_buf(),
            handle,
            gate,
            hibernator,
            mode,
            state: LendState::Loading,
            rpc: None,
            rpc_ready: false,
            rpc_error: None,
            build: None,
            devices,
            watchdog_deadline: None,
            watchdog_tripped: false,
            was_lending: false,
        };
        supervisor.publish();
        supervisor
    }

    /// One tick (every 2 s): reload lend.json, call `decide`, start/stop the RPC server,
    /// hibernate before starting / resume after stopping, set the gate, write status.json.
    pub async fn tick(&mut self, busy: Option<&MeshBusy>) -> LendState {
        self.reload_mode();
        self.refresh_rpc().await;
        let local_now = Local::now().naive_local();
        let utc_now = Utc::now();

        if self.owner_came_back().await {
            self.publish_and_status();
            return self.state.clone();
        }

        let state = lend::decide(&LendInputs {
            mode: &self.mode,
            window: &self.config.window,
            local_now,
            utc_now,
            busy,
            rpc_up: self.rpc_ready,
            rpc_error: self.rpc_error.as_deref(),
        });
        self.state = match state {
            LendState::Loading | LendState::Lending => self.lend().await,
            other => {
                self.stop_lending().await;
                other
            }
        };
        self.check_watchdog(utc_now).await;
        self.publish_and_status();
        self.was_lending = matches!(self.state, LendState::Lending | LendState::Loading);
        self.state.clone()
    }

    pub fn snapshot(&self) -> MeshCompute {
        self.handle.snapshot()
    }

    /// Set and save the mode (CLI and `POST /mesh/v1/lend`).
    pub fn set_mode(&mut self, mode: LendMode) -> Result<()> {
        self.handle.set_mode(mode.clone())?;
        self.mode = mode;
        if !matches!(self.mode, LendMode::Pause) {
            self.rpc_error = None;
        }
        self.publish();
        Ok(())
    }

    /// Stop the RPC server and resume every hibernated app. Idempotent.
    pub async fn release(&mut self) {
        self.gate.set(false);
        self.stop_rpc().await;
        if let Some(hibernator) = self.hibernator.as_mut() {
            let _ = hibernator.resume_all();
        }
        self.watchdog_deadline = None;
        self.publish();
    }

    /// The handle shared with the worker's HTTP layer.
    pub fn handle(&self) -> ComputeHandle {
        self.handle.clone()
    }

    /// Re-read `lend.json` and expire a Play that has run out.
    fn reload_mode(&mut self) {
        let loaded = lend::load_mode(&self.data_dir.join(lend::LEND_FILE));
        if loaded != self.mode {
            self.mode = loaded;
            if !matches!(self.mode, LendMode::Pause) {
                self.rpc_error = None;
            }
        }
        if let LendMode::Play { until: Some(until) } = &self.mode {
            let expired = DateTime::parse_from_rfc3339(until)
                .map(|until| Utc::now() > until.with_timezone(&Utc))
                .unwrap_or(true);
            if expired {
                self.mode = LendMode::Auto;
                let _ = lend::save_mode(&self.data_dir.join(lend::LEND_FILE), &self.mode);
                self.rpc_error = None;
            }
        }
    }

    /// Notice an RPC server that exited and refresh whether its loopback port is up.
    async fn refresh_rpc(&mut self) {
        let Some(child) = self.rpc.as_mut() else {
            self.rpc_ready = false;
            return;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                self.rpc = None;
                self.rpc_ready = false;
                self.rpc_error = Some(format!("ggml-rpc-server exited ({status})"));
            }
            Ok(None) => {
                self.rpc_ready = port_ready(self.config.rpc_port).await;
            }
            Err(error) => {
                self.rpc = None;
                self.rpc_ready = false;
                self.rpc_error = Some(format!("cannot watch ggml-rpc-server ({error})"));
            }
        }
    }

    /// Inside the window: hibernate once, start the RPC server, keep the gate open.
    async fn lend(&mut self) -> LendState {
        if self.rpc.is_none() && self.rpc_error.is_none() {
            if !self.was_lending && !self.watchdog_tripped {
                self.hibernate_apps().await;
            }
            if let Err(error) = self.start_rpc().await {
                self.rpc_error = Some(error);
                self.gate.set(false);
                return LendState::Unavailable {
                    reason: self
                        .rpc_error
                        .clone()
                        .unwrap_or_else(|| "ggml-rpc-server did not start".into()),
                };
            }
        }
        if self.rpc_ready {
            self.gate.set(true);
            LendState::Lending
        } else {
            self.gate.set(false);
            LendState::Loading
        }
    }

    /// Outside the window or resting: close the tunnel, stop the server, bring the apps back.
    async fn stop_lending(&mut self) {
        self.gate.set(false);
        self.stop_rpc().await;
        if let Some(hibernator) = self.hibernator.as_mut() {
            // Idempotent: an empty journal resumes nothing.
            let _ = hibernator.resume_all();
        }
        self.watchdog_deadline = None;
        self.watchdog_tripped = false;
    }

    async fn hibernate_apps(&mut self) {
        let Some(hibernator) = self.hibernator.as_mut() else {
            return;
        };
        let mut own = vec![std::process::id()];
        if let Some(child) = self.rpc.as_ref().and_then(Child::id) {
            own.push(child);
        }
        match hibernator.hibernate(&own) {
            Ok(hibernated) if !hibernated.is_empty() => {
                self.watchdog_deadline = Some(self.session_end(Utc::now()) + WATCHDOG_GRACE);
                tracing::info!(apps = hibernated.len(), "hibernated apps to lend compute");
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(error = %error, "hibernation failed"),
        }
    }

    /// The end of this lending session: the window end, a Play `until`, or a day.
    fn session_end(&self, utc_now: DateTime<Utc>) -> DateTime<Utc> {
        if let Some(end) =
            lend::window_end(&self.config.window, Local::now().naive_local(), utc_now)
        {
            return end;
        }
        if let LendMode::Play { until: Some(until) } = &self.mode
            && let Ok(until) = DateTime::parse_from_rfc3339(until)
        {
            return until.with_timezone(&Utc);
        }
        utc_now + FALLBACK_SESSION
    }

    /// A suspension past the session end plus the grace is resumed, even if lending goes on.
    async fn check_watchdog(&mut self, utc_now: DateTime<Utc>) {
        let Some(deadline) = self.watchdog_deadline else {
            return;
        };
        if utc_now <= deadline {
            return;
        }
        if let Some(hibernator) = self.hibernator.as_mut() {
            let _ = hibernator.resume_all();
            tracing::warn!("resumed hibernated apps: the lending watchdog expired");
        }
        self.watchdog_deadline = None;
        self.watchdog_tripped = true;
    }

    /// The owner touched a hibernated program: resume everything and step aside.
    async fn owner_came_back(&mut self) -> bool {
        let Some(hibernator) = self.hibernator.as_mut() else {
            return false;
        };
        if !hibernator.owner_returned() {
            return false;
        }
        let _ = hibernator.resume_all();
        self.watchdog_deadline = None;
        self.stop_rpc().await;
        self.gate.set(false);
        self.state = LendState::Resting {
            reason: "you came back to a hibernated program".into(),
            full: false,
        };
        true
    }

    async fn start_rpc(&mut self) -> std::result::Result<(), String> {
        if !self.config.cache_dir.exists()
            && let Err(error) = std::fs::create_dir_all(&self.config.cache_dir)
        {
            return Err(format!("cannot create the RPC cache folder ({error})"));
        }
        let mut command = Command::new(&self.config.rpc_server);
        command
            .arg("-H")
            .arg("127.0.0.1")
            .arg("-p")
            .arg(self.config.rpc_port.to_string())
            .arg("-c")
            .env("LLAMA_CACHE", &self.config.cache_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if !self.config.devices.is_empty() {
            command.arg("-d").arg(self.config.devices.join(","));
        }
        if self.config.threads > 0 {
            command.arg("-t").arg(self.config.threads.to_string());
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("cannot start ggml-rpc-server ({error})"))?;
        let lines = Arc::new(Mutex::new(Vec::new()));
        for stream in [
            child.stdout.take().map(StdioLines::Out),
            child.stderr.take().map(StdioLines::Err),
        ]
        .into_iter()
        .flatten()
        {
            let lines = lines.clone();
            tokio::spawn(read_startup(stream, lines));
        }
        self.rpc = Some(child);
        self.rpc_error = None;
        self.rpc_ready = false;
        let started = tokio::time::Instant::now();
        while started.elapsed() < RPC_READY_TIMEOUT {
            if let Some(child) = self.rpc.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                self.rpc = None;
                return Err(format!("ggml-rpc-server exited ({status})"));
            }
            if port_ready(self.config.rpc_port).await {
                self.rpc_ready = true;
                break;
            }
            tokio::time::sleep(PORT_POLL_INTERVAL).await;
        }
        let captured = lines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(build) = parse_build(&captured) {
            self.build = Some(build);
        }
        let devices = parse_devices(&captured, &self.config);
        if !devices.is_empty() {
            self.devices = devices;
        }
        if !self.rpc_ready {
            tracing::info!("ggml-rpc-server is still starting; the worker reports Loading");
        }
        Ok(())
    }

    async fn stop_rpc(&mut self) {
        self.rpc_ready = false;
        if let Some(mut child) = self.rpc.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }

    fn publish(&self) {
        self.handle.publish(MeshCompute {
            mode: self.mode.clone(),
            state: self.state.clone(),
            window: self.config.window.clone(),
            devices: self.devices.clone(),
            build: self.build.clone(),
            hibernated: self
                .hibernator
                .as_ref()
                .map(Hibernator::hibernated)
                .unwrap_or_default(),
            tunnels: 0,
        });
    }

    fn publish_and_status(&self) {
        self.publish();
        let at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        if let Err(error) = self.handle.write_status(&at) {
            tracing::warn!(error = %error, "cannot write status.json");
        }
    }
}

/// One stream of the RPC server's startup output.
enum StdioLines {
    Out(tokio::process::ChildStdout),
    Err(tokio::process::ChildStderr),
}

impl StdioLines {
    fn into_reader(self) -> Box<dyn tokio::io::AsyncRead + Unpin + Send> {
        match self {
            Self::Out(stdout) => Box::new(stdout),
            Self::Err(stderr) => Box::new(stderr),
        }
    }
}

async fn read_startup(stream: StdioLines, lines: Arc<Mutex<Vec<String>>>) {
    let mut lines_out = tokio::io::BufReader::new(stream.into_reader()).lines();
    while let Ok(Some(line)) = lines_out.next_line().await {
        tracing::debug!(target: "sail_mesh::compute", "{line}");
        let mut captured = lines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if captured.len() < MAX_STARTUP_LINES {
            captured.push(line);
        }
    }
}

/// The llama.cpp build token from the RPC server's own output. This build prints only the
/// RPC protocol version (`Starting RPC server v8.0.1`), so that token stands in for a build;
/// a `build: NNNN` line is preferred when present.
fn parse_build(lines: &[String]) -> Option<String> {
    for line in lines {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        for (index, token) in tokens.iter().enumerate() {
            if (token.eq_ignore_ascii_case("build:") || token.eq_ignore_ascii_case("build"))
                && let Some(next) = tokens.get(index + 1)
            {
                return Some(
                    next.trim_matches(|c: char| !c.is_ascii_alphanumeric())
                        .to_owned(),
                );
            }
            if token.len() > 1
                && token.starts_with('b')
                && token[1..].chars().all(|c| c.is_ascii_digit())
            {
                return Some((*token).to_owned());
            }
        }
        if let Some(version) = line.strip_prefix("Starting RPC server ") {
            return Some(version.trim().to_owned());
        }
    }
    None
}

fn parse_devices(lines: &[String], config: &ComputeConfig) -> Vec<ComputeDevice> {
    let mut devices = Vec::new();
    let mut in_devices = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "Devices:" {
            in_devices = true;
            continue;
        }
        if !in_devices {
            continue;
        }
        if trimmed.starts_with("transport") {
            break;
        }
        let Some((name, rest)) = trimmed.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || name.contains(' ') || name.eq_ignore_ascii_case("endpoint") {
            continue;
        }
        if !config.devices.is_empty()
            && !config
                .devices
                .iter()
                .any(|wanted| wanted.eq_ignore_ascii_case(name))
        {
            continue;
        }
        let description = rest.split(" (").next().unwrap_or(rest).trim().to_owned();
        if description.is_empty() {
            continue;
        }
        devices.push(ComputeDevice {
            name: name.to_owned(),
            description,
            lend_bytes: lend_bytes_for(name, config),
        });
    }
    devices
}

fn lend_bytes_for(name: &str, config: &ComputeConfig) -> u64 {
    if name.eq_ignore_ascii_case("cpu") {
        config.lend_ram_bytes
    } else {
        config.lend_vram_bytes
    }
}

/// The devices the RPC server is asked to expose, before its startup output is parsed.
fn described_devices(config: &ComputeConfig) -> Vec<ComputeDevice> {
    config
        .devices
        .iter()
        .map(|name| ComputeDevice {
            name: name.clone(),
            description: name.clone(),
            lend_bytes: lend_bytes_for(name, config),
        })
        .collect()
}

/// Whether the loopback RPC port accepts a connection right now.
async fn port_ready(port: u16) -> bool {
    matches!(
        tokio::time::timeout(PORT_PROBE_TIMEOUT, TcpStream::connect(("127.0.0.1", port))).await,
        Ok(Ok(_))
    )
}
