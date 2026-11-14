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
    discovery::{Announcement, DISCOVERY_PORT, announce},
    identity::{WorkerIdentity, default_dir},
    probe::{PauseList, SystemProbe},
    rest::{Rest, RestGate, RestLimits, RestPolicy},
    worker::{WorkerConfig, parse_upstream, serve},
};
use std::{net::SocketAddr, path::PathBuf, process::ExitCode, time::Duration};

const USAGE: &str = "\
sail-mesh-worker — lend this machine's model to a SAIL host

USAGE:
  sail-mesh-worker init [--name NAME] [--dir DIR]
  sail-mesh-worker serve [OPTIONS]
  sail-mesh-worker pairing-code [--advertise HOST[:PORT]] [--dir DIR]
  sail-mesh-worker rotate-secret [--dir DIR]

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
    let dir = dir(&mut args)?;
    args.finish()?;

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
        let _ = tokio::signal::ctrl_c().await;
    };
    let result = serve(&identity, &config, shutdown).await;
    drop(stop_announcing);
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
