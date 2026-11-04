use sail_protocol::*;
use std::{net::IpAddr, path::PathBuf, time::Duration};

/// Where a mixture-of-experts model's expert weights live when offloading to the GPU.
///
/// For MoE models such as Qwen3-Coder-30B-A3B or Qwen3.6-35B-A3B, most of the weights are
/// experts and only a few run per token. Keeping all experts in system RAM and putting
/// every layer's attention and KV cache on the GPU (`--gpu-layers 99 --cpu-moe`) uses a
/// small GPU far better than moving the first N whole layers. `Layers(n)` keeps the experts
/// of `n` layers on the CPU (`--n-cpu-moe n`) and the rest on the GPU, for cards with spare
/// VRAM. Measure with `scripts/mind-gpu-setup.sh` before enabling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CpuMoe {
    /// Normal layer offload (dense models, or CPU-only).
    #[default]
    Off,
    /// Every layer's experts in system RAM (`--cpu-moe`).
    All,
    /// Experts of this many layers in system RAM (`--n-cpu-moe N`).
    Layers(u32),
}

impl CpuMoe {
    /// Parse `SAIL_CPU_MOE`: `off`/`0`, `all`, or a layer count (1-256).
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "off" | "0" | "false" | "none" => Ok(CpuMoe::Off),
            "all" | "on" | "true" => Ok(CpuMoe::All),
            n => match n.parse::<u32>() {
                Ok(n) if (1..=256).contains(&n) => Ok(CpuMoe::Layers(n)),
                _ => Err(InferenceError::new(
                    ErrorCode::InvalidConfiguration,
                    "SAIL_CPU_MOE must be off, all, or a layer count from 1 to 256",
                )),
            },
        }
    }

    /// The setting from `SAIL_CPU_MOE`, `Off` when unset.
    pub fn from_env() -> Result<Self> {
        match std::env::var("SAIL_CPU_MOE") {
            Ok(v) => Self::parse(&v),
            Err(_) => Ok(CpuMoe::Off),
        }
    }

    fn args(self) -> Vec<String> {
        match self {
            CpuMoe::Off => vec![],
            CpuMoe::All => vec!["--cpu-moe".into()],
            CpuMoe::Layers(n) => vec!["--n-cpu-moe".into(), n.to_string()],
        }
    }
}

#[derive(Debug, Clone)]
pub struct LocalModelConfig {
    pub name: String,
    pub aliases: Vec<String>,
    pub model_path: PathBuf,
    pub server_binary: PathBuf,
    pub endpoint: String,
    pub projector: Option<PathBuf>,
    pub managed: bool,
    pub compute: ComputeBackend,
    pub device: String,
    pub gpu_layers: u32,
    /// MoE expert placement for Vulkan/Hybrid; see [`CpuMoe`].
    pub cpu_moe: CpuMoe,
    pub threads: usize,
    pub threads_batch: Option<usize>,
    pub context_tokens: usize,
    pub cache_type_k: String,
    pub cache_type_v: String,
    pub flash_attn: bool,
    pub cache_reuse: usize,
    pub batch_size: usize,
    pub ubatch_size: usize,
    pub no_mmap: bool,
    pub mlock: bool,
    pub startup_timeout: Duration,
    pub probe_timeout: Duration,
}
impl Default for LocalModelConfig {
    fn default() -> Self {
        let endpoint = std::env::var("SAIL_LLM_URL")
            .unwrap_or("http://127.0.0.1:8081/v1/chat/completions".into());
        let managed = std::env::var("SAIL_LLM_URL").is_err();
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        Self {
            name: "Qwen3.6-35B-A3B".into(),
            aliases: vec![
                "local".into(),
                "qwen".into(),
                "qwen3.6".into(),
                "qwen3".into(),
                "qwen3.6-35b".into(),
                "gemma4".into(),
                "gemma-4".into(),
            ],
            model_path: sail_common::models::primary_model_path(),
            server_binary: std::env::var("SAIL_LLAMA_SERVER")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    let candidates = [
                        PathBuf::from("/home/main/llama.cpp/build-vulkan/bin/llama-server"),
                        PathBuf::from("/home/main/llama-cpp-turboquant/build/bin/llama-server"),
                        PathBuf::from("/home/main/llama.cpp/build/bin/llama-server"),
                    ];
                    for cand in candidates {
                        if cand.is_file() {
                            return cand;
                        }
                    }
                    PathBuf::from("/home/main/llama.cpp/build/bin/llama-server")
                }),
            endpoint,
            projector: std::env::var("SAIL_MMPROJ").map(PathBuf::from).ok(),
            managed,
            compute: ComputeBackend::Cpu,
            device: "Vulkan0".into(),
            gpu_layers: 8,
            cpu_moe: CpuMoe::Off,
            threads: cpus.min(5),
            threads_batch: None,
            context_tokens: std::env::var("SAIL_CONTEXT_TOKENS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(16384),
            cache_type_k: std::env::var("SAIL_CACHE_TYPE_K").unwrap_or_else(|_| "q4_0".into()),
            cache_type_v: std::env::var("SAIL_CACHE_TYPE_V").unwrap_or_else(|_| "q4_0".into()),
            flash_attn: std::env::var("SAIL_FLASH_ATTN")
                .map(|v| v != "0" && v != "off" && v != "false")
                .unwrap_or(true),
            cache_reuse: std::env::var("SAIL_CACHE_REUSE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(256),
            batch_size: std::env::var("SAIL_BATCH_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(2048),
            ubatch_size: std::env::var("SAIL_UBATCH_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(512),
            no_mmap: std::env::var("SAIL_NO_MMAP")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false),
            mlock: std::env::var("SAIL_MLOCK")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false),
            startup_timeout: Duration::from_secs(120),
            probe_timeout: Duration::from_secs(3),
        }
    }
}
impl LocalModelConfig {
    pub fn from_env() -> Result<Self> {
        let compute = match std::env::var("SAIL_MODEL_BACKEND").as_deref() {
            Ok("cpu") | Err(_) => ComputeBackend::Cpu,
            Ok("vulkan") => ComputeBackend::Vulkan,
            Ok("hybrid") | Ok("gpu+cpu") | Ok("gpu_cpu") => ComputeBackend::Hybrid,
            _ => {
                return Err(InferenceError::new(
                    ErrorCode::InvalidConfiguration,
                    "SAIL_MODEL_BACKEND must be cpu, vulkan, or hybrid",
                ));
            }
        };
        let mut config = Self {
            compute,
            ..Self::default()
        };
        if let Ok(v) = std::env::var("SAIL_MODEL_THREADS") {
            config.threads = v.parse().map_err(|_| {
                InferenceError::new(
                    ErrorCode::InvalidConfiguration,
                    "Invalid SAIL_MODEL_THREADS",
                )
            })?;
        }
        if let Ok(v) = std::env::var("SAIL_GPU_LAYERS") {
            config.gpu_layers = v.parse().map_err(|_| {
                InferenceError::new(ErrorCode::InvalidConfiguration, "Invalid SAIL_GPU_LAYERS")
            })?;
        }
        config.cpu_moe = CpuMoe::from_env()?;
        if let Ok(v) = std::env::var("SAIL_VULKAN_DEVICE") {
            config.device = v;
        }
        if let Ok(v) = std::env::var("SAIL_CACHE_TYPE_K") {
            config.cache_type_k = v;
        }
        if let Ok(v) = std::env::var("SAIL_CACHE_TYPE_V") {
            config.cache_type_v = v;
        }
        if let Ok(v) = std::env::var("SAIL_FLASH_ATTN") {
            config.flash_attn = v != "0" && v != "off" && v != "false";
        }
        if let Ok(v) = std::env::var("SAIL_CACHE_REUSE")
            && let Ok(n) = v.parse()
        {
            config.cache_reuse = n;
        }
        if let Ok(v) = std::env::var("SAIL_MODEL_THREADS_BATCH")
            && let Ok(n) = v.parse()
        {
            config.threads_batch = Some(n);
        }
        if let Ok(v) = std::env::var("SAIL_BATCH_SIZE")
            && let Ok(n) = v.parse()
        {
            config.batch_size = n;
        }
        if let Ok(v) = std::env::var("SAIL_UBATCH_SIZE")
            && let Ok(n) = v.parse()
        {
            config.ubatch_size = n;
        }
        if let Ok(v) = std::env::var("SAIL_NO_MMAP") {
            config.no_mmap = v == "1" || v == "true";
        }
        if let Ok(v) = std::env::var("SAIL_MLOCK") {
            config.mlock = v == "1" || v == "true";
        }
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<reqwest::Url> {
        let url = local_endpoint(&self.endpoint)?;
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        if self.name.is_empty()
            || self.threads == 0
            || self.threads > cpus
            || self.batch_size == 0
            || self.ubatch_size == 0
            || self.ubatch_size > self.batch_size
            || !(512..=65536).contains(&self.context_tokens)
            || self.gpu_layers > 128
            || self.startup_timeout.is_zero()
            || self.probe_timeout.is_zero()
        {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfiguration,
                "Invalid model name, CPU thread/context/batch/GPU budget or deadline",
            ));
        }
        if let Some(tb) = self.threads_batch
            && (tb == 0 || tb > cpus * 2)
        {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfiguration,
                "Invalid threads_batch budget",
            ));
        }
        if (self.compute == ComputeBackend::Vulkan || self.compute == ComputeBackend::Hybrid)
            && (!self.device.starts_with("Vulkan")
                || self.device.chars().any(char::is_whitespace)
                || self.gpu_layers == 0)
        {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfiguration,
                "Vulkan and Hybrid require a named Vulkan device and nonzero bounded GPU layers",
            ));
        }
        if self.compute == ComputeBackend::Cpu && self.cpu_moe != CpuMoe::Off {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfiguration,
                "SAIL_CPU_MOE places MoE experts while offloading to the GPU; it needs SAIL_MODEL_BACKEND=vulkan or hybrid",
            ));
        }
        if self.managed
            && (url.scheme() != "http"
                || !matches!(url.host_str(), Some("127.0.0.1" | "localhost")))
        {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfiguration,
                "Managed llama-server binds HTTP on IPv4 loopback; use external mode for other local configurations",
            ));
        }
        Ok(url)
    }
    pub fn from_profile(
        profile: &InferenceProfile,
        model_path: PathBuf,
        server_binary: PathBuf,
        endpoint: String,
    ) -> Self {
        let managed = local_endpoint(&endpoint)
            .map(|u| matches!(u.host_str(), Some("127.0.0.1" | "localhost")))
            .unwrap_or(true);
        Self {
            name: profile.profile_id.clone(),
            aliases: vec!["local".into()],
            model_path,
            server_binary,
            endpoint,
            projector: None,
            managed,
            compute: profile.backend,
            device: profile.device.clone(),
            gpu_layers: profile.gpu_layers,
            // Profiles do not carry expert placement yet; the operator's setting applies.
            cpu_moe: CpuMoe::from_env().unwrap_or_default(),
            threads: profile.threads,
            threads_batch: Some(profile.threads_batch),
            context_tokens: profile.context_tokens,
            cache_type_k: profile.cache_type_k.clone(),
            cache_type_v: profile.cache_type_v.clone(),
            flash_attn: profile.flash_attn,
            cache_reuse: profile.cache_reuse,
            batch_size: profile.batch_size,
            ubatch_size: profile.ubatch_size,
            no_mmap: profile.no_mmap,
            mlock: profile.mlock,
            startup_timeout: Duration::from_secs(120),
            probe_timeout: Duration::from_secs(3),
        }
    }
    pub fn to_profile(&self) -> InferenceProfile {
        InferenceProfile {
            profile_id: self.name.clone(),
            backend: self.compute,
            threads: self.threads,
            threads_batch: self.threads_batch.unwrap_or(self.threads),
            context_tokens: self.context_tokens,
            cache_type_k: self.cache_type_k.clone(),
            cache_type_v: self.cache_type_v.clone(),
            flash_attn: self.flash_attn,
            cache_reuse: self.cache_reuse,
            batch_size: self.batch_size,
            ubatch_size: self.ubatch_size,
            mlock: self.mlock,
            no_mmap: self.no_mmap,
            gpu_layers: self.gpu_layers,
            device: self.device.clone(),
        }
    }
    /// Estimated memory required per context token in bytes for the KV cache.
    pub fn kv_cache_bytes_per_token(&self) -> u64 {
        let bytes_per_elem = |t: &str| match t.to_ascii_lowercase().as_str() {
            "q4_0" | "q4_1" => 1u64,
            "q8_0" => 2u64,
            "f32" => 4u64,
            _ => 2u64, // f16 / bf16 default
        };
        let k_bytes = bytes_per_elem(&self.cache_type_k);
        let v_bytes = bytes_per_elem(&self.cache_type_v);
        // KV cache stores key and value tensors for each attention layer across KV heads and head dimensions.
        // For Qwen 3.6 35B A3B (40 layers, 2 KV heads, dim 128 = 10,240 elements per K/V tensor per token),
        // q4_0 takes ~11.5 KiB total, f16 takes ~40 KiB total.
        // For larger dense architectures (up to 16 KV heads, 48 layers, dim 128 = ~98k elements),
        // q4_0 takes ~55 KiB, f16 takes ~192 KiB.
        // Using 32 KiB per byte-per-element (i.e. 64 KiB for q4_0/q4_0, 128 KiB for f16/f16) provides
        // a safe, conservative upper bound that fits 16k context comfortably within 32 GB hosts.
        (k_bytes.saturating_add(v_bytes)).saturating_mul(32 * 1024)
    }

    /// Estimated RAM bytes needed to host model weights, projector, working buffers,
    /// and parallel KV cache slots during startup admission.
    pub fn estimated_ram_bytes(&self, parallel: usize) -> std::io::Result<u64> {
        let weights = std::fs::metadata(&self.model_path)?.len();
        let projector = self
            .projector
            .as_ref()
            .map(|p| std::fs::metadata(p).map(|m| m.len()))
            .transpose()?
            .unwrap_or(0);
        let kv_bytes = (self.context_tokens as u64)
            .saturating_mul(parallel.max(1) as u64)
            .saturating_mul(self.kv_cache_bytes_per_token());
        Ok(weights
            .saturating_add(projector)
            .saturating_add(512 * sail_common::resources::MIB)
            .saturating_add(kv_bytes))
    }
    pub fn launch_args(&self, parallel: usize) -> Result<Vec<String>> {
        let url = self.validate()?;
        let threads_batch = self.threads_batch.unwrap_or(self.threads);
        let mut args = vec![
            "-m".into(),
            self.model_path.display().to_string(),
            "--jinja".into(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            url.port_or_known_default().unwrap().to_string(),
            "--threads".into(),
            self.threads.to_string(),
            "--threads-batch".into(),
            threads_batch.to_string(),
            "--ctx-size".into(),
            (self.context_tokens * parallel).to_string(),
            "--parallel".into(),
            parallel.to_string(),
            "--batch-size".into(),
            self.batch_size.to_string(),
            "--ubatch-size".into(),
            self.ubatch_size.to_string(),
            "--image-max-tokens".into(),
            "1024".into(),
        ];
        match self.compute {
            ComputeBackend::Cpu => args.extend([
                "--device".into(),
                "none".into(),
                "--gpu-layers".into(),
                "0".into(),
            ]),
            ComputeBackend::Vulkan | ComputeBackend::Hybrid => {
                args.extend([
                    "--device".into(),
                    self.device.clone(),
                    "--gpu-layers".into(),
                    self.gpu_layers.to_string(),
                ]);
                args.extend(self.cpu_moe.args());
            }
        }
        if let Some(projector) = &self.projector
            && projector.is_file()
        {
            args.extend(["--mmproj".into(), projector.display().to_string()]);
        }
        if !self.cache_type_k.is_empty() {
            args.extend(["-ctk".into(), self.cache_type_k.clone()]);
        }
        if !self.cache_type_v.is_empty() {
            args.extend(["-ctv".into(), self.cache_type_v.clone()]);
        }
        if self.flash_attn {
            args.extend(["-fa".into(), "on".into()]);
        }
        if self.cache_reuse > 0 {
            args.extend(["--cache-reuse".into(), self.cache_reuse.to_string()]);
        }
        if self.no_mmap {
            args.push("--no-mmap".into());
        }
        if self.mlock {
            args.push("--mlock".into());
        }
        Ok(args)
    }
}
pub fn local_endpoint(endpoint: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|e| InferenceError::new(ErrorCode::InvalidConfiguration, e.to_string()))?;
    let local = url.host_str().is_some_and(|h| {
        h == "localhost"
            || h.trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if !local
        || !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/v1/chat/completions"
    {
        return Err(InferenceError::new(
            ErrorCode::ForbiddenEndpoint,
            "Inference requires an explicit loopback /v1/chat/completions endpoint without credentials, query or fragment",
        ));
    }
    Ok(url)
}
