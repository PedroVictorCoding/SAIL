//! Pooled lane lifecycle against a pinned loopback worker and a fake llama-server.
#![cfg(target_os = "linux")]

use std::{
    fs::{self, File},
    io::Write,
    net::TcpListener,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use sail_common::resources::{AdmissionPolicy, HostProbe, ResourceScheduler};
use sail_mesh::{
    Registry, WorkerRecord,
    compute::{
        supervisor::{ComputeConfig, ComputeSupervisor},
        tunnel::LendGate,
    },
    identity::WorkerIdentity,
    rest::RestGate,
    worker::{WorkerConfig, serve_with_compute},
};
use sail_mind::{
    HardwareProbe, LocalModelConfig,
    heavy::{
        admission::HostCaps,
        lane::{HeavyConfig, HeavyLane},
    },
    mesh::{MeshPool, MeshSettings},
};
use sail_protocol::{
    mesh::{HeavyState, LendMode, LendState, LendWindow, MeshMode},
    resources::HostSample,
};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

const GIB: u64 = 1 << 30;

struct FixtureHost;

impl HostProbe for FixtureHost {
    fn sample(&self) -> std::io::Result<HostSample> {
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

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn string(buffer: &mut Vec<u8>, value: &str) {
    buffer.extend((value.len() as u64).to_le_bytes());
    buffer.extend(value.as_bytes());
}

fn model_file(path: &Path, host_bytes: u64) {
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
    file.set_len(header.len() as u64 + host_bytes + 32).unwrap();
}

struct Fixture {
    lane: Arc<HeavyLane>,
    supervisor: ComputeSupervisor,
    lifecycle: CancellationToken,
    port: u16,
    model_path: PathBuf,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new(stem: &str, host_bytes: u64, ram_bytes: u64) -> Self {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = tempfile::tempdir().unwrap();
        let identity = WorkerIdentity::create(dir.path(), "pooled-test").unwrap();
        let rpc_port = free_port();
        let mut supervisor = ComputeSupervisor::new(
            ComputeConfig {
                rpc_server: manifest.join("../sail-mesh/tests/fixtures/fake-ggml-rpc-server.py"),
                cache_dir: dir.path().join("cache"),
                devices: vec!["CPU".into()],
                threads: 1,
                window: LendWindow {
                    start: "00:00".into(),
                    end: "00:00".into(),
                },
                lend_vram_bytes: 0,
                lend_ram_bytes: 4 * GIB,
                rpc_port,
            },
            dir.path(),
            LendGate::new(),
            None,
        );
        assert_eq!(supervisor.tick(None).await, LendState::Lending);
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], free_port()));
        let worker = WorkerConfig {
            bind: address,
            upstream: "http://127.0.0.1:9".parse().unwrap(),
            max_parallel: 1,
            gate: RestGate::default(),
        };
        let record = WorkerRecord {
            name: "pooled-test".into(),
            address: address.to_string(),
            fingerprint: identity.fingerprint(),
            secret: identity.secret.clone(),
            serves: Vec::new(),
            enabled: true,
            paired_at: "2026-11-23T00:00:00Z".into(),
        };
        let lifecycle = CancellationToken::new();
        let stop = lifecycle.clone().cancelled_owned();
        let handle = supervisor.handle();
        tokio::spawn(async move {
            serve_with_compute(&identity, &worker, Some(handle), stop)
                .await
                .unwrap();
        });
        timeout(Duration::from_secs(3), async {
            while tokio::net::TcpStream::connect(address).await.is_err() {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let registry_path = Registry::path_in(dir.path());
        let mut registry = Registry::default();
        registry.upsert(record);
        registry.save(&registry_path).unwrap();
        let pool = MeshPool::new(
            MeshSettings {
                mode: MeshMode::Offload,
                priorities: Vec::new(),
                registry_path,
            },
            LocalModelConfig::default(),
        );
        let model_path = dir.path().join(format!("{stem}.gguf"));
        model_file(&model_path, host_bytes);
        let port = free_port();
        let lane = HeavyLane::with_scheduler(
            HeavyConfig {
                model_path: model_path.clone(),
                server_binary: manifest.join("tests/fixtures/fake-pooled-llama-server.py"),
                port,
                context_tokens: 512,
                caps: HostCaps {
                    ram_bytes,
                    vram_bytes: 0,
                },
            },
            pool,
            Arc::new(ResourceScheduler::new(
                Arc::new(FixtureHost),
                AdmissionPolicy::default(),
            )),
        );
        lane.spawn(lifecycle.clone());
        Self {
            lane,
            supervisor,
            lifecycle,
            port,
            model_path,
            _dir: dir,
        }
    }

    async fn wait_for(&self, predicate: impl Fn(&HeavyState) -> bool) {
        timeout(Duration::from_secs(5), async {
            while !predicate(&self.lane.status().state) {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("last state: {:?}", self.lane.status().state));
    }

    fn launches(&self) -> usize {
        fs::read_to_string(self.model_path.with_extension("launches"))
            .map_or(0, |log| log.lines().count())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.lifecycle.cancel();
    }
}

#[tokio::test]
async fn starts_on_lending_and_stops_within_five_seconds_of_pause() {
    let mut fixture = Fixture::new("pooled", 32, 2 * GIB).await;
    fixture
        .wait_for(|state| *state == HeavyState::Running)
        .await;
    let endpoint = fixture.lane.endpoint().unwrap();
    assert_eq!(endpoint.worker, "pooled-test");
    assert_eq!(endpoint.model, "pooled");
    let result: serde_json::Value = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(endpoint.url)
        .json(&serde_json::json!({"messages": []}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["choices"][0]["message"]["content"], "fixture answer");

    fixture.supervisor.set_mode(LendMode::Pause).unwrap();
    assert_eq!(fixture.supervisor.tick(None).await, LendState::Paused);
    timeout(Duration::from_secs(5), async {
        loop {
            if fixture.lane.endpoint().is_none()
                && tokio::net::TcpStream::connect(("127.0.0.1", fixture.port))
                    .await
                    .is_err()
            {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn admission_refuses_before_starting_the_server() {
    let (total_mb, _) = HardwareProbe::read_ram_mb();
    let host_bytes = (total_mb << 20) + GIB;
    let mut fixture = Fixture::new("oversized", host_bytes, host_bytes + 2 * GIB).await;
    fixture
        .wait_for(|state| matches!(state, HeavyState::Failed { .. }))
        .await;
    assert!(fixture.lane.endpoint().is_none());
    assert_eq!(fixture.launches(), 0);
    fixture.supervisor.release().await;
}

#[tokio::test]
async fn crashing_binary_is_attempted_only_once_per_lending_session() {
    let mut fixture = Fixture::new("crash", 32, 2 * GIB).await;
    fixture
        .wait_for(|state| matches!(state, HeavyState::Failed { .. }))
        .await;
    // Cross two discovery intervals while the worker stays Lending.
    sleep(Duration::from_secs(21)).await;
    assert_eq!(fixture.launches(), 1);
    assert!(matches!(
        fixture.lane.status().state,
        HeavyState::Failed { .. }
    ));
    fixture.supervisor.release().await;
}
