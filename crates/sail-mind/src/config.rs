use sail_protocol::*;
use std::{net::IpAddr, path::PathBuf, time::Duration};

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
    pub threads: usize,
    pub context_tokens: usize,
    pub startup_timeout: Duration,
    pub probe_timeout: Duration,
}
impl Default for LocalModelConfig {
    fn default() -> Self {
        let endpoint = std::env::var("SAIL_LLM_URL")
            .unwrap_or("http://127.0.0.1:8081/v1/chat/completions".into());
        let managed = std::env::var("SAIL_LLM_URL").is_err();
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
            model_path: std::env::var("SAIL_MODEL_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    let candidates = [
                        PathBuf::from("models/Qwen3.6-35B-A3B-Q4_K_M.gguf"),
                        PathBuf::from("/home/main/SAIL/models/Qwen3.6-35B-A3B-Q4_K_M.gguf"),
                        PathBuf::from("models/Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf"),
                        PathBuf::from("/home/main/SAIL/models/Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf"),
                        PathBuf::from("models/gemma4-v2-Q4_K_M.gguf"),
                    ];
                    for cand in candidates {
                        if cand.is_file() {
                            return cand;
                        }
                    }
                    PathBuf::from("models/Qwen3.6-35B-A3B-Q4_K_M.gguf")
                }),
            server_binary: std::env::var("SAIL_LLAMA_SERVER")
                .map(PathBuf::from)
                .unwrap_or("/home/main/llama.cpp/build/bin/llama-server".into()),
            endpoint,
            projector: std::env::var("SAIL_MMPROJ")
                .map(PathBuf::from)
                .ok(),
            managed,
            compute: ComputeBackend::Cpu,
            device: "Vulkan0".into(),
            gpu_layers: 8,
            threads: std::thread::available_parallelism()
                .map(|n| n.get().min(6))
                .unwrap_or(1),
            context_tokens: 8192,
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
            _ => {
                return Err(InferenceError::new(
                    ErrorCode::InvalidConfiguration,
                    "SAIL_MODEL_BACKEND must be cpu or vulkan",
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
        if let Ok(v) = std::env::var("SAIL_VULKAN_DEVICE") {
            config.device = v;
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
            || !(512..=65536).contains(&self.context_tokens)
            || self.gpu_layers > 128
            || self.startup_timeout.is_zero()
            || self.probe_timeout.is_zero()
        {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfiguration,
                "Invalid model name, CPU thread/context/GPU budget or deadline",
            ));
        }
        if self.compute == ComputeBackend::Vulkan
            && (!self.device.starts_with("Vulkan")
                || self.device.chars().any(char::is_whitespace)
                || self.gpu_layers == 0)
        {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfiguration,
                "Vulkan requires a named Vulkan device and nonzero bounded GPU layers",
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
    pub fn launch_args(&self, parallel: usize) -> Result<Vec<String>> {
        let url = self.validate()?;
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
            self.threads.to_string(),
            "--ctx-size".into(),
            (self.context_tokens * parallel).to_string(),
            "--parallel".into(),
            parallel.to_string(),
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
            ComputeBackend::Vulkan => args.extend([
                "--device".into(),
                self.device.clone(),
                "--gpu-layers".into(),
                self.gpu_layers.to_string(),
            ]),
        }
        if let Some(projector) = &self.projector
            && projector.is_file()
        {
            args.extend(["--mmproj".into(), projector.display().to_string()]);
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
