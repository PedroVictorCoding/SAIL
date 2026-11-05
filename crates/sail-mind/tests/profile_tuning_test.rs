use sail_mind::{
    InferenceProfileManager, LocalModelConfig,
    protocol::{ComputeBackend, HardwareDiagnostics},
};
use std::path::PathBuf;

#[test]
fn launch_explicitly_enforces_flash_attention_for_all_backends() {
    for backend in [
        ComputeBackend::Cpu,
        ComputeBackend::Vulkan,
        ComputeBackend::Hybrid,
    ] {
        for enabled in [false, true] {
            let config = LocalModelConfig {
                compute: backend,
                flash_attn: enabled,
                cache_type_k: "f16".into(),
                cache_type_v: "f16".into(),
                ..Default::default()
            };
            let args = config.launch_args(1).unwrap();
            let flags: Vec<_> = args.windows(2).filter(|pair| pair[0] == "-fa").collect();
            assert_eq!(
                flags.len(),
                1,
                "missing or ambiguous Flash Attention setting"
            );
            assert_eq!(flags[0][1], if enabled { "on" } else { "off" });
        }
    }
}

#[test]
fn cpu_tuned_profile_decouples_batch_threads_and_sets_optimal_defaults() {
    let profile = InferenceProfileManager::cpu_tuned("Qwen3.6-35B");
    assert_eq!(profile.backend, ComputeBackend::Cpu);
    assert_eq!(profile.cache_type_k, "q4_0");
    assert_eq!(profile.cache_type_v, "q4_0");
    assert!(profile.flash_attn);
    assert_eq!(profile.cache_reuse, 256);
    assert_eq!(profile.context_tokens, 16384);
    assert_eq!(profile.batch_size, 2048);
    assert_eq!(profile.ubatch_size, 512);

    // Generation threads <= 5 to leave headroom for voice audio workers
    assert!(profile.threads <= 5);
    // Prefill batch threads can scale up to utilize CPU hyperthreads
    assert!(profile.threads_batch >= profile.threads);
}

#[test]
fn cpu_baseline_fallback_uses_conservative_safe_settings() {
    let fallback = InferenceProfileManager::cpu_baseline_fallback("Qwen3.6-35B");
    assert_eq!(fallback.backend, ComputeBackend::Cpu);
    assert_eq!(fallback.cache_type_k, "f16");
    assert_eq!(fallback.cache_type_v, "f16");
    assert!(!fallback.flash_attn);
    assert_eq!(fallback.cache_reuse, 0);
    assert_eq!(fallback.context_tokens, 8192);
    assert_eq!(fallback.batch_size, 512);
    assert_eq!(fallback.ubatch_size, 512);
}

#[test]
fn hybrid_and_vulkan_tuned_profiles_clamp_layers_properly() {
    let hybrid = InferenceProfileManager::hybrid_tuned("Qwen3.6-35B", "Vulkan0", 16);
    assert_eq!(hybrid.backend, ComputeBackend::Hybrid);
    assert_eq!(hybrid.device, "Vulkan0");
    assert_eq!(hybrid.gpu_layers, 16);

    let vulkan = InferenceProfileManager::vulkan_tuned("Qwen3.6-35B", "Vulkan0", 64);
    assert_eq!(vulkan.backend, ComputeBackend::Vulkan);
    assert_eq!(vulkan.gpu_layers, 64);
}

#[test]
fn profile_resolution_falls_back_to_cpu_when_render_node_inaccessible() {
    let hw = HardwareDiagnostics {
        cpu_cores: 6,
        cpu_threads: 12,
        total_ram_mb: 32000,
        free_ram_mb: 16000,
        gpu_detected: true,
        gpu_name: Some("AMD Radeon RX 5500".into()),
        render_device_accessible: false,
        permission_remediation: Some(
            "Add user to render group: sudo usermod -a -G render,video main".into(),
        ),
        supported_devices: vec!["OpenBLAS".into()],
    };

    let requested = InferenceProfileManager::vulkan_tuned("Qwen3.6-35B", "Vulkan0", 16);
    let (resolved, fallback_reason) =
        InferenceProfileManager::resolve_profile(Some(&requested), &hw, "Qwen3.6-35B");

    assert_eq!(resolved.backend, ComputeBackend::Cpu);
    assert!(fallback_reason.is_some());
    let reason = fallback_reason.unwrap();
    assert!(reason.contains("render device is inaccessible"));
    assert!(reason.contains("Falling back to tuned CPU profile"));
}

#[test]
fn profile_resolution_clamps_ram_when_headroom_is_low() {
    let hw = HardwareDiagnostics {
        cpu_cores: 6,
        cpu_threads: 12,
        total_ram_mb: 32000,
        free_ram_mb: 1200, // < 2GB free
        gpu_detected: false,
        gpu_name: None,
        render_device_accessible: false,
        permission_remediation: None,
        supported_devices: vec!["OpenBLAS".into()],
    };

    let mut requested = InferenceProfileManager::cpu_tuned("Qwen3.6-35B");
    requested.context_tokens = 32768;
    requested.mlock = true;

    let (resolved, _) =
        InferenceProfileManager::resolve_profile(Some(&requested), &hw, "Qwen3.6-35B");

    assert_eq!(resolved.context_tokens, 8192);
    assert!(!resolved.mlock);
}

#[test]
fn config_conversion_to_and_from_profile_preserves_tuning() {
    let profile = InferenceProfileManager::cpu_tuned("Qwen3.6-35B");
    let config = LocalModelConfig::from_profile(
        &profile,
        PathBuf::from("/srv/sail/models/model.gguf"),
        PathBuf::from("/srv/sail/llama-server"),
        "http://127.0.0.1:8081/v1/chat/completions".into(),
    );

    assert_eq!(config.threads, profile.threads);
    assert_eq!(config.threads_batch, Some(profile.threads_batch));
    assert_eq!(config.batch_size, profile.batch_size);
    assert_eq!(config.ubatch_size, profile.ubatch_size);
    assert_eq!(config.cache_type_k, profile.cache_type_k);
    assert_eq!(config.cache_type_v, profile.cache_type_v);
    assert_eq!(config.flash_attn, profile.flash_attn);
    assert_eq!(config.cache_reuse, profile.cache_reuse);

    let roundtrip = config.to_profile();
    assert_eq!(roundtrip.threads, profile.threads);
    assert_eq!(roundtrip.threads_batch, profile.threads_batch);
    assert_eq!(roundtrip.batch_size, profile.batch_size);
    assert_eq!(roundtrip.ubatch_size, profile.ubatch_size);
    assert_eq!(roundtrip.cache_type_k, profile.cache_type_k);
    assert_eq!(roundtrip.cache_type_v, profile.cache_type_v);
}
