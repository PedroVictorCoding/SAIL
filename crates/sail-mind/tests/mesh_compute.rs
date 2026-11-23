//! MeshPool compute status and `set_lend` against a real loopback TLS worker.
use sail_mesh::{
    Registry, WorkerRecord,
    compute::{
        ComputeHandle,
        supervisor::{ComputeConfig, ComputeSupervisor},
        tunnel::LendGate,
    },
    identity::WorkerIdentity,
    rest::RestGate,
    worker::{WorkerConfig, parse_upstream, serve_with_compute},
};
use sail_mind::{
    LocalModelConfig,
    mesh::{MeshOpError, MeshPool, MeshSettings},
};
use sail_protocol::{
    mesh::{LendMode, LendState, LendWindow, MeshCompute, MeshMode},
    resources::WorkPriority,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::oneshot;

struct Worker {
    identity: WorkerIdentity,
    address: String,
    _dir: tempfile::TempDir,
    stop: Option<oneshot::Sender<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("free port")
        .local_addr()
        .expect("free port address")
        .port()
}

fn always() -> LendWindow {
    LendWindow {
        start: "00:00".into(),
        end: "00:00".into(),
    }
}

fn rpc_fixture() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../sail-mesh/tests/fixtures/fake-ggml-rpc-server.py");
    assert!(path.is_file(), "missing {}", path.display());
    path
}

async fn start_worker(dir: tempfile::TempDir, compute: Option<ComputeHandle>) -> Worker {
    let identity = WorkerIdentity::create(dir.path(), "desk").expect("identity");
    let port = free_port();
    let address = format!("127.0.0.1:{port}");
    let config = WorkerConfig {
        bind: ([127, 0, 0, 1], port).into(),
        upstream: parse_upstream("http://127.0.0.1:9").expect("upstream"),
        max_parallel: 1,
        gate: RestGate::default(),
    };
    let (stop, stopped) = oneshot::channel::<()>();
    let served = WorkerIdentity::load(dir.path()).expect("load identity");
    tokio::spawn(async move {
        let _ = serve_with_compute(&served, &config, compute, async {
            let _ = stopped.await;
        })
        .await;
    });
    for _ in 0..300 {
        if tokio::net::TcpStream::connect(&address).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Worker {
        identity,
        address,
        _dir: dir,
        stop: Some(stop),
    }
}

fn paired(worker: &Worker) -> WorkerRecord {
    let code = worker.identity.pairing_code(&worker.address);
    WorkerRecord::from_code(&code, Vec::new(), "2026-11-23T00:00:00Z".into())
}

fn pool(worker: &Worker) -> (Arc<MeshPool>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("registry dir");
    let registry = Registry::path_in(dir.path());
    let mut reg = Registry::default();
    reg.upsert(paired(worker));
    reg.save(&registry).expect("save registry");
    let pool = MeshPool::new(
        MeshSettings {
            mode: MeshMode::Offload,
            priorities: vec![WorkPriority::Background],
            registry_path: registry,
        },
        LocalModelConfig::default(),
    );
    (pool, dir)
}

fn compute(handle: &ComputeHandle) -> MeshCompute {
    handle.snapshot()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_carries_compute_and_set_lend_round_trips() {
    let dir = tempfile::tempdir().expect("worker dir");
    let gate = LendGate::new();
    let handle = ComputeHandle::new(
        gate,
        free_port(),
        dir.path(),
        LendWindow {
            start: "01:00".into(),
            end: "08:00".into(),
        },
        vec![sail_protocol::mesh::ComputeDevice {
            name: "CPU".into(),
            description: "test CPU".into(),
            lend_bytes: 1024,
        }],
    );
    let worker = start_worker(dir, Some(handle.clone())).await;
    let (pool, _registry) = pool(&worker);
    pool.refresh().await;
    let status = pool.status().workers.into_iter().next().expect("worker");
    let reported = status.compute.expect("compute from hello");
    assert_eq!(reported.state, LendState::Loading);
    assert_eq!(reported.mode, LendMode::Auto);
    assert_eq!(reported.devices.len(), 1);
    assert_eq!(reported.devices[0].name, "CPU");
    assert!(pool.lending_worker().is_none(), "Loading is not Lending");

    let after = pool
        .set_lend("desk", LendMode::Pause)
        .await
        .expect("set lend");
    let snapshot = after.compute.expect("post response carries compute");
    assert_eq!(snapshot.mode, LendMode::Pause);
    assert_eq!(
        compute(&handle).mode,
        LendMode::Pause,
        "the worker saved the new mode"
    );

    pool.refresh().await;
    let status = pool.status().workers.into_iter().next().expect("worker");
    assert_eq!(status.compute.expect("refreshed").mode, LendMode::Pause);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lending_worker_is_selected_and_cleared_after_pause() {
    let dir = tempfile::tempdir().expect("worker dir");
    let gate = LendGate::new();
    let mut supervisor = ComputeSupervisor::new(
        ComputeConfig {
            rpc_server: rpc_fixture(),
            cache_dir: dir.path().join("cache"),
            devices: vec!["CPU".into()],
            threads: 1,
            window: always(),
            lend_vram_bytes: 0,
            lend_ram_bytes: 1 << 30,
            rpc_port: free_port(),
        },
        dir.path(),
        gate.clone(),
        None,
    );
    let mut state = LendState::Loading;
    for _ in 0..100 {
        state = supervisor.tick(None).await;
        if state == LendState::Lending {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(state, LendState::Lending);
    let handle = supervisor.handle();
    let worker = start_worker(dir, Some(handle)).await;
    let (pool, _registry) = pool(&worker);
    pool.refresh().await;

    let status = pool.status().workers.into_iter().next().expect("worker");
    assert_eq!(status.compute.expect("compute").state, LendState::Lending);
    let (name, record) = pool.lending_worker().expect("lending worker");
    assert_eq!(name, "desk");
    assert_eq!(record.address, worker.address);

    pool.set_lend("desk", LendMode::Pause).await.expect("pause");
    assert_eq!(supervisor.tick(None).await, LendState::Paused);
    pool.refresh().await;
    assert!(
        pool.lending_worker().is_none(),
        "a paused worker is no longer lending"
    );

    pool.set_lend("desk", LendMode::Auto)
        .await
        .expect("automatic");
    assert_eq!(supervisor.tick(None).await, LendState::Lending);
    pool.refresh().await;
    assert_eq!(
        pool.lending_worker().map(|(name, _)| name).as_deref(),
        Some("desk")
    );

    supervisor.release().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_compute_unknown_and_unreachable_are_reported() {
    let dir = tempfile::tempdir().expect("worker dir");
    let worker = start_worker(dir, None).await;
    let (pool, _registry) = pool(&worker);
    pool.refresh().await;
    let status = pool.status().workers.into_iter().next().expect("worker");
    assert!(status.compute.is_none(), "no --compute means no snapshot");
    assert!(pool.lending_worker().is_none());

    assert!(matches!(
        pool.set_lend("missing", LendMode::Pause).await,
        Err(MeshOpError::NotFound(_))
    ));
    assert!(matches!(
        pool.set_lend("desk", LendMode::Pause).await,
        Err(MeshOpError::NoCompute(_))
    ));

    drop(worker);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        matches!(
            pool.set_lend("desk", LendMode::Pause).await,
            Err(MeshOpError::Unverified(_))
        ),
        "a stopped worker is unreachable"
    );
}
