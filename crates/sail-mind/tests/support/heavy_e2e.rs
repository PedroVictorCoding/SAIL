use std::{
    fs::{self, File, Permissions},
    io::Write,
    net::{SocketAddr, TcpListener},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    routing::{get, post},
};
use sail_common::resources::{HostProbe, ResourceScheduler};
use sail_mesh::{
    Registry,
    compute::{
        supervisor::{ComputeConfig, ComputeSupervisor},
        tunnel::LendGate,
    },
    identity::WorkerIdentity,
    rest::RestGate,
    worker::{WorkerConfig, serve_with_compute},
};
use sail_mind::{
    CancellationToken, LocalModelConfig, MindService,
    heavy::HeavyLane,
    local::LocalGemmaBackend,
    mesh::{MeshPool, MeshSettings, pair_at},
    protocol::{
        ResourceLimits,
        mesh::{HeavyState, LendMode, LendState, LendWindow, MeshMode},
        resources::HostSample,
    },
};
use serde_json::json;
use tokio::time::{sleep, timeout};

pub const WORKER: &str = "c8-worker";
const CHILD: &str = "SAIL_C8_TEST_CHILD";

fn free_address() -> SocketAddr {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
}

// Isolate real environment-based configuration and the managed server's log from
// the test harness and from the checkout.
pub async fn enter_fixture(test: &str) -> bool {
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return true;
    }
    let dir = tempfile::tempdir().unwrap();
    let server = dir.path().join("fake-llama-server.py");
    fs::write(&server, include_str!("../fixtures/heavy-e2e-server.py")).unwrap();
    fs::set_permissions(&server, Permissions::from_mode(0o700)).unwrap();
    let model = dir.path().join("pooled.gguf");
    model_file(&model, 32, 32);
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", test, "--nocapture"])
        .current_dir(dir.path())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env(CHILD, test)
        .env("SAIL_HEAVY_MODEL_PATH", model)
        .env("SAIL_HEAVY_SERVER_BINARY", server)
        .env("SAIL_HEAVY_PORT", free_address().port().to_string())
        .env("SAIL_HEAVY_CTX", "512")
        .env("SAIL_HEAVY_HOST_RAM_GIB", "2")
        .env("SAIL_HEAVY_HOST_VRAM_GIB", "0")
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(45), child.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "child must run the acceptance test"
    );
    false
}

fn string(buffer: &mut Vec<u8>, value: &str) {
    buffer.extend((value.len() as u64).to_le_bytes());
    buffer.extend(value.as_bytes());
}

fn model_file(path: &Path, host_bytes: u64, expert_bytes: u64) {
    let mut header = b"GGUF".to_vec();
    header.extend(3_u32.to_le_bytes());
    header.extend(2_u64.to_le_bytes()); // tensors
    header.extend(3_u64.to_le_bytes()); // metadata entries

    string(&mut header, "general.architecture");
    header.extend(8_u32.to_le_bytes()); // string
    string(&mut header, "gpt-oss");
    for (key, count) in [("gpt-oss.block_count", 1_u32), ("gpt-oss.expert_count", 2)] {
        string(&mut header, key);
        header.extend(4_u32.to_le_bytes()); // u32
        header.extend(count.to_le_bytes());
    }
    for (name, offset) in [
        ("token_embd.weight", 0),
        ("blk.0.ffn_up_exps.weight", host_bytes),
    ] {
        string(&mut header, name);
        header.extend(1_u32.to_le_bytes()); // dimensions
        header.extend(8_u64.to_le_bytes()); // elements
        header.extend(0_u32.to_le_bytes()); // f32
        header.extend(offset.to_le_bytes());
    }
    header.resize(header.len().next_multiple_of(32), 0);
    let mut file = File::create(path).unwrap();
    file.write_all(&header).unwrap();
    // The fixture reads only the header. The sparse body models tensor offsets.
    file.set_len(header.len() as u64 + host_bytes + expert_bytes)
        .unwrap();
}

struct RoomyHost;

impl HostProbe for RoomyHost {
    fn sample(&self) -> std::io::Result<HostSample> {
        const GIB: u64 = 1 << 30;
        Ok(HostSample {
            available_ram_bytes: 16 * GIB,
            total_ram_bytes: 32 * GIB,
            swap_used_bytes: 0,
            swap_total_bytes: 0,
            logical_cpus: 4,
            memory_pressure_avg10: Some(0.0),
        })
    }
}

pub async fn wait_for(mut ready: impl FnMut() -> bool, limit: Duration) -> bool {
    timeout(limit, async {
        while !ready() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok()
}

pub struct Stack {
    pub mind: MindService,
    pub lane: Arc<HeavyLane>,
    pub host_endpoint: String,
    pub model: PathBuf,
    pub host_completions: Arc<AtomicUsize>,
    pool: Arc<MeshPool>,
    supervisor: ComputeSupervisor,
    worker_stop: CancellationToken,
    worker_addr: SocketAddr,
    rpc_addr: SocketAddr,
    host_stop: tokio::task::JoinHandle<()>,
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.worker_stop.cancel();
        self.host_stop.abort();
    }
}

impl Stack {
    pub async fn start() -> Self {
        let root = std::env::current_dir().unwrap();
        let model = root.join("pooled.gguf");
        let resident = root.join("resident.gguf");
        let host_completions = Arc::new(AtomicUsize::new(0));
        let counter = host_completions.clone();
        let app = Router::new()
            .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
            .route(
                "/props",
                get(move || {
                    let path = resident.clone();
                    async move { Json(json!({"model_path":path, "n_ctx":8192})) }
                }),
            )
            .route(
                "/v1/chat/completions",
                post(move || {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::Relaxed);
                        Json(
                            json!({"choices":[{"message":{"content":"unexpected host fallback"}}]}),
                        )
                    }
                }),
            );
        let host = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let host_base = format!("http://{}", host.local_addr().unwrap());
        let host_endpoint = format!("{host_base}/v1/chat/completions");
        let host_stop = tokio::spawn(async move { axum::serve(host, app).await.unwrap() });

        let worker_data = root.join("worker");
        let identity = WorkerIdentity::create(&worker_data, WORKER).unwrap();
        let rpc_addr = free_address();
        let mut supervisor = ComputeSupervisor::new(
            ComputeConfig {
                rpc_server: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../sail-mesh/tests/fixtures/fake-ggml-rpc-server.py"),
                cache_dir: worker_data.join("cache"),
                devices: vec!["CPU".into()],
                threads: 1,
                window: LendWindow {
                    start: "00:00".into(),
                    end: "00:00".into(),
                },
                lend_vram_bytes: 0,
                lend_ram_bytes: 0,
                rpc_port: rpc_addr.port(),
            },
            &worker_data,
            LendGate::new(),
            None,
        );
        supervisor.set_mode(LendMode::Pause).unwrap();
        assert_eq!(supervisor.tick(None).await, LendState::Paused);
        let worker_addr = free_address();
        let pairing = identity.pairing_code(&worker_addr.to_string()).render();
        let worker = WorkerConfig {
            bind: worker_addr,
            upstream: host_base.parse().unwrap(),
            max_parallel: 1,
            gate: RestGate::default(),
        };
        let worker_stop = CancellationToken::new();
        let stop = worker_stop.clone().cancelled_owned();
        let compute = supervisor.handle();
        tokio::spawn(async move {
            serve_with_compute(&identity, &worker, Some(compute), stop)
                .await
                .unwrap()
        });
        timeout(Duration::from_secs(3), async {
            while tokio::net::TcpStream::connect(worker_addr).await.is_err() {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();

        let registry_path = Registry::path_in(&root);
        let (record, hello, replaced) = pair_at(&registry_path, &pairing, vec![]).await.unwrap();
        assert!(!replaced);
        assert_eq!(record.name, WORKER);
        assert_eq!(hello.compute.unwrap().state, LendState::Paused);
        let config = LocalModelConfig {
            model_path: root.join("resident.gguf"),
            server_binary: root.join("fake-llama-server.py"),
            endpoint: host_endpoint.clone(),
            managed: false,
            persist: false,
            ..Default::default()
        };
        let scheduler = Arc::new(ResourceScheduler::new(
            Arc::new(RoomyHost),
            Default::default(),
        ));
        let backend =
            LocalGemmaBackend::with_scheduler(config.clone(), 1, scheduler.clone()).unwrap();
        let mind = MindService::with_scheduler(
            vec![Arc::new(backend)],
            ResourceLimits::default(),
            scheduler,
        )
        .unwrap();
        let pool = MeshPool::new(
            MeshSettings {
                mode: MeshMode::Offload,
                priorities: vec![],
                registry_path,
            },
            config,
        );
        mind.attach_mesh(pool.clone());
        mind.start().await.unwrap();
        let lane = mind.heavy().expect("configured pooled lane");
        let mut stack = Self {
            mind,
            lane,
            host_endpoint,
            model,
            host_completions,
            pool,
            supervisor,
            worker_stop,
            worker_addr,
            rpc_addr,
            host_stop,
        };
        stack
            .set_mode(LendMode::Play { until: None }, LendState::Lending)
            .await;
        assert!(
            wait_for(
                || stack.lane.status().state == HeavyState::Running,
                Duration::from_secs(18)
            )
            .await,
            "pooled status: {:?}",
            stack.lane.status()
        );
        let address: SocketAddr = fs::read_to_string(stack.model.with_extension("rpc"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(address.ip().is_loopback());
        assert_ne!(address, stack.worker_addr);
        assert_ne!(address, stack.rpc_addr);
        // Probe the exact address passed to --rpc, through the live pinned TLS upgrade.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut tunnel = tokio::net::TcpStream::connect(address).await.unwrap();
        tunnel.write_all(b"C8").await.unwrap();
        let mut echo = [0; 2];
        timeout(Duration::from_secs(2), tunnel.read_exact(&mut echo))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&echo, b"C8");
        stack
    }

    async fn set_mode(&mut self, mode: LendMode, expected: LendState) {
        let reply = self.pool.set_lend(WORKER, mode.clone()).await.unwrap();
        assert_eq!(reply.compute.unwrap().mode, mode);
        assert_eq!(self.supervisor.tick(None).await, expected);
    }

    pub async fn pause(&mut self) {
        timeout(Duration::from_secs(5), async {
            self.set_mode(LendMode::Pause, LendState::Paused).await;
            assert!(
                wait_for(
                    || matches!(self.lane.status().state, HeavyState::Stopped { .. }),
                    Duration::from_secs(5)
                )
                .await
            );
        })
        .await
        .expect("worker pause must stop the pooled lane within five seconds");
        assert!(self.lane.endpoint().is_none());
    }
}
