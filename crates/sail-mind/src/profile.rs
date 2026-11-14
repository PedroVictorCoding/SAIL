use sail_protocol::{ComputeBackend, HardwareDiagnostics, InferenceProfile};

pub struct InferenceProfileManager;

impl InferenceProfileManager {
    /// Return the tuned CPU profile for local inference on multi-core systems (i5-10400).
    /// Decouples generation threads (5) from prefill batch threads (10) to preserve
    /// interactive latency for speech workers while maximizing prompt throughput.
    pub fn cpu_tuned(model: &str) -> InferenceProfile {
        let physical_cores = std::thread::available_parallelism()
            .map(|n| (n.get() / 2).max(1))
            .unwrap_or(4);
        let gen_threads = physical_cores.min(5);
        let batch_threads = (gen_threads * 2).min(12);

        InferenceProfile {
            profile_id: format!("{model}-cpu-tuned"),
            backend: ComputeBackend::Cpu,
            threads: gen_threads,
            threads_batch: batch_threads,
            context_tokens: crate::config::DEFAULT_CONTEXT_TOKENS,
            cache_type_k: "q4_0".into(),
            cache_type_v: "q4_0".into(),
            flash_attn: true,
            cache_reuse: 256,
            batch_size: 2048,
            ubatch_size: 512,
            mlock: false,
            no_mmap: false,
            gpu_layers: 0,
            device: "none".into(),
        }
    }

    /// Return the tuned Hybrid profile offloading initial layers to Vulkan GPU
    /// while running remaining compute on CPU.
    pub fn hybrid_tuned(model: &str, device: &str, gpu_layers: u32) -> InferenceProfile {
        let mut p = Self::cpu_tuned(model);
        p.profile_id = format!("{model}-hybrid-tuned");
        p.backend = ComputeBackend::Hybrid;
        p.device = device.to_string();
        p.gpu_layers = gpu_layers.clamp(1, 32);
        p
    }

    /// Return the tuned Vulkan GPU profile.
    pub fn vulkan_tuned(model: &str, device: &str, gpu_layers: u32) -> InferenceProfile {
        let mut p = Self::cpu_tuned(model);
        p.profile_id = format!("{model}-vulkan-tuned");
        p.backend = ComputeBackend::Vulkan;
        p.device = device.to_string();
        p.gpu_layers = gpu_layers.clamp(1, 128);
        p
    }

    /// Safe, minimal fallback profile when accelerated or quantized configurations fail.
    pub fn cpu_baseline_fallback(model: &str) -> InferenceProfile {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().min(4))
            .unwrap_or(2);

        InferenceProfile {
            profile_id: format!("{model}-cpu-baseline"),
            backend: ComputeBackend::Cpu,
            threads,
            threads_batch: threads,
            context_tokens: 8192,
            cache_type_k: "f16".into(),
            cache_type_v: "f16".into(),
            flash_attn: false,
            cache_reuse: 0,
            batch_size: 512,
            ubatch_size: 512,
            mlock: false,
            no_mmap: false,
            gpu_layers: 0,
            device: "none".into(),
        }
    }

    /// Resolve an optimal profile given requested configuration and hardware diagnostics.
    /// If GPU is requested but device or permissions are missing, gracefully falls back
    /// to the tuned CPU profile and records the justification.
    pub fn resolve_profile(
        requested: Option<&InferenceProfile>,
        hw: &HardwareDiagnostics,
        model: &str,
    ) -> (InferenceProfile, Option<String>) {
        if let Some(req) = requested {
            let mut resolved = req.clone();
            // Check GPU / Vulkan constraints
            if matches!(
                resolved.backend,
                ComputeBackend::Vulkan | ComputeBackend::Hybrid
            ) {
                if !hw.render_device_accessible {
                    let reason = format!(
                        "Requested backend {:?} requires GPU, but render device is inaccessible ({:?}). Falling back to tuned CPU profile.",
                        resolved.backend, hw.permission_remediation
                    );
                    return (Self::cpu_tuned(model), Some(reason));
                }
                if hw.supported_devices.is_empty()
                    || !hw
                        .supported_devices
                        .iter()
                        .any(|d| d.starts_with(&resolved.device))
                {
                    let reason = format!(
                        "Requested GPU device '{}' not available in runtime ({:?}). Falling back to tuned CPU profile.",
                        resolved.device, hw.supported_devices
                    );
                    return (Self::cpu_tuned(model), Some(reason));
                }
            }

            // Check RAM headroom: if free RAM is low (< 2048 MB), clamp context to 8192 tokens and disable mlock
            if hw.free_ram_mb > 0 && hw.free_ram_mb < 2048 {
                resolved.context_tokens = resolved.context_tokens.min(8192);
                resolved.mlock = false;
            }

            (resolved, None)
        } else {
            // Default selection: if Vulkan device is accessible, use Hybrid; otherwise CPU tuned
            let vulkan_available = hw.render_device_accessible
                && hw.supported_devices.iter().any(|d| d.starts_with("Vulkan"));

            if vulkan_available {
                (Self::hybrid_tuned(model, "Vulkan0", 8), None)
            } else {
                (Self::cpu_tuned(model), None)
            }
        }
    }
}
