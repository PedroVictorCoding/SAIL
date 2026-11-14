//! `sail-mesh-worker`: run on a Windows or Linux PC to lend its model to a SAIL host.
//!
//! ```text
//! sail-mesh-worker init --name desktop
//! sail-mesh-worker serve --llama-server C:\llama\llama-server.exe --model C:\models\qwen.gguf
//! ```
//! `serve` prints the pairing code; on the host run `sail-core mesh pair '<code>'`.
use sail_mesh::{
    DEFAULT_PORT, MESH_VERSION, MeshError, Result,
    discovery::{Announcement, DISCOVERY_PORT, announce},
    identity::{WorkerIdentity, default_dir},
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
    let dir = dir(&mut args)?;
    args.finish()?;

    let identity = WorkerIdentity::load(&dir)?;
    let mut child = None;
    let upstream = match (llama_server, upstream) {
        (Some(_), Some(_)) => {
            return Err(MeshError::new(
                "Use either --upstream or --llama-server, not both",
            ));
        }
        (Some(binary), None) => {
            let model = model.ok_or_else(|| MeshError::new("--llama-server needs --model"))?;
            let mut command = tokio::process::Command::new(&binary);
            command
                .args(["--jinja", "--host", "127.0.0.1", "--port"])
                .arg(llama_port.to_string())
                .args(["--model", &model, "--ctx-size"])
                .arg(ctx.to_string())
                .args(["--parallel"])
                .arg(max_parallel.clamp(1, 16).to_string())
                .args(["--gpu-layers"])
                .arg(gpu_layers.to_string())
                .args(&extra)
                .kill_on_drop(true);
            let spawned = command.spawn().map_err(|e| {
                MeshError::new(format!("Cannot start llama-server {binary:?}: {e}"))
            })?;
            println!(
                "Started llama-server (pid {:?}) on 127.0.0.1:{llama_port}",
                spawned.id()
            );
            child = Some(spawned);
            parse_upstream(&format!("http://127.0.0.1:{llama_port}"))?
        }
        (None, upstream) => {
            if model.is_some() {
                return Err(MeshError::new("--model is only used with --llama-server"));
            }
            parse_upstream(upstream.as_deref().unwrap_or("http://127.0.0.1:8080"))?
        }
    };
    let config = WorkerConfig {
        bind,
        upstream,
        max_parallel,
    };
    if let Some(child) = child.as_mut() {
        println!("Waiting for llama-server to answer http://127.0.0.1:{llama_port}/health");
        wait_for_llama_server(llama_port, child).await?;
    }
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
    if let Some(mut child) = child {
        let _ = child.kill().await;
    }
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
