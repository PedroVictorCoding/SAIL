#![cfg(unix)]
use axum::{Json, Router, routing::get};
use sail_mind::{
    LocalModelConfig, backend::InferenceBackend, local::LocalGemmaBackend, protocol::*,
};
use serde_json::json;
use std::{path::PathBuf, time::Duration};
struct Files {
    root: PathBuf,
}
impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn config_script(body: Option<&str>) -> (LocalModelConfig, Files) {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("sail-mind-lifecycle-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let binary = root.join("llama-fixture.sh");
    let model = root.join("model.gguf");
    std::fs::write(&model, b"fixture-not-inference-weights").unwrap();
    let exe = std::env::current_exe()
        .unwrap()
        .display()
        .to_string()
        .replace('\'', "'\"'\"'");
    let script=body.map(str::to_owned).unwrap_or_else(||format!("#!/bin/sh\nif [ \"$1\" = --list-devices ]; then echo 'Available devices: BLAS'; exit 0; fi\nwhile [ $# -gt 0 ]; do if [ \"$1\" = --port ]; then export SAIL_FIXTURE_PORT=\"$2\"; shift 2; else shift; fi; done\nexec '{exe}' --exact managed_model_helper --ignored --nocapture\n"));
    std::fs::write(&binary, script).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    (
        LocalModelConfig {
            endpoint: format!("http://127.0.0.1:{port}/v1/chat/completions"),
            model_path: model,
            server_binary: binary,
            projector: None,
            managed: true,
            probe_timeout: Duration::from_millis(100),
            startup_timeout: Duration::from_secs(3),
            ..Default::default()
        },
        Files { root },
    )
}
#[tokio::test]
#[ignore = "Child-process test fixture; run by managed lifecycle tests"]
async fn managed_model_helper() {
    let port = std::env::var("SAIL_FIXTURE_PORT").expect("fixture port");
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let app = Router::new()
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route(
            "/props",
            get(|| async {
                Json(json!({"n_ctx":8192,"modalities":{"vision":false,"tools":true}}))
            }),
        );
    eprintln!("managed Rust model fixture ready");
    axum::serve(listener, app).await.unwrap();
}
#[tokio::test]
async fn owned_process_readiness_crash_recovery_and_shutdown_are_real() {
    let (config, _files) = config_script(None);
    let backend = LocalGemmaBackend::new(config, 1).unwrap();
    let first = backend.start().await.unwrap();
    assert_eq!(first.state, ModelState::Ready);
    assert!(first.owned_process);
    let pid = first.process_id.unwrap();
    let killed = tokio::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .await
        .unwrap();
    assert!(killed.success());
    tokio::time::sleep(Duration::from_millis(30)).await;
    let second = backend.start().await.unwrap();
    assert_ne!(second.process_id, Some(pid));
    assert!(second.owned_process);
    backend.shutdown().await.unwrap();
    let status = backend.status().await;
    assert_eq!(status.state, ModelState::Stopped);
    assert!(!status.owned_process);
}
#[tokio::test]
async fn startup_crashes_timeouts_and_missing_vulkan_are_explicit() {
    let (config, _files) = config_script(Some(
        "#!/bin/sh\necho 'intentional load failure' >&2\nexit 3\n",
    ));
    let backend = LocalGemmaBackend::new(config, 1).unwrap();
    assert_eq!(
        backend.start().await.unwrap_err().code,
        ErrorCode::BackendUnavailable
    );
    backend.shutdown().await.unwrap();
    let (mut config, _files) = config_script(Some("#!/bin/sh\nexec sleep 30\n"));
    config.startup_timeout = Duration::from_millis(150);
    let backend = LocalGemmaBackend::new(config, 1).unwrap();
    assert_eq!(
        backend.start().await.unwrap_err().code,
        ErrorCode::BackendUnavailable
    );
    assert!(!backend.status().await.owned_process);
    let (mut config, _files) = config_script(None);
    config.compute = ComputeBackend::Vulkan;
    let backend = LocalGemmaBackend::new(config, 1).unwrap();
    assert_eq!(
        backend.start().await.unwrap_err().code,
        ErrorCode::UnsupportedCapability
    );
    assert!(!backend.status().await.owned_process);
}
#[tokio::test]
async fn shutdown_cancels_in_progress_startup_without_waiting_for_loading_deadline() {
    let (config, _files) = config_script(Some("#!/bin/sh\nexec sleep 30\n"));
    let service = sail_mind::MindService::new(config, Default::default()).unwrap();
    let starting = tokio::spawn({
        let service = service.clone();
        async move { service.start().await }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    tokio::time::timeout(Duration::from_secs(1), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        starting.await.unwrap().unwrap_err().code,
        ErrorCode::ShuttingDown
    );
    assert!(!service.status().await.accepting_requests);
}

#[tokio::test]
async fn low_host_memory_prevents_managed_model_process_launch() {
    struct LowMemory;
    impl sail_common::resources::HostProbe for LowMemory {
        fn sample(&self) -> std::io::Result<sail_protocol::resources::HostSample> {
            Ok(sail_protocol::resources::HostSample {
                total_ram_bytes: 32 * sail_common::resources::GIB,
                available_ram_bytes: sail_common::resources::GIB,
                swap_used_bytes: 0,
                swap_total_bytes: 0,
                logical_cpus: 12,
                memory_pressure_avg10: None,
            })
        }
    }
    let (mut config, files) = config_script(None);
    let marker = files.root.join("launched");
    std::fs::write(
        &config.server_binary,
        format!("#!/bin/sh\ntouch '{}'\nexec sleep 30\n", marker.display()),
    )
    .unwrap();
    config.compute = ComputeBackend::Cpu;
    let scheduler = std::sync::Arc::new(sail_common::resources::ResourceScheduler::new(
        std::sync::Arc::new(LowMemory),
        Default::default(),
    ));
    let backend = LocalGemmaBackend::with_scheduler(config, 1, scheduler.clone()).unwrap();
    assert_eq!(
        backend.start().await.unwrap_err().code,
        ErrorCode::ResourceLimit
    );
    assert!(
        !marker.exists(),
        "rejected model must not execute its binary"
    );
    assert_eq!(scheduler.status().pending_ram_bytes, 0);
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn quantized_kv_cache_calculation_fits_qwen_16k_on_32gb_host() {
    struct ReferenceHostMemory;
    impl sail_common::resources::HostProbe for ReferenceHostMemory {
        fn sample(&self) -> std::io::Result<sail_protocol::resources::HostSample> {
            Ok(sail_protocol::resources::HostSample {
                total_ram_bytes: 32 * sail_common::resources::GIB,
                available_ram_bytes: 24_900_000_000, // ~24.9 GB available on 32GB host
                swap_used_bytes: 0,
                swap_total_bytes: 0,
                logical_cpus: 12,
                memory_pressure_avg10: Some(0.0),
            })
        }
    }
    let (mut config, files) = config_script(None);
    // Simulate ~20.42 GB weights file (Qwen3.6-35B-A3B)
    let weights_size: u64 = 20_419_565_568;
    std::fs::File::create(&config.model_path)
        .unwrap()
        .set_len(weights_size)
        .unwrap();

    config.context_tokens = 16384;
    config.cache_type_k = "q4_0".into();
    config.cache_type_v = "q4_0".into();

    assert_eq!(config.kv_cache_bytes_per_token(), 64 * 1024);
    let est = config.estimated_ram_bytes(1).unwrap();
    // 20.42 GB weights + 512 MB buffer + 16384 * 64 KiB = ~22.03 GB
    assert!(
        est < 22_500_000_000,
        "Estimated RAM for 16k context with q4_0 KV must be ~22.0 GB, got {est}"
    );

    let marker = files.root.join("launched");
    std::fs::write(
        &config.server_binary,
        format!("#!/bin/sh\ntouch '{}'\nexec sleep 30\n", marker.display()),
    )
    .unwrap();
    config.compute = ComputeBackend::Cpu;
    let scheduler = std::sync::Arc::new(sail_common::resources::ResourceScheduler::new(
        std::sync::Arc::new(ReferenceHostMemory),
        Default::default(),
    ));
    let backend = LocalGemmaBackend::with_scheduler(config, 1, scheduler.clone()).unwrap();

    let started = tokio::select! {
        res = backend.start() => res.is_ok(),
        _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
            marker.exists()
        }
    };
    assert!(
        started,
        "Model must be admitted and launched on reference 32GB host"
    );
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn constrained_memory_adapts_context_window_and_admits() {
    struct TightMemory;
    impl sail_common::resources::HostProbe for TightMemory {
        fn sample(&self) -> std::io::Result<sail_protocol::resources::HostSample> {
            Ok(sail_protocol::resources::HostSample {
                total_ram_bytes: 32 * sail_common::resources::GIB,
                // Exactly 23.5 GB available: enough for ~20.42 GB weights + 512 MB buffer + 2 GB headroom + ~500 MB KV
                available_ram_bytes: 23_500_000_000,
                swap_used_bytes: 0,
                swap_total_bytes: 0,
                logical_cpus: 12,
                memory_pressure_avg10: Some(0.0),
            })
        }
    }
    let (mut config, files) = config_script(None);
    let weights_size: u64 = 20_419_565_568;
    std::fs::File::create(&config.model_path)
        .unwrap()
        .set_len(weights_size)
        .unwrap();

    config.context_tokens = 16384;
    config.cache_type_k = "q4_0".into();
    config.cache_type_v = "q4_0".into();

    let marker = files.root.join("launched");
    std::fs::write(
        &config.server_binary,
        format!("#!/bin/sh\ntouch '{}'\nexec sleep 30\n", marker.display()),
    )
    .unwrap();
    config.compute = ComputeBackend::Cpu;
    let scheduler = std::sync::Arc::new(sail_common::resources::ResourceScheduler::new(
        std::sync::Arc::new(TightMemory),
        Default::default(),
    ));
    let backend = LocalGemmaBackend::with_scheduler(config, 1, scheduler.clone()).unwrap();

    let started = tokio::select! {
        res = backend.start() => res.is_ok(),
        _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
            marker.exists()
        }
    };
    assert!(
        started,
        "Model must adapt context window and be admitted when host RAM is tight"
    );
    backend.shutdown().await.unwrap();
}

#[tokio::test]
async fn persistent_model_outlives_shutdown_is_adopted_and_replaced_when_settings_change() {
    use sail_mind::persist::{PersistedServer, record_path};
    let (mut config, _files) = config_script(None);
    config.persist = true;
    let port = reqwest::Url::parse(&config.endpoint)
        .unwrap()
        .port()
        .unwrap();
    let record_file = record_path(port);

    // First run launches the server and detaches from it on shutdown.
    let first = LocalGemmaBackend::new(config.clone(), 1).unwrap();
    let pid = first.start().await.unwrap().process_id.unwrap();
    first.shutdown().await.unwrap();
    drop(first);
    let record = PersistedServer::read(&record_file).expect("warm server recorded");
    assert_eq!(record.pid, pid);
    assert!(
        record.is_alive(),
        "shutdown must leave a persistent model running"
    );

    // Next run with identical settings adopts it instead of reloading.
    let second = LocalGemmaBackend::new(config.clone(), 1).unwrap();
    let adopted = second.start().await.unwrap();
    assert_eq!(adopted.state, ModelState::Ready);
    assert!(!adopted.owned_process);
    assert!(record.is_alive());
    drop(second);

    // Changed launch settings replace the warm server.
    config.threads += 1;
    let third = LocalGemmaBackend::new(config, 1).unwrap();
    let replaced = third.start().await.unwrap();
    assert!(!record.is_alive(), "stale warm server must be stopped");
    assert!(replaced.owned_process);
    assert_ne!(replaced.process_id, Some(pid));
    let fresh = PersistedServer::read(&record_file).unwrap();
    assert_eq!(Some(fresh.pid), replaced.process_id);

    third.shutdown().await.unwrap();
    fresh.terminate(Duration::from_secs(5)).await;
    assert!(!fresh.is_alive());
    let _ = std::fs::remove_file(record_file);
}
