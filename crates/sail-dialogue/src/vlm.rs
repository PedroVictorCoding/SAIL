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
    /// Convert a local tool invocation into a strongly-typed Guardian Capability
    pub fn tool_to_capability(call: &KernelToolCall) -> sail_guardian::Capability {
        match call.tool.as_str() {
            "os:status" => {
                sail_guardian::Capability::System(sail_guardian::SystemCapability::OsTelemetry)
            }
            "proc:list" | "forge:enqueue" | "forge:cancel" | "github:issues" => {
                sail_guardian::capability::bound_tool_invocation(&call.tool, &call.params)
            }
            "fs:query" | "fs:read" => {
                let path_str = call
                    .params
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");
                sail_guardian::Capability::Filesystem(sail_guardian::FsCapability::Read {
                    path: std::path::PathBuf::from(path_str),
                })
            }
            "fs:write" => {
                sail_guardian::capability::bound_tool_invocation("fs:write", &call.params)
            }
            "vision:frame" => {
                sail_guardian::capability::bound_tool_invocation("vision:frame", &call.params)
            }
            cmd => sail_guardian::Capability::Tool(sail_guardian::ToolCapability::KernelTool(
                cmd.to_string(),
            )),
        }
    }

    /// Execute a kernel tool through the mandatory Guardian execution gate.
    /// Fails closed if Guardian is unavailable or if authorization is denied.
    pub fn execute_guarded(
        caller: sail_common::types::SubsystemId,
        call: &KernelToolCall,
        token: Option<uuid::Uuid>,
    ) -> KernelToolResult {
        Self::execute_guarded_for(caller, call, token, None)
    }
    pub fn execute_guarded_for(
        caller: sail_common::types::SubsystemId,
        call: &KernelToolCall,
        token: Option<uuid::Uuid>,
        correlation: Option<uuid::Uuid>,
    ) -> KernelToolResult {
        let broker = match sail_guardian::global_broker() {
            Ok(b) => b,
            Err(e) => {
                return KernelToolResult {
                    tool: call.tool.clone(),
                    success: false,
                    output: serde_json::json!({
                        "error": format!("Guardian execution gate unavailable: {e}"),
                        "denied": true,
                    }),
                };
            }
        };

        let cap = Self::tool_to_capability(call);
        let execution = if call.tool == "fs:query" || call.tool == "fs:read" {
            let path = call
                .params
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or(".");
            broker.authorize_read(caller, Path::new(path), token, correlation, |file| {
                if call.tool == "fs:read" {
                    Self::tool_fs_read(file)
                } else {
                    Self::tool_fs_query(file)
                }
            })
        } else {
            broker
                .authorize_and_execute(caller, cap, token, correlation, || Self::execute_raw(call))
        };
        match execution {
            Ok(res) => res,
            Err(denied) => {
                let approval_session = match &denied {
                    sail_guardian::ExecutionDenied::ApprovalRequired { session_id, .. } => {
                        Some(*session_id)
                    }
                    _ => None,
                };
                let approval_action = if approval_session.is_some()
                    && ["proc:list", "fs:query"].contains(&call.tool.as_str())
                    && serde_json::to_vec(&call.params).is_ok_and(|bytes| bytes.len() <= 8192)
                {
                    Some(serde_json::json!({"tool":call.tool,"params":call.params}))
                } else {
                    None
                };
                let error_code = match &denied {
                    sail_guardian::ExecutionDenied::SandboxViolation(
                        sail_guardian::SandboxViolation::PathEscapesRoot { .. }
                        | sail_guardian::SandboxViolation::SensitivePathBlocked { .. },
                    ) => "outside_workspace",
                    _ => "permission_denied",
                };
                let error_msg = if error_code == "outside_workspace" {
                    "Path is outside the authorized workspace".to_string()
                } else {
                    format!("Execution denied by Guardian: {denied}")
                };
                KernelToolResult {
                    tool: call.tool.clone(),
                    success: false,
                    output: serde_json::json!({
                        "error": error_msg,
                        "error_code": error_code,
                        "workspace_root": std::env::var("SAIL_TOOL_ROOT").unwrap_or_else(|_| ".".into()),
                        "denied": true,
                        "approval_session":approval_session,
                        "approval_required":approval_session.is_some(),
                        "session_id":approval_session,
                        "approval_action":approval_action,
                    }),
                }
            }
        }
    }

    /// Point-of-use gatekeeper entry point enforcing Guardian authorization
    pub fn execute(call: &KernelToolCall) -> KernelToolResult {
        Self::execute_guarded(sail_common::types::SubsystemId::Dialogue, call, None)
    }

    /// Crate-internal adapter entry; the scheduler already consumed the exact
    /// invocation grant. Filesystem effects still operate on confined descriptors.
    pub(crate) fn execute_scheduled(call: &KernelToolCall) -> KernelToolResult {
        if call.tool == "fs:query" || call.tool == "fs:read" {
            let path = call.params["path"].as_str().unwrap_or(".");
            return match sail_guardian::global_broker().and_then(|broker| {
                broker
                    .sandbox()
                    .open_read(Path::new(path))
                    .map_err(Into::into)
            }) {
                Ok(file) => {
                    if call.tool == "fs:read" {
                        Self::tool_fs_read(file)
                    } else {
                        Self::tool_fs_query(file)
                    }
                }
                Err(error) => Self::error("fs:query", &error.to_string()),
            };
        }
        Self::execute_raw(call)
    }
    /// Internal raw execution: only invoked by GuardianExecutionBroker after authorization
    fn execute_raw(call: &KernelToolCall) -> KernelToolResult {
        match call.tool.as_str() {
            "os:status" => Self::tool_os_status(),
            "proc:list" => Self::tool_proc_list(&call.params),
            "fs:query" => Self::error("fs:query", "Descriptor access required"),
            "fs:read" => Self::error("fs:read", "Descriptor access required"),
            "fs:write" => Self::tool_fs_write(&call.params),
            "vision:frame" => Self::tool_vision_frame(&call.params),
            "forge:enqueue" => Self::tool_forge_enqueue(&call.params),
            "forge:status" => Self::tool_forge_status(&call.params),
            "forge:list" => Self::tool_forge_list(&call.params),
            "forge:cancel" => Self::tool_forge_cancel(&call.params),
            "github:issues" => Self::tool_github_issues(&call.params),
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
                    && let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>()
                {
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
    fn tool_fs_query(file: sail_guardian::confined::ConfinedFile) -> KernelToolResult {
        let result = (|| -> std::io::Result<serde_json::Value> {
            let metadata = file.metadata()?;
            if metadata.is_file() {
                return Ok(
                    serde_json::json!({"type":"file","path":file.path(),"size_bytes":metadata.len()}),
                );
            }
            let entries: Vec<_> = file
                .entries(20)?
                .into_iter()
                .map(|(name, is_dir)| serde_json::json!({"name":name,"is_dir":is_dir}))
                .collect();
            Ok(serde_json::json!({"type":"directory","path":file.path(),"entries":entries}))
        })();
        match result {
            Ok(output) => KernelToolResult {
                tool: "fs:query".into(),
                success: true,
                output,
            },
            Err(error) => Self::error("fs:query", &error.to_string()),
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

    /// fs:read — Read the text contents of a file within the authorized workspace
    fn tool_fs_read(file: sail_guardian::confined::ConfinedFile) -> KernelToolResult {
        match file.read_limited(65536) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(content) => KernelToolResult {
                    tool: "fs:read".into(),
                    success: true,
                    output: serde_json::json!({"path":file.path(),"content":content}),
                },
                Err(_) => Self::error("fs:read", "File is not UTF-8 text"),
            },
            Err(error) => Self::error("fs:read", &error.to_string()),
        }
    }
    /// fs:write — Create or overwrite a file within the authorized workspace
    fn tool_fs_write(params: &serde_json::Value) -> KernelToolResult {
        let path = Path::new(params["path"].as_str().unwrap_or(""));
        let content = params["content"].as_str().unwrap_or("");
        match sail_guardian::global_broker().and_then(|broker| {
            broker
                .sandbox()
                .write_file(path, content.as_bytes())
                .map_err(Into::into)
        }) {
            Ok(bytes) => KernelToolResult {
                tool: "fs:write".into(),
                success: true,
                output: serde_json::json!({"path":path,"bytes_written":bytes}),
            },
            Err(error) => Self::error("fs:write", &error.to_string()),
        }
    }
    /// forge:enqueue — Enqueue an issue or task for autonomous resolution in Forge
    fn tool_forge_enqueue(params: &serde_json::Value) -> KernelToolResult {
        let orch = match sail_forge::global_orchestrator() {
            Ok(o) => o,
            Err(e) => return Self::error("forge:enqueue", &e),
        };
        let issue_num = params
            .get("issue_number")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                params.get("issue").and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                })
            })
            .unwrap_or_else(|| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                (now % 10000) + 1000
            });
        let repo = params
            .get("repo")
            .and_then(|v| v.as_str())
            .unwrap_or("PedroVictorCoding/SAIL")
            .to_string();
        let title = params
            .get("task")
            .or_else(|| params.get("title"))
            .and_then(|v| v.as_str())
            .unwrap_or("Autonomous task resolution")
            .to_string();
        let body = params
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or(&title)
            .to_string();

        let req = sail_forge::IssueEnqueueRequest {
            repo: repo.clone(),
            issue_number: issue_num,
            issue_title: Some(title.clone()),
            issue_body: Some(body),
            priority: Some("high".into()),
            budget: None,
            review_policy: None,
        };

        match orch.enqueue_issue(req) {
            Ok(job_id) => KernelToolResult {
                tool: "forge:enqueue".into(),
                success: true,
                output: serde_json::json!({
                    "job_id": job_id,
                    "repo": repo,
                    "issue_number": issue_num,
                    "title": title,
                    "status": "enqueued",
                    "message": format!("Forge job {job_id} enqueued for issue #{issue_num}. Running autonomous resolution loop in background worktree."),
                }),
            },
            Err(e) => Self::error(
                "forge:enqueue",
                &format!("Failed to enqueue Forge job: {e}"),
            ),
        }
    }

    /// forge:status — Query the status of an active or checkpointed Forge job
    fn tool_forge_status(params: &serde_json::Value) -> KernelToolResult {
        let orch = match sail_forge::global_orchestrator() {
            Ok(o) => o,
            Err(e) => return Self::error("forge:status", &e),
        };
        let job_id = params.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
        if job_id.is_empty() {
            return Self::error("forge:status", "Missing job_id parameter");
        }
        match orch.get_status(job_id) {
            Some(status) => KernelToolResult {
                tool: "forge:status".into(),
                success: true,
                output: serde_json::json!({
                    "job_id": job_id,
                    "status": status,
                }),
            },
            None => Self::error("forge:status", &format!("Job {job_id} not found")),
        }
    }

    /// forge:list — List all active and stored Forge jobs
    fn tool_forge_list(_params: &serde_json::Value) -> KernelToolResult {
        let orch = match sail_forge::global_orchestrator() {
            Ok(o) => o,
            Err(e) => return Self::error("forge:list", &e),
        };
        let jobs = orch.list_all_jobs();
        KernelToolResult {
            tool: "forge:list".into(),
            success: true,
            output: serde_json::json!({
                "count": jobs.len(),
                "jobs": jobs,
            }),
        }
    }

    /// forge:cancel — Cancel a running Forge job
    fn tool_forge_cancel(params: &serde_json::Value) -> KernelToolResult {
        let orch = match sail_forge::global_orchestrator() {
            Ok(o) => o,
            Err(e) => return Self::error("forge:cancel", &e),
        };
        let job_id = params.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
        let reason = params
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("User requested cancellation");
        match orch.cancel_job(job_id, reason) {
            Ok(()) => KernelToolResult {
                tool: "forge:cancel".into(),
                success: true,
                output: serde_json::json!({
                    "job_id": job_id,
                    "cancelled": true,
                    "reason": reason,
                }),
            },
            Err(e) => Self::error("forge:cancel", &format!("Failed to cancel job: {e}")),
        }
    }

    /// github:issues — Fetch recent open issues from GitHub CLI
    fn tool_github_issues(params: &serde_json::Value) -> KernelToolResult {
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(15)
            .clamp(1, 50);
        let output = std::process::Command::new("gh")
            .args([
                "issue",
                "list",
                "--json",
                "number,title,state,labels",
                "--limit",
                &limit.to_string(),
            ])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                if let Ok(issues) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    let count = issues.as_array().map(|a| a.len()).unwrap_or(0);
                    KernelToolResult {
                        tool: "github:issues".into(),
                        success: true,
                        output: serde_json::json!({
                            "count": count,
                            "issues": issues,
                        }),
                    }
                } else {
                    Self::error("github:issues", "Failed to parse GitHub CLI JSON output")
                }
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Self::error("github:issues", &format!("gh issue list failed: {stderr}"))
            }
            Err(e) => Self::error("github:issues", &format!("GitHub CLI unavailable: {e}")),
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
    pub fn with_config(config: sail_mind::GemmaConfig) -> Self {
        Self {
            runner: sail_mind::GemmaRunner::new(config),
        }
    }
    pub fn with_service(service: sail_mind::MindService) -> Self {
        Self {
            runner: sail_mind::GemmaRunner::from_service(service),
        }
    }
    pub fn with_services(services: &sail_mind::VoiceModelServices) -> Self {
        services
            .inference()
            .map(Self::with_service)
            .unwrap_or_else(|| Self::new("local"))
    }
    pub async fn health(&self) -> serde_json::Value {
        self.runner.health().await
    }
    pub async fn stream(
        &self,
        messages: Vec<serde_json::Value>,
        tx: tokio::sync::mpsc::Sender<String>,
    ) -> Result<serde_json::Value> {
        self.stream_with_tools(messages, true, tx).await
    }
    pub async fn stream_with_tools(
        &self,
        messages: Vec<serde_json::Value>,
        allow_tools: bool,
        tx: tokio::sync::mpsc::Sender<String>,
    ) -> Result<serde_json::Value> {
        let tools = if allow_tools {
            selected_tool_definitions(&messages)
        } else {
            serde_json::json!([])
        };
        self.runner.stream_chat(messages, tools, tx).await
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
/// Keep the model service's schema and byte budget bounded as tool packs grow.
pub fn selected_tool_definitions(messages: &[serde_json::Value]) -> serde_json::Value {
    let query = messages
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .map(|m| m["content"].to_string().to_lowercase())
        .unwrap_or_default();
    let words: Vec<_> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .collect();
    let all = tool_definitions();
    let mut ranked: Vec<_> = all
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, tool)| {
            let text = format!(
                "{} {}",
                tool["function"]["name"], tool["function"]["description"]
            )
            .to_lowercase();
            (
                words.iter().filter(|word| text.contains(**word)).count(),
                i,
                tool.clone(),
            )
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut bytes = 2;
    let mut selected = Vec::new();
    for (_, _, tool) in ranked {
        let size = tool.to_string().len() + 1;
        if selected.len() < 8 && bytes + size <= 16384 {
            bytes += size;
            selected.push(tool);
        }
    }
    serde_json::Value::Array(selected)
}
pub fn tool_definitions() -> serde_json::Value {
    serde_json::json!([
        {"type":"function","function":{"name":"os_status","description":"Read local OS memory, uptime and load","parameters":{"type":"object","properties":{}}}},
        {"type":"function","function":{"name":"proc_list","description":"List local processes","parameters":{"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":100}}}}},
        {"type":"function","function":{"name":"fs_query","description":"Inspect a file or directory inside the authorized workspace. Use relative paths; dot (.) means the workspace root. Never probe outside paths or retry a denied query.","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}},
        {"type":"function","function":{"name":"canvas_publish","description":"Display a diagram or structured visual beside the conversation","parameters":{"type":"object","properties":{"artifact_type":{"type":"string","enum":["mermaid","json","svg"]},"content":{"type":"string","maxLength":32700}},"required":["artifact_type","content"],"additionalProperties":false}}},
        {"type":"function","function":{"name":"local_time","description":"Read the current local device UTC clock","parameters":{"type":"object","properties":{},"additionalProperties":false}}},
        {"type":"function","function":{"name":"command_execute","description":"Execute a command in the isolated no-network runner after exact owner approval. Never host shell execution. Runner may be unavailable.","parameters":{"type":"object","properties":{"executable":{"type":"string","maxLength":4096},"argv":{"type":"array","maxItems":128,"items":{"type":"string","maxLength":8192}}},"required":["executable","argv"],"additionalProperties":false}}},
        {"type":"function","function":{"name":"vision_frame","description":"Measure luminance and edge density of the currently attached image. For semantic questions reason directly from the image input.","parameters":{"type":"object","properties":{}}}},
        {"type":"function","function":{"name":"fs_read","description":"Read text contents of a file inside the authorized workspace.","parameters":{"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"Relative path to the file in the workspace"}}}}},
        {"type":"function","function":{"name":"fs_write","description":"Create or write a file inside the authorized workspace. Requires explicit user approval.","parameters":{"type":"object","required":["path","content"],"properties":{"path":{"type":"string","description":"Relative path to write"},"content":{"type":"string","description":"File content to write"}}}}},
        {"type":"function","function":{"name":"forge_enqueue","description":"Enqueue an autonomous coding task or GitHub issue for Forge to solve in an isolated sandbox with automated testing, review, and PR drafting.","parameters":{"type":"object","properties":{"issue_number":{"type":"integer","description":"GitHub issue number to solve"},"repo":{"type":"string","description":"Repository name, defaults to PedroVictorCoding/SAIL"},"task":{"type":"string","description":"Optional description or instructions for the task"}}}}},
        {"type":"function","function":{"name":"forge_status","description":"Check the status, current step, test outcome, and diff of a Forge autonomous coding job.","parameters":{"type":"object","required":["job_id"],"properties":{"job_id":{"type":"string","description":"The job ID returned from forge_enqueue"}}}}}
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

#[cfg(test)]
mod schema_selection_tests {
    #[test]
    fn selection_keeps_forge_tools_within_mind_limits() {
        let tools = super::selected_tool_definitions(&[
            serde_json::json!({"role":"user","content":"Enqueue Forge to solve GitHub issue 90"}),
        ]);
        assert!(tools.as_array().unwrap().len() <= 8);
        assert!(tools.to_string().len() <= 16384);
        assert!(
            tools
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["function"]["name"] == "forge_enqueue")
        );
    }
}
