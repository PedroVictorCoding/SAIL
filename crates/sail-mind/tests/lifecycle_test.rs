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
