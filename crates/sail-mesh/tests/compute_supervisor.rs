//! The compute supervisor: window and mode decisions, RPC lifecycle, status.json.
use chrono::Local;
use sail_mesh::compute::{
    STATUS_FILE, StatusFile,
    lend::{self},
    supervisor::{ComputeConfig, ComputeSupervisor},
    tunnel::LendGate,
};
use sail_protocol::mesh::{LendMode, LendState, LendWindow, MeshBusy};
use std::{path::PathBuf, time::Duration};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-ggml-rpc-server.py")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind free port")
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

/// A one-minute window that starts two minutes from now, so "now" is outside it.
fn window_after_now() -> LendWindow {
    let start = Local::now() + chrono::Duration::minutes(2);
    let end = start + chrono::Duration::minutes(1);
    LendWindow {
        start: start.format("%H:%M").to_string(),
        end: end.format("%H:%M").to_string(),
    }
}

fn config(dir: &std::path::Path, window: LendWindow, port: u16) -> ComputeConfig {
    ComputeConfig {
        rpc_server: fixture(),
        cache_dir: dir.join("cache"),
        devices: vec!["Vulkan0".into(), "CPU".into()],
        threads: 2,
        window,
        lend_vram_bytes: 16 * 1024 * 1024 * 1024,
        lend_ram_bytes: 48 * 1024 * 1024 * 1024,
        rpc_port: port,
    }
}

async fn wait_port_closed(port: u16) -> bool {
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

fn read_status(dir: &std::path::Path) -> StatusFile {
    let data = std::fs::read(dir.join(STATUS_FILE)).expect("status.json");
    serde_json::from_slice(&data).expect("status JSON")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lends_starts_the_rpc_server_and_steps_back_on_pause_and_rest() {
    let dir = tempfile::tempdir().expect("data dir");
    lend::save_mode(&dir.path().join(lend::LEND_FILE), &LendMode::Auto).expect("save mode");
    let port = free_port();
    let gate = LendGate::new();
    let mut supervisor = ComputeSupervisor::new(
        config(dir.path(), always(), port),
        dir.path(),
        gate.clone(),
        None,
    );
    assert_eq!(supervisor.tick(None).await, LendState::Lending);
    assert!(gate.lending());
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.state, LendState::Lending);
    assert_eq!(snapshot.build.as_deref(), Some("v9.9.9"));
    let device = snapshot
        .devices
        .iter()
        .find(|device| device.name == "Vulkan0")
        .expect("Vulkan0");
    assert_eq!(device.description, "Fake GPU");
    assert_eq!(device.lend_bytes, 16 * 1024 * 1024 * 1024);
    let cpu = snapshot
        .devices
        .iter()
        .find(|device| device.name == "CPU")
        .expect("CPU");
    assert_eq!(cpu.description, "Fake CPU");
    assert_eq!(cpu.lend_bytes, 48 * 1024 * 1024 * 1024);

    let status = read_status(dir.path());
    let age = chrono::DateTime::parse_from_rfc3339(&status.at)
        .map(|at| chrono::Utc::now().signed_duration_since(at.with_timezone(&chrono::Utc)))
        .expect("status time");
    assert!(age.num_seconds() <= 10, "status is fresh: {age}");
    assert_eq!(status.compute.state, LendState::Lending);

    supervisor.set_mode(LendMode::Pause).expect("pause");
    assert_eq!(supervisor.tick(None).await, LendState::Paused);
    assert!(!gate.lending());
    assert!(wait_port_closed(port).await, "the RPC server stopped");

    supervisor
        .set_mode(LendMode::Play { until: None })
        .expect("play");
    assert_eq!(supervisor.tick(None).await, LendState::Lending);

    let busy = MeshBusy {
        reason: "making music (REAPER)".into(),
        full: true,
        retry_after_secs: None,
    };
    assert_eq!(
        supervisor.tick(Some(&busy)).await,
        LendState::Resting {
            reason: "making music (REAPER)".into(),
            full: true
        }
    );
    assert!(!gate.lending());
    assert!(wait_port_closed(port).await, "a game or music app stops it");

    assert_eq!(supervisor.tick(None).await, LendState::Lending);
    supervisor.release().await;
    assert!(!gate.lending());
    assert!(wait_port_closed(port).await, "release stops it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auto_respects_the_window_and_play_overrides_and_expires_to_auto() {
    let dir = tempfile::tempdir().expect("data dir");
    lend::save_mode(&dir.path().join(lend::LEND_FILE), &LendMode::Auto).expect("save mode");
    let port = free_port();
    let gate = LendGate::new();
    let mut supervisor = ComputeSupervisor::new(
        config(dir.path(), window_after_now(), port),
        dir.path(),
        gate.clone(),
        None,
    );
    match supervisor.tick(None).await {
        LendState::OutsideWindow { next_start } => {
            assert!(chrono::DateTime::parse_from_rfc3339(&next_start).is_ok());
        }
        other => panic!("expected OutsideWindow, got {other:?}"),
    }
    assert!(!gate.lending());
    assert!(supervisor.snapshot().build.is_none(), "nothing was started");

    supervisor
        .set_mode(LendMode::Play { until: None })
        .expect("play");
    assert_eq!(supervisor.tick(None).await, LendState::Lending);

    supervisor
        .set_mode(LendMode::Play {
            until: Some("2020-01-01T00:00:00Z".into()),
        })
        .expect("expired play");
    assert!(matches!(
        supervisor.tick(None).await,
        LendState::OutsideWindow { .. }
    ));
    assert_eq!(
        lend::load_mode(&dir.path().join(lend::LEND_FILE)),
        LendMode::Auto,
        "an expired Play is saved as Auto"
    );
    assert!(!gate.lending());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_rpc_binary_is_unavailable_without_a_spawn_loop() {
    let dir = tempfile::tempdir().expect("data dir");
    lend::save_mode(&dir.path().join(lend::LEND_FILE), &LendMode::Auto).expect("save mode");
    let mut config = config(dir.path(), always(), free_port());
    config.rpc_server = dir.path().join("missing-ggml-rpc-server");
    let gate = LendGate::new();
    let mut supervisor = ComputeSupervisor::new(config, dir.path(), gate.clone(), None);
    match supervisor.tick(None).await {
        LendState::Unavailable { reason } => {
            assert!(reason.contains("cannot start ggml-rpc-server"), "{reason}")
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert!(!gate.lending());
    assert!(matches!(
        supervisor.tick(None).await,
        LendState::Unavailable { .. }
    ));
    assert!(
        read_status(dir.path()).compute.build.is_none(),
        "no build from a binary that never ran"
    );
}
