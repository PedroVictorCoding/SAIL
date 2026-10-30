use sail_mind::{GemmaConfig, GemmaRunner, LocalModelConfig, protocol::ComputeBackend};
use serde_json::json;
#[tokio::test]
async fn voice_rejects_nonlocal_and_credentialed_inference_endpoints() {
    for endpoint in [
        "https://example.com/v1/chat/completions",
        "http://127.0.0.1:8080/v1/chat/completions?forward=cloud",
        "http://user:secret@localhost:8080/v1/chat/completions",
    ] {
        let runner = GemmaRunner::new(GemmaConfig {
            server_url: endpoint.into(),
            ..Default::default()
        });
        let (tx, _) = tokio::sync::mpsc::channel(1);
        assert!(
            runner
                .stream_chat(vec![], json!([]), tx)
                .await
                .unwrap_err()
                .to_string()
                .contains("loopback")
        );
    }
}
#[test]
fn cpu_and_vulkan_launch_profiles_are_explicit_and_bounded() {
    let cpu = LocalModelConfig::default().launch_args(1).unwrap();
    assert!(cpu.windows(2).any(|p| p == ["--device", "none"]));
    assert!(cpu.windows(2).any(|p| p == ["--gpu-layers", "0"]));
    assert!(cpu.windows(2).any(|p| p == ["--parallel", "1"]));
    let gpu = LocalModelConfig {
        compute: ComputeBackend::Vulkan,
        device: "Vulkan0".into(),
        gpu_layers: 8,
        ..Default::default()
    }
    .launch_args(1)
    .unwrap();
    assert!(gpu.windows(2).any(|p| p == ["--device", "Vulkan0"]));
    assert!(gpu.windows(2).any(|p| p == ["--gpu-layers", "8"]));
    assert!(
        LocalModelConfig {
            threads: 0,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        LocalModelConfig {
            compute: ComputeBackend::Vulkan,
            device: "cuda0".into(),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
}
