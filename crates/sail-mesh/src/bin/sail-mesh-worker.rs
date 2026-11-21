//! `sail-mesh-worker`: run on a Windows or Linux PC to lend its model to a SAIL host.
//!
//! ```text
//! sail-mesh-worker init --name desktop
//! sail-mesh-worker serve --llama-server C:\llama\llama-server.exe --model C:\models\qwen.gguf
//! ```
//! `serve` prints the pairing code; on the host run `sail-core mesh pair '<code>'`.
//!
//! The worker shares the PC with its owner: llama-server runs at low priority on a capped
//! number of threads, and a usage monitor makes the worker step aside (see
//! `sail_mesh::rest`): completely while a game or music software runs, with exponential
//! backoff while other programs keep the PC busy.
use sail_mesh::{
    DEFAULT_PORT, MESH_VERSION, MeshError, Result,
    compute::{
        STATUS_FILE, StatusFile,
        lend::{self},
        supervisor::{ComputeConfig, ComputeSupervisor, TICK_INTERVAL},
        tunnel::LendGate,
    },
    discovery::{Announcement, DISCOVERY_PORT, announce},
    hibernate::{Hibernator, system_control},
    identity::{WorkerIdentity, default_dir},
    probe::{PauseList, SystemProbe},
    rest::{Rest, RestGate, RestLimits, RestPolicy},
    worker::{WorkerConfig, parse_upstream, serve_with_compute},
};
use sail_protocol::mesh::LendMode;
use std::{net::SocketAddr, path::PathBuf, process::ExitCode, time::Duration};

/// Loopback port for `ggml-rpc-server`. Fixed a priori: llama.cpp's default 50052.
const RPC_PORT: u16 = 50052;

/// `status` treats a file older than this as a stopped worker. Fixed a priori: the
/// supervisor writes every 2 s, so 10 s tolerates a stalled tick or two.
const STATUS_FRESH_SECONDS: i64 = 10;

const USAGE: &str = "\
sail-mesh-worker — lend this machine's model to a SAIL host

USAGE:
  sail-mesh-worker init [--name NAME] [--dir DIR]
  sail-mesh-worker serve [OPTIONS]
  sail-mesh-worker pairing-code [--advertise HOST[:PORT]] [--dir DIR]
  sail-mesh-worker rotate-secret [--dir DIR]
  sail-mesh-worker pause | auto [--dir DIR]
  sail-mesh-worker play [--for D] [--dir DIR]
  sail-mesh-worker status [--dir DIR]
  sail-mesh-worker hibernate add <exe>... | remove <exe>... | list | resume-all [--dir DIR]

SERVE OPTIONS:
  --bind ADDR           Listen address (default 0.0.0.0:7710)
  --advertise HOST      Address the host should dial, for the printed pairing code
                        (default: this machine's LAN address)
  --max-parallel N      Generations accepted at once (default 1)
  --upstream URL        An already running llama-server (default http://127.0.0.1:8080)
  --llama-server PATH   Start this llama-server binary instead (needs --model)
  --model GGUF          Model file for --llama-server
  --ctx N               Context size for --llama-server (default 16384)
  --gpu-layers N        GPU layers for --llama-server (default 99; 0 = CPU only)
  --llama-port N        Loopback port for --llama-server (default 8080)
  --llama-arg ARG       Extra llama-server argument (repeatable)
  --no-announce         Do not announce this worker on the LAN (UDP 7711)
  --dir DIR             Identity folder (default %APPDATA%\\sail-mesh or ~/.config/sail-mesh)

RESOURCE CAPS (for --llama-server):
  --threads N           CPU threads llama-server may use (default: half of this PC's)
  --priority P          low, below-normal or normal (default below-normal): your own
                        programs always win the CPU

STEPPING ASIDE (the worker watches how you use this PC):
  Gaming (full-screen game, or heavy GPU use by another program) or music software
  (Ableton, FL Studio, REAPER, Cubase, Bitwig, Studio One, Pro Tools…) stops
  llama-server at once to free the GPU and memory. Other heavy use (CPU, GPU, low memory)
  pauses new work for 1 min, doubling each time it is still busy (up to --max-backoff).
  --pause-for NAME      Also step aside completely while this program runs, for example
                        a game: --pause-for Cyberpunk2077.exe (repeatable)
  --busy-cpu PCT        Other programs' CPU that counts as busy (default 50)
  --busy-gpu PCT        Other programs' GPU that counts as busy (default 20)
  --gaming-gpu PCT      Other programs' GPU that counts as gaming (default 50)
  --min-free-ram MB     Less free memory than this counts as busy (default 2048)
  --max-backoff MIN     Longest pause, in minutes (default 30)
  --no-rest             Never step aside (the worker serves whenever it is asked)

POOLED COMPUTE (lend ggml devices to a SAIL host through this worker's TLS):
  --compute PATH        Start this ggml-rpc-server as well (compute-only workers work too)
  --cache-dir DIR       RPC weight cache (default <dir>/rpc-cache; exported as LLAMA_CACHE)
  --compute-devices L   Devices for -d, for example Vulkan0,CPU (default: all the server finds)
  --compute-threads N   CPU threads for the CPU device (default: half of this PC's)
  --window HH:MM-HH:MM  Lending window in this PC's own time zone (default 01:00-08:00)
  --lend-vram-gib N     VRAM the host may use on each GPU (default 0 = no declared cap)
  --lend-ram-gib N      RAM the host may use on the CPU device (default 0 = no declared cap)
";

struct Args(Vec<String>);
impl Args {
    fn take(&mut self, flag: &str) -> Result<Option<String>> {
        let Some(i) = self.0.iter().position(|a| a == flag) else {
            return Ok(None);
        };
        if i + 1 >= self.0.len() {
            return Err(MeshError::new(format!("{flag} needs a value")));
        }
        let value = self.0.remove(i + 1);
        self.0.remove(i);
        Ok(Some(value))
    }
    fn take_all(&mut self, flag: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        while let Some(v) = self.take(flag)? {
            out.push(v);
        }
        Ok(out)
    }
    fn parse<T: std::str::FromStr>(&mut self, flag: &str) -> Result<Option<T>> {
        self.take(flag)?
            .map(|v| {
                v.parse()
                    .map_err(|_| MeshError::new(format!("{flag}: invalid value {v:?}")))
            })
            .transpose()
    }
    fn flag(&mut self, name: &str) -> bool {
        match self.0.iter().position(|a| a == name) {
            Some(index) => {
                self.0.remove(index);
                true
            }
            None => false,
        }
    }
    fn finish(self) -> Result<()> {
        match self.0.first() {
            Some(extra) => Err(MeshError::new(format!(
                "Unknown argument {extra:?}\n\n{USAGE}"
            ))),
            None => Ok(()),
        }
    }
}

fn dir(args: &mut Args) -> Result<PathBuf> {
    Ok(args
        .take("--dir")?
        .map(PathBuf::from)
        .unwrap_or_else(default_dir))
}

/// This machine's address on the LAN: the source address the OS would use to reach a
/// public IP. Nothing is sent (UDP connect only picks a route).
fn lan_address() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    socket.local_addr().ok().map(|a| a.ip())
}

fn advertised(advertise: Option<String>, port: u16) -> String {
    match advertise {
        Some(a) if a.contains(':') && !a.starts_with('[') && a.matches(':').count() == 1 => a,
        Some(a) if a.starts_with('[') && a.contains("]:") => a,
        Some(a) => format!("{a}:{port}"),
        None => match lan_address() {
            Some(std::net::IpAddr::V6(ip)) => format!("[{ip}]:{port}"),
            Some(ip) => format!("{ip}:{port}"),
            None => format!("127.0.0.1:{port}"),
        },
    }
}

fn print_code(identity: &WorkerIdentity, address: &str) {
    let code = identity.pairing_code(address).render();
    println!("\nPair this worker by running on the SAIL host:\n");
    println!("  sail-core mesh pair '{code}'\n");
    println!("The code contains this worker's secret. Treat it like a password.");
    println!("Certificate fingerprint: {}\n", identity.fingerprint());
}

/// Write the owner's lending mode; a running worker applies it within 2 s.
fn lend_command(command: &str, mut args: Args) -> Result<()> {
    let dir = dir(&mut args)?;
    let mode = match command {
        "pause" => LendMode::Pause,
        "auto" => LendMode::Auto,
        _ => {
            let until = match args
                .take("--for")?
                .map(|text| lend::parse_duration(&text))
                .transpose()?
            {
                Some(duration) => {
                    let duration = chrono::Duration::from_std(duration)
                        .map_err(|_| MeshError::new("That lending time is too long"))?;
                    let until = chrono::Utc::now()
                        .checked_add_signed(duration)
                        .ok_or_else(|| MeshError::new("That lending time is too far away"))?;
                    Some(until.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                }
                None => None,
            };
            LendMode::Play { until }
        }
    };
    args.finish()?;
    lend::save_mode(&dir.join(lend::LEND_FILE), &mode)?;
    println!(
        "{}",
        match &mode {
            LendMode::Pause =>
                "Paused lending. The worker applies this within 2 seconds.".to_owned(),
            LendMode::Auto =>
                "Automatic lending. The worker follows its window (01:00-08:00 by default)."
                    .to_owned(),
            LendMode::Play { until: Some(until) } => format!("Lending now until {until}."),
            LendMode::Play { until: None } => "Lending now until you change it.".to_owned(),
        }
    );
    Ok(())
}

/// One plain-words line from the file the supervisor writes each tick.
fn status_command(mut args: Args) -> Result<()> {
    let dir = dir(&mut args)?;
    args.finish()?;
    let stale = || {
        println!("The worker is not running");
    };
    let Ok(data) = std::fs::read(dir.join(STATUS_FILE)) else {
        stale();
        return Ok(());
    };
    let Ok(status) = serde_json::from_slice::<StatusFile>(&data) else {
        stale();
        return Ok(());
    };
    let fresh = chrono::DateTime::parse_from_rfc3339(&status.at)
        .map(|at| {
            (chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_seconds()
                <= STATUS_FRESH_SECONDS
        })
        .unwrap_or(false);
    if !fresh {
        stale();
        return Ok(());
    }
    println!("{}", status_line(&status.compute));
    Ok(())
}

fn status_line(compute: &sail_protocol::mesh::MeshCompute) -> String {
    use sail_protocol::mesh::LendState;
    match &compute.state {
        LendState::Lending => {
            let tunnels = compute.tunnels;
            format!(
                "Lending: this PC is lending compute to SAIL ({tunnels} connection{}).",
                if tunnels == 1 { "" } else { "s" }
            )
        }
        LendState::Loading => "Loading: the RPC server is starting.".to_owned(),
        LendState::Paused => "Paused by you.".to_owned(),
        LendState::OutsideWindow { next_start } => {
            format!("Outside lending hours (next start {next_start}).")
        }
        LendState::Resting { reason, .. } => format!("Resting: {reason}."),
        LendState::Unavailable { reason } => format!("Unavailable: {reason}."),
    }
}

/// The owner's hibernation list and the programs suspended right now.
fn hibernate_command(mut args: Args) -> Result<()> {
    let dir = dir(&mut args)?;
    let Some(action) = args.0.first().cloned() else {
        return Err(MeshError::new(
            "Use: sail-mesh-worker hibernate add <exe>... | remove <exe>... | list | resume-all",
        ));
    };
    args.0.remove(0);
    let names = std::mem::take(&mut args.0);
    let mut hibernator = Hibernator::new(system_control(), &dir);
    match action.as_str() {
        "add" => {
            hibernator.add(&names)?;
            println!(
                "Hibernating {} program name(s) while lending compute.",
                names.len()
            );
        }
        "remove" => {
            hibernator.remove(&names)?;
            println!("Removed {} program name(s) from hibernation.", names.len());
        }
        "list" => {
            let listed = hibernator.listed();
            if listed.is_empty() {
                println!("No programs are listed for hibernation.");
            } else {
                println!("Listed for hibernation: {}", listed.join(", "));
            }
            let hibernated = hibernator.hibernated();
            if hibernated.is_empty() {
                println!("Nothing is hibernated right now.");
            } else {
                for app in hibernated {
                    println!(
                        "{} ({} process{}): {} released since {}",
                        app.name,
                        app.processes,
                        if app.processes == 1 { "" } else { "es" },
                        app.released_bytes,
                        app.since
                    );
                }
            }
        }
        "resume-all" => {
            let resumed = hibernator.resume_all()?;
            println!("Resumed {resumed} process(es).");
        }
        other => {
            return Err(MeshError::new(format!(
                "Unknown hibernate action {other:?}\n\n{USAGE}"
            )));
        }
    }
    Ok(())
}

/// GiB written as a decimal number, converted to bytes.
fn gib_to_bytes(text: &str) -> Result<u64> {
    let gib: f64 = text
        .parse()
        .map_err(|_| MeshError::new(format!("--lend-*-gib needs a number, got {text:?}")))?;
    if !gib.is_finite() || gib < 0.0 {
        return Err(MeshError::new(format!("--lend-*-gib cannot be {text:?}")));
    }
    Ok((gib * 1024.0 * 1024.0 * 1024.0) as u64)
}

async fn run() -> Result<()> {
    let mut args = Args(std::env::args().skip(1).collect());
    let command = if args.0.is_empty() {
        String::from("help")
    } else {
        args.0.remove(0)
    };
    match command.as_str() {
        "init" => {
            let name = args.take("--name")?.unwrap_or_else(|| {
                std::env::var("COMPUTERNAME")
                    .or_else(|_| std::env::var("HOSTNAME"))
                    .unwrap_or_else(|_| "worker".into())
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                    .take(40)
                    .collect()
            });
            let dir = dir(&mut args)?;
            args.finish()?;
            let identity = WorkerIdentity::create(&dir, &name)?;
            println!("Created worker \"{}\" in {}", identity.name, dir.display());
            println!("Next: sail-mesh-worker serve --llama-server <path> --model <gguf>");
            Ok(())
        }
        "pairing-code" => {
            let advertise = args.take("--advertise")?;
            let dir = dir(&mut args)?;
            args.finish()?;
            let identity = WorkerIdentity::load(&dir)?;
            print_code(&identity, &advertised(advertise, DEFAULT_PORT));
            Ok(())
        }
        "rotate-secret" => {
            let dir = dir(&mut args)?;
            args.finish()?;
            let mut identity = WorkerIdentity::load(&dir)?;
            identity.rotate_secret()?;
            println!("New secret saved. The host must pair again (restart `serve` to use it).");
            Ok(())
        }
        "pause" | "auto" | "play" => lend_command(&command, args),
        "status" => status_command(args),
        "hibernate" => hibernate_command(args),
        "serve" => serve_command(args).await,
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(MeshError::new(format!(
            "Unknown command {other:?}\n\n{USAGE}"
        ))),
    }
}

async fn wait_for_llama_server(port: u16, child: &mut tokio::process::Child) -> Result<()> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| MeshError::new(format!("Cannot check llama-server: {e}")))?;
    let url = format!("http://127.0.0.1:{port}/health");
    let started = tokio::time::Instant::now();
    let mut report_at = Duration::from_secs(15);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            eprintln!("llama-server exited before it was ready: {status}");
            return Err(MeshError::new(
                "llama-server exited before it answered /health",
            ));
        }
        if let Ok(response) = client.get(&url).send().await
            && response.status().is_success()
        {
            return Ok(());
        }
        let waited = started.elapsed();
        if waited >= Duration::from_secs(600) {
            return Err(MeshError::new(
                "llama-server did not answer /health within 10 minutes",
            ));
        }
        if waited >= report_at {
            println!(
                "Waiting for llama-server to load the model… ({} s)",
                waited.as_secs()
            );
            report_at += Duration::from_secs(15);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Priority {
    Low,
    BelowNormal,
    Normal,
}

impl std::str::FromStr for Priority {
    type Err = ();
    fn from_str(value: &str) -> std::result::Result<Self, ()> {
        match value.to_ascii_lowercase().as_str() {
            "low" | "idle" => Ok(Self::Low),
            "below-normal" | "below_normal" | "belownormal" => Ok(Self::BelowNormal),
            "normal" => Ok(Self::Normal),
            _ => Err(()),
        }
    }
}

/// The llama-server this worker started, which it can stop and start again.
struct Llama {
    binary: String,
    args: Vec<String>,
    port: u16,
    priority: Priority,
    child: Option<tokio::process::Child>,
}

impl Llama {
    fn command(&self) -> tokio::process::Command {
        // Linux/macOS: `nice` lowers the priority and then becomes llama-server (same pid).
        #[cfg(unix)]
        let mut command = match self.priority {
            Priority::Normal => tokio::process::Command::new(&self.binary),
            low => {
                let mut command = tokio::process::Command::new("nice");
                let level = if low == Priority::Low { "19" } else { "10" };
                command.args(["-n", level]).arg(&self.binary);
                command
            }
        };
        #[cfg(not(unix))]
        let mut command = tokio::process::Command::new(&self.binary);
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::{
                BELOW_NORMAL_PRIORITY_CLASS, IDLE_PRIORITY_CLASS,
            };
            match self.priority {
                Priority::Low => {
                    command.creation_flags(IDLE_PRIORITY_CLASS);
                }
                Priority::BelowNormal => {
                    command.creation_flags(BELOW_NORMAL_PRIORITY_CLASS);
                }
                Priority::Normal => {}
            }
        }
        command.args(&self.args).kill_on_drop(true);
        command
    }

    fn start(&mut self) -> Result<()> {
        let spawned = self.command().spawn();
        #[cfg(unix)]
        let spawned = match spawned {
            Err(_) if self.priority != Priority::Normal => {
                eprintln!("`nice` is not available; starting llama-server at normal priority");
                let mut command = tokio::process::Command::new(&self.binary);
                command.args(&self.args).kill_on_drop(true);
                command.spawn()
            }
            other => other,
        };
        let spawned = spawned.map_err(|e| {
            MeshError::new(format!("Cannot start llama-server {:?}: {e}", self.binary))
        })?;
        println!(
            "Started llama-server (pid {:?}) on 127.0.0.1:{}",
            spawned.id(),
            self.port
        );
        self.child = Some(spawned);
        Ok(())
    }

    async fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
        }
    }

    fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(|c| c.id())
    }
}

fn minutes(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 90 {
        format!("{seconds} s")
    } else {
        format!("{} min", (seconds + 30) / 60)
    }
}

/// Watch how the PC is used and step aside when its owner needs it. Owns the managed
/// llama-server (if any) and stops it when `shutdown` fires.
async fn monitor(
    gate: RestGate,
    mut llama: Option<Llama>,
    rest: Option<(RestLimits, PauseList)>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let Some((limits, pause)) = rest else {
        let _ = shutdown.await;
        if let Some(llama) = llama.as_mut() {
            llama.stop().await;
        }
        return;
    };
    let mut probe = Some(SystemProbe::new(pause));
    if let Some(probe) = &probe {
        println!(
            "Watching this PC ({}) to step aside while you use it. --no-rest turns this off.",
            probe.describe()
        );
    }
    let mut policy = RestPolicy::new(limits);
    // Stopped by the monitor (not crashed): start it again once the PC is free.
    let mut rested = false;
    let mut shown = Rest::Available;
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
        let mut own = vec![std::process::id()];
        own.extend(llama.as_ref().and_then(Llama::pid));
        let generating = gate.active() > 0;
        // Listing processes takes a few milliseconds; keep it off the async threads.
        let Some(mut taken) = probe.take() else { break };
        let Ok((back, reading)) = tokio::task::spawn_blocking(move || {
            let reading = taken.sample(&own, generating);
            (taken, reading)
        })
        .await
        else {
            break;
        };
        probe = Some(back);
        let now = std::time::Instant::now();
        let state = policy.observe(&reading, now).clone();
        gate.set_busy(policy.busy(now));
        let changed = std::mem::discriminant(&state) != std::mem::discriminant(&shown)
            || matches!((&state, &shown), (Rest::Backoff { until: a, .. }, Rest::Backoff { until: b, .. }) if a != b);
        match &state {
            Rest::Full { reason } => {
                if changed {
                    println!("Stepping aside: {reason}. New work goes back to the SAIL host.");
                }
                if let Some(llama) = llama.as_mut()
                    && llama.child.is_some()
                {
                    llama.stop().await;
                    rested = true;
                    println!("Stopped llama-server to free the GPU, CPU and memory.");
                }
            }
            Rest::Backoff { until, reason } if changed => {
                println!(
                    "Pausing new work for {}: {reason}.",
                    minutes(until.saturating_duration_since(now))
                );
            }
            Rest::Backoff { .. } => {}
            Rest::Available => {
                if changed {
                    println!("This PC is free again; taking work.");
                }
                if rested && let Some(llama) = llama.as_mut() {
                    match llama.start() {
                        Ok(()) => rested = false,
                        Err(e) => eprintln!("{e}"),
                    }
                }
            }
        }
        shown = state;
    }
    gate.set_busy(None);
    if let Some(llama) = llama.as_mut() {
        llama.stop().await;
    }
}

async fn serve_command(mut args: Args) -> Result<()> {
    let bind: SocketAddr = args
        .parse("--bind")?
        .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)));
    let advertise = args.take("--advertise")?;
    let max_parallel: usize = args.parse("--max-parallel")?.unwrap_or(1);
    let upstream = args.take("--upstream")?;
    let llama_server = args.take("--llama-server")?;
    let model = args.take("--model")?;
    let ctx: u32 = args.parse("--ctx")?.unwrap_or(16_384);
    let gpu_layers: u32 = args.parse("--gpu-layers")?.unwrap_or(99);
    let llama_port: u16 = args.parse("--llama-port")?.unwrap_or(8080);
    let extra = args.take_all("--llama-arg")?;
    let no_announce = args.flag("--no-announce");
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    let threads: Option<usize> = args.parse("--threads")?;
    let priority: Option<Priority> = args.parse("--priority")?;
    let pause = args.take_all("--pause-for")?;
    let mut limits = RestLimits::default();
    if let Some(cpu) = args.parse::<f32>("--busy-cpu")? {
        limits.cpu_percent = cpu.clamp(1.0, 100.0);
    }
    if let Some(gpu) = args.parse::<f32>("--busy-gpu")? {
        limits.gpu_percent = gpu.clamp(1.0, 100.0);
    }
    if let Some(gpu) = args.parse::<f32>("--gaming-gpu")? {
        limits.gpu_full_percent = gpu.clamp(1.0, 100.0);
    }
    if let Some(mb) = args.parse::<u64>("--min-free-ram")? {
        limits.min_free_ram_mb = mb;
    }
    if let Some(max) = args.parse::<u64>("--max-backoff")? {
        limits.max_backoff = Duration::from_secs(max.clamp(1, 24 * 60) * 60);
    }
    let no_rest = args.flag("--no-rest");
    let compute_binary = args.take("--compute")?;
    let cache_dir = args.take("--cache-dir")?;
    let compute_devices: Vec<String> = args
        .take("--compute-devices")?
        .map(|list| {
            list.split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let compute_threads: Option<usize> = args.parse("--compute-threads")?;
    let window_text = args.take("--window")?;
    let lend_vram_gib = args.take("--lend-vram-gib")?;
    let lend_ram_gib = args.take("--lend-ram-gib")?;
    let dir = dir(&mut args)?;
    args.finish()?;

    if compute_binary.is_none() {
        for (flag, set) in [
            ("--cache-dir", cache_dir.is_some()),
            ("--compute-devices", !compute_devices.is_empty()),
            ("--compute-threads", compute_threads.is_some()),
            ("--window", window_text.is_some()),
            ("--lend-vram-gib", lend_vram_gib.is_some()),
            ("--lend-ram-gib", lend_ram_gib.is_some()),
        ] {
            if set {
                return Err(MeshError::new(format!(
                    "{flag} is only used with --compute"
                )));
            }
        }
    }
    let compute_window = match window_text {
        Some(text) => lend::parse_window(&text)?,
        None => lend::parse_window("01:00-08:00")?,
    };
    let compute_config = match compute_binary {
        Some(binary) => Some(ComputeConfig {
            rpc_server: PathBuf::from(binary),
            cache_dir: cache_dir
                .map(PathBuf::from)
                .unwrap_or_else(|| dir.join("rpc-cache")),
            devices: compute_devices,
            threads: compute_threads.unwrap_or(cores / 2).clamp(1, cores.max(1)),
            window: compute_window,
            lend_vram_bytes: gib_to_bytes(lend_vram_gib.as_deref().unwrap_or("0"))?,
            lend_ram_bytes: gib_to_bytes(lend_ram_gib.as_deref().unwrap_or("0"))?,
            rpc_port: RPC_PORT,
        }),
        None => None,
    };

    let identity = WorkerIdentity::load(&dir)?;
    let mut llama = None;
    let upstream = match (llama_server, upstream) {
        (Some(_), Some(_)) => {
            return Err(MeshError::new(
                "Use either --upstream or --llama-server, not both",
            ));
        }
        (Some(binary), None) => {
            let model = model.ok_or_else(|| MeshError::new("--llama-server needs --model"))?;
            let threads = threads.unwrap_or(cores / 2).clamp(1, cores.max(1));
            let mut llama_args: Vec<String> = vec![
                "--jinja".into(),
                "--host".into(),
                "127.0.0.1".into(),
                "--port".into(),
                llama_port.to_string(),
                "--model".into(),
                model,
                "--ctx-size".into(),
                ctx.to_string(),
                "--parallel".into(),
                max_parallel.clamp(1, 16).to_string(),
                "--gpu-layers".into(),
                gpu_layers.to_string(),
                "--threads".into(),
                threads.to_string(),
                "--threads-batch".into(),
                threads.to_string(),
            ];
            llama_args.extend(extra);
            let priority = priority.unwrap_or(Priority::BelowNormal);
            println!(
                "llama-server limits: {threads} of {cores} CPU threads, {gpu_layers} GPU layers, \
                 {} at once, {:?} priority",
                max_parallel.clamp(1, 16),
                priority
            );
            let mut managed = Llama {
                binary,
                args: llama_args,
                port: llama_port,
                priority,
                child: None,
            };
            managed.start()?;
            llama = Some(managed);
            parse_upstream(&format!("http://127.0.0.1:{llama_port}"))?
        }
        (None, upstream) => {
            if model.is_some() {
                return Err(MeshError::new("--model is only used with --llama-server"));
            }
            if threads.is_some() || priority.is_some() {
                return Err(MeshError::new(
                    "--threads and --priority apply to --llama-server; set them on your own \
                     llama-server instead",
                ));
            }
            parse_upstream(upstream.as_deref().unwrap_or("http://127.0.0.1:8080"))?
        }
    };
    let gate = RestGate::default();
    let config = WorkerConfig {
        bind,
        upstream,
        max_parallel,
        gate: gate.clone(),
    };
    let mut hibernator = Hibernator::new(system_control(), &dir);
    match hibernator.recover() {
        Ok(0) => {}
        Ok(resumed) => println!("Resumed {resumed} program(s) from the hibernation journal."),
        Err(error) => eprintln!("warning: cannot recover hibernated programs: {error}"),
    }
    let (compute_handle, mut compute_loop) = match compute_config {
        Some(compute_config) => {
            println!(
                "Pooled compute: ggml-rpc-server on 127.0.0.1:{}; window {}-{} (this PC's time).",
                compute_config.rpc_port, compute_config.window.start, compute_config.window.end
            );
            let supervisor =
                ComputeSupervisor::new(compute_config, &dir, LendGate::new(), Some(hibernator));
            let handle = supervisor.handle();
            let rest = gate.clone();
            let (stop, mut wait) = tokio::sync::oneshot::channel::<()>();
            let task = tokio::spawn(async move {
                let mut supervisor = supervisor;
                loop {
                    let busy = rest.busy();
                    supervisor.tick(busy.as_ref()).await;
                    tokio::select! {
                        _ = tokio::time::sleep(TICK_INTERVAL) => {}
                        _ = &mut wait => {
                            supervisor.release().await;
                            break;
                        }
                    }
                }
            });
            (Some(handle), Some((stop, task)))
        }
        None => (None, None),
    };
    if let Some(Llama {
        child: Some(child), ..
    }) = llama.as_mut()
    {
        println!("Waiting for llama-server to answer http://127.0.0.1:{llama_port}/health");
        wait_for_llama_server(llama_port, child).await?;
    }
    if no_rest {
        println!(
            "Stepping aside is off (--no-rest): the worker takes work even while you use this PC."
        );
    } else if llama.is_none() {
        println!(
            "Using your own llama-server: while you game or make music the worker refuses work, \
             but it cannot stop that server to free the GPU."
        );
    }
    let rest = (!no_rest).then(|| (limits, PauseList::new(&pause)));
    let (stop_monitor, monitor_stopped) = tokio::sync::oneshot::channel::<()>();
    let monitor = tokio::spawn(monitor(gate, llama, rest, monitor_stopped));
    println!(
        "SAIL Mesh worker \"{}\" listening on https://{} (model server {})",
        identity.name, config.bind, config.upstream
    );
    let stop_announcing = if no_announce {
        println!("LAN announcement is off (--no-announce); pairing is still manual.");
        None
    } else {
        let announcement = Announcement {
            version: MESH_VERSION,
            worker_id: identity.worker_id.clone(),
            name: identity.name.clone(),
            port: config.bind.port(),
            fingerprint: identity.fingerprint(),
        };
        let (stop, wait) = tokio::sync::oneshot::channel::<()>();
        let target = SocketAddr::from(([255, 255, 255, 255], DISCOVERY_PORT));
        tokio::spawn(announce(
            announcement,
            target,
            Duration::from_secs(5),
            async move {
                let _ = wait.await;
            },
        ));
        println!(
            "Announcing on the LAN every 5 s (UDP {DISCOVERY_PORT}); pairing is still manual. \
             Use --no-announce to stop."
        );
        Some(stop)
    };
    print_code(&identity, &advertised(advertise, config.bind.port()));
    let shutdown = async {
        #[cfg(unix)]
        {
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = async {
                    match terminate.as_mut() {
                        Some(signal) => {
                            signal.recv().await;
                        }
                        None => std::future::pending::<()>().await,
                    }
                } => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    };
    let result = serve_with_compute(&identity, &config, compute_handle, shutdown).await;
    drop(stop_announcing);
    if let Some((stop, task)) = compute_loop.take() {
        let _ = stop.send(());
        let _ = task.await;
    }
    let _ = stop_monitor.send(());
    let _ = monitor.await;
    result
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sail_mesh=info".into()),
        )
        .init();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
