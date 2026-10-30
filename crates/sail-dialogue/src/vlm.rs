use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use sail_common::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelToolCall {
    pub tool: String, // "os:status", "proc:list", "fs:query", "vision:frame"
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelToolResult {
    pub tool: String,
    pub success: bool,
    pub output: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionFrameInput {
    pub width: u32,
    pub height: u32,
    pub format: String, // "rgb", "rgba", "grayscale"
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionAnalysis {
    pub width: u32,
    pub height: u32,
    pub average_luminance: f32,
    pub color_dominant: String,
    pub edge_density: f32,
    pub description: String,
}

pub struct LocalKernelToolBroker;

impl LocalKernelToolBroker {
    /// Execute a kernel tool directly in pure Rust on the host without any external APIs
    pub fn execute(call: &KernelToolCall) -> KernelToolResult {
        match call.tool.as_str() {
            "os:status" => Self::tool_os_status(),
            "proc:list" => Self::tool_proc_list(&call.params),
            "fs:query" => Self::tool_fs_query(&call.params),
            "vision:frame" => Self::tool_vision_frame(&call.params),
            unknown => KernelToolResult {
                tool: unknown.to_string(),
                success: false,
                output: serde_json::json!({ "error": format!("Unknown kernel tool: {unknown}") }),
            },
        }
    }

    fn error(tool: &str, message: &str) -> KernelToolResult {
        KernelToolResult {
            tool: tool.into(),
            success: false,
            output: serde_json::json!({"error":message}),
        }
    }
    /// os:status — Query OS kernel status, load averages, memory info directly from /proc
    fn tool_os_status() -> KernelToolResult {
        let uptime = fs::read_to_string("/proc/uptime")
            .unwrap_or_default()
            .split_whitespace()
            .next()
            .unwrap_or("0")
            .parse::<f64>()
            .unwrap_or(0.0);

        let loadavg = fs::read_to_string("/proc/loadavg")
            .unwrap_or_default()
            .split_whitespace()
            .take(3)
            .map(|s| s.to_string())
            .collect::<Vec<String>>()
            .join(" ");

        let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let mut mem_total_kb = 0u64;
        let mut mem_avail_kb = 0u64;

        for line in meminfo.lines() {
            if line.starts_with("MemTotal:") {
                mem_total_kb = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
            } else if line.starts_with("MemAvailable:") {
                mem_avail_kb = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
            }
        }

        let mem_used_gb = ((mem_total_kb.saturating_sub(mem_avail_kb)) as f64) / (1024.0 * 1024.0);
        let mem_total_gb = (mem_total_kb as f64) / (1024.0 * 1024.0);

        KernelToolResult {
            tool: "os:status".to_string(),
            success: true,
            output: serde_json::json!({
                "os": std::env::consts::OS,
                "uptime_hours": format!("{:.1}", uptime / 3600.0),
                "load_average": loadavg,
                "memory_used_gb": format!("{:.2} / {:.2} GB", mem_used_gb, mem_total_gb),
                "memory_percent": format!("{:.1}%", (mem_used_gb / mem_total_gb.max(1.0)) * 100.0),
                "logical_cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
            }),
        }
    }

    /// proc:list — Inspect active processes from /proc
    fn tool_proc_list(params: &serde_json::Value) -> KernelToolResult {
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .clamp(1, 100) as usize;
        let mut processes = Vec::new();

        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                if let Ok(file_type) = entry.file_type()
                    && file_type.is_dir()
                        && let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() {
                            let cmdline_path = entry.path().join("cmdline");
                            if let Ok(cmdline) = fs::read(cmdline_path) {
                                let name = String::from_utf8_lossy(&cmdline)
                                    .replace('\0', " ")
                                    .trim()
                                    .to_string();
                                if !name.is_empty() {
                                    processes.push(serde_json::json!({
                                        "pid": pid,
                                        "command": name.chars().take(48).collect::<String>(),
                                    }));
                                    if processes.len() >= limit {
                                        break;
                                    }
                                }
                            }
                        }
            }
        }

        KernelToolResult {
            tool: "proc:list".to_string(),
            success: true,
            output: serde_json::json!({
                "count": processes.len(),
                "processes": processes,
            }),
        }
    }

    /// fs:query — Query local file system paths, directory entries, sizes
    fn tool_fs_query(params: &serde_json::Value) -> KernelToolResult {
        let target_path = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        let root = std::env::var("SAIL_TOOL_ROOT").unwrap_or_else(|_| ".".into());
        let root = match fs::canonicalize(root) {
            Ok(p) => p,
            Err(e) => return Self::error("fs:query", &e.to_string()),
        };
        let candidate = Path::new(target_path);
        let candidate = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            root.join(candidate)
        };
        let path = match fs::canonicalize(candidate) {
            Ok(p) if p.starts_with(&root) => p,
            _ => {
                return Self::error(
                    "fs:query",
                    "Path is outside authorized workspace or does not exist",
                );
            }
        };
        if path.is_file() {
            let metadata = fs::metadata(&path).ok();
            return KernelToolResult {
                tool: "fs:query".to_string(),
                success: true,
                output: serde_json::json!({
                    "type": "file",
                    "path": target_path,
                    "size_bytes": metadata.map(|m| m.len()).unwrap_or(0),
                }),
            };
        }

        let mut entries = Vec::new();
        if let Ok(dir_entries) = fs::read_dir(&path) {
            for entry in dir_entries.flatten().take(20) {
                entries.push(serde_json::json!({
                    "name": entry.file_name().to_string_lossy(),
                    "is_dir": entry.file_type().map(|t| t.is_dir()).unwrap_or(false),
                }));
            }
        }

        KernelToolResult {
            tool: "fs:query".to_string(),
            success: true,
            output: serde_json::json!({
                "type": "directory",
                "path": target_path,
                "entries": entries,
            }),
        }
    }

    /// vision:frame — Process local image frame, calculate spatial features and scene stats
    fn tool_vision_frame(params: &serde_json::Value) -> KernelToolResult {
        let frame: VisionFrameInput = match serde_json::from_value(params.clone()) {
            Ok(f) => f,
            Err(e) => return Self::error("vision:frame", &e.to_string()),
        };
        let channels = match frame.format.as_str() {
            "rgb" => 3,
            "rgba" => 4,
            "grayscale" => 1,
            _ => return Self::error("vision:frame", "Unsupported raw pixel format"),
        };
        let expected = (frame.width as usize)
            .checked_mul(frame.height as usize)
            .and_then(|n| n.checked_mul(channels));
        if expected != Some(frame.data.len())
            || frame.data.is_empty()
            || frame.data.len() > 4_000_000
        {
            return Self::error("vision:frame", "Invalid frame dimensions or data length");
        }
        let luma: Vec<f32> = frame
            .data
            .chunks_exact(channels)
            .map(|p| {
                if channels == 1 {
                    p[0] as f32 / 255.
                } else {
                    (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.
                }
            })
            .collect();
        let luminance = luma.iter().sum::<f32>() / luma.len() as f32;
        let edges = luma
            .windows(2)
            .filter(|p| (p[0] - p[1]).abs() > 0.15)
            .count() as f32
            / luma.len() as f32;
        KernelToolResult {
            tool: "vision:frame".into(),
            success: true,
            output: serde_json::json!({"width":frame.width,"height":frame.height,"average_luminance":luminance,"edge_density":edges}),
        }
    }
}

pub struct VoiceVlm {
    runner: sail_mind::GemmaRunner,
}
impl VoiceVlm {
    pub fn new(_model_name: impl Into<String>) -> Self {
        Self {
            runner: sail_mind::GemmaRunner::new(sail_mind::GemmaConfig::default()),
        }
    }
    pub async fn health(&self) -> serde_json::Value {
        self.runner.health().await
    }
    pub async fn stream(
        &self,
        messages: Vec<serde_json::Value>,
        tx: tokio::sync::mpsc::Sender<String>,
    ) -> Result<serde_json::Value> {
        self.runner
            .stream_chat(messages, tool_definitions(), tx)
            .await
    }
    pub async fn reason_and_route(
        &self,
        prompt: &str,
        vision: Option<VisionFrameInput>,
    ) -> Result<String> {
        let content = multimodal_content(prompt, vision)?;
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let result = self
            .stream(
                vec![serde_json::json!({"role":"user","content":content})],
                tx,
            )
            .await;
        drain.abort();
        Ok(result?["content"].as_str().unwrap_or_default().into())
    }
}
pub fn tool_definitions() -> serde_json::Value {
    serde_json::json!([
        {"type":"function","function":{"name":"os_status","description":"Read local OS memory, uptime and load","parameters":{"type":"object","properties":{}}}},
        {"type":"function","function":{"name":"proc_list","description":"List local processes","parameters":{"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":100}}}}},
        {"type":"function","function":{"name":"fs_query","description":"Inspect a path within the authorized workspace","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}},
        {"type":"function","function":{"name":"vision_frame","description":"Measure luminance and edge density of the currently attached image. For semantic questions reason directly from the image input.","parameters":{"type":"object","properties":{}}}}
    ])
}
pub fn multimodal_content(
    prompt: &str,
    vision: Option<VisionFrameInput>,
) -> Result<serde_json::Value> {
    use base64::Engine;
    if let Some(frame) = vision {
        if !matches!(frame.format.as_str(), "image/png" | "image/jpeg")
            || frame.data.len() > 4_000_000
        {
            return Err(sail_common::error::SailError::Config(
                "Vision reasoning requires PNG/JPEG up to 4MB".into(),
            ));
        }
        let image = base64::engine::general_purpose::STANDARD.encode(frame.data);
        Ok(
            serde_json::json!([{"type":"text","text":prompt},{"type":"image_url","image_url":{"url":format!("data:{};base64,{image}",frame.format)}}]),
        )
    } else {
        Ok(serde_json::json!(prompt))
    }
}

pub fn raw_vision_frame(frame: &VisionFrameInput) -> Result<VisionFrameInput> {
    use image::GenericImageView;
    let format = match frame.format.as_str() {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        _ => {
            return Err(sail_common::error::SailError::Config(
                "Unsupported encoded image".into(),
            ));
        }
    };
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(&frame.data), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(64 * 1024 * 1024);
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| sail_common::error::SailError::Config(e.to_string()))?;
    let image = image.thumbnail(640, 480);
    let (width, height) = image.dimensions();
    Ok(VisionFrameInput {
        width,
        height,
        format: "rgb".into(),
        data: image.into_rgb8().into_raw(),
    })
}
