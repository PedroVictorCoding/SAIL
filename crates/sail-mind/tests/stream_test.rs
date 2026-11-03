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
    assert!(cpu.windows(2).any(|p| p == ["-ctk", "q4_0"]));
    assert!(cpu.windows(2).any(|p| p == ["-ctv", "q4_0"]));
    assert!(cpu.windows(2).any(|p| p == ["-fa", "on"]));
    assert!(cpu.windows(2).any(|p| p == ["--cache-reuse", "256"]));
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

    // Validation caps threads at the CPUs actually available, so use a count this host has.
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let threads = cpus.min(4);
    let hybrid = LocalModelConfig {
        compute: ComputeBackend::Hybrid,
        device: "Vulkan0".into(),
        gpu_layers: 16,
        threads,
        ..Default::default()
    }
    .launch_args(1)
    .unwrap();
    let threads = threads.to_string();
    assert!(hybrid.windows(2).any(|p| p == ["--device", "Vulkan0"]));
    assert!(hybrid.windows(2).any(|p| p == ["--gpu-layers", "16"]));
    assert!(
        hybrid
            .windows(2)
            .any(|p| p == ["--threads", threads.as_str()])
    );
    assert!(
        hybrid
            .windows(2)
            .any(|p| p == ["--threads-batch", threads.as_str()])
    );

    assert!(
        LocalModelConfig {
            threads: cpus + 1,
            ..Default::default()
        }
        .validate()
        .is_err(),
        "more threads than available CPUs must be rejected"
    );

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
    assert!(
        LocalModelConfig {
            compute: ComputeBackend::Hybrid,
            device: "invalid gpu".into(),
            gpu_layers: 4,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        LocalModelConfig {
            compute: ComputeBackend::Hybrid,
            device: "Vulkan0".into(),
            gpu_layers: 0,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
}

#[test]
fn moe_experts_can_stay_in_system_ram_while_attention_runs_on_the_gpu() {
    use sail_mind::config::CpuMoe;
    let all = LocalModelConfig {
        compute: ComputeBackend::Hybrid,
        device: "Vulkan0".into(),
        gpu_layers: 99,
        cpu_moe: CpuMoe::All,
        ..Default::default()
    }
    .launch_args(1)
    .unwrap();
    assert!(all.windows(2).any(|p| p == ["--gpu-layers", "99"]));
    assert!(all.iter().any(|a| a == "--cpu-moe"));
    assert!(!all.iter().any(|a| a == "--n-cpu-moe"));

    let some = LocalModelConfig {
        compute: ComputeBackend::Vulkan,
        device: "Vulkan0".into(),
        gpu_layers: 99,
        cpu_moe: CpuMoe::Layers(30),
        ..Default::default()
    }
    .launch_args(1)
    .unwrap();
    assert!(some.windows(2).any(|p| p == ["--n-cpu-moe", "30"]));

    // Default: no expert flags at all, so dense models and existing setups are unchanged.
    let plain = LocalModelConfig {
        compute: ComputeBackend::Vulkan,
        device: "Vulkan0".into(),
        gpu_layers: 8,
        ..Default::default()
    }
    .launch_args(1)
    .unwrap();
    assert!(!plain.iter().any(|a| a.contains("cpu-moe")));

    // Expert placement only means something when offloading.
    assert!(
        LocalModelConfig {
            cpu_moe: CpuMoe::All,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
}

#[test]
fn cpu_moe_setting_parses_strictly() {
    use sail_mind::config::CpuMoe;
    assert_eq!(CpuMoe::parse("all").unwrap(), CpuMoe::All);
    assert_eq!(CpuMoe::parse(" OFF ").unwrap(), CpuMoe::Off);
    assert_eq!(CpuMoe::parse("0").unwrap(), CpuMoe::Off);
    assert_eq!(CpuMoe::parse("24").unwrap(), CpuMoe::Layers(24));
    for bad in ["-1", "999", "many", "1.5"] {
        assert!(CpuMoe::parse(bad).is_err(), "{bad}");
    }
}
