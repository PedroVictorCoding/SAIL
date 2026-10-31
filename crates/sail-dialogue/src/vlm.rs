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
            "os:status" => sail_guardian::Capability::System(sail_guardian::SystemCapability::OsTelemetry),
            "proc:list" => sail_guardian::Capability::System(sail_guardian::SystemCapability::ProcessInspect),
            "fs:query" | "fs:read" => {
                let path_str = call.params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                let configured = std::env::var("SAIL_TOOL_ROOT").unwrap_or_else(|_| ".".into());
                let candidate = std::path::Path::new(path_str);
                let resolved = if candidate.is_absolute() {
                    candidate.to_path_buf()
                } else {
                    std::path::Path::new(&configured).join(candidate)
                };
                sail_guardian::Capability::Filesystem(sail_guardian::FsCapability::Read {
                    path: resolved,
                })
            }
            "fs:write" => {
                let path_str = call.params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                let configured = std::env::var("SAIL_TOOL_ROOT").unwrap_or_else(|_| ".".into());
                let candidate = std::path::Path::new(path_str);
                let resolved = if candidate.is_absolute() {
                    candidate.to_path_buf()
                } else {
                    std::path::Path::new(&configured).join(candidate)
                };
                sail_guardian::Capability::Filesystem(sail_guardian::FsCapability::Write {
                    path: resolved,
                })
            }
            "vision:frame" => sail_guardian::Capability::Device(sail_guardian::DeviceCapability::Camera),
            cmd => sail_guardian::Capability::Tool(sail_guardian::ToolCapability::KernelTool(cmd.to_string())),
        }
    }

    /// Execute a kernel tool through the mandatory Guardian execution gate.
    /// Fails closed if Guardian is unavailable or if authorization is denied.
    pub fn execute_guarded(
        caller: sail_common::types::SubsystemId,
        call: &KernelToolCall,
        token: Option<uuid::Uuid>,
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
        match broker.authorize_and_execute(caller, cap, token, None, || Self::execute_raw(call)) {
            Ok(res) => res,
            Err(denied) => {
                if let sail_guardian::ExecutionDenied::ApprovalRequired { session_id, rationale } = &denied {
                    return KernelToolResult {
                        tool: call.tool.clone(),
                        success: false,
                        output: serde_json::json!({
                            "error": format!("Execution requires user approval: {rationale}"),
                            "error_code": "permission_denied",
                            "approval_required": true,
                            "session_id": session_id.to_string(),
                            "rationale": rationale,
                            "tool": call.tool,
                            "params": call.params,
                            "workspace_root": std::env::var("SAIL_TOOL_ROOT").unwrap_or_else(|_| ".".into()),
                            "denied": true,
                        }),
                    };
                }
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
                    }),
                }
            }
        }
    }

    /// Point-of-use gatekeeper entry point enforcing Guardian authorization
    pub fn execute(call: &KernelToolCall) -> KernelToolResult {
        Self::execute_guarded(sail_common::types::SubsystemId::Dialogue, call, None)
    }

    /// Internal raw execution: only invoked by GuardianExecutionBroker after authorization
    pub fn execute_raw(call: &KernelToolCall) -> KernelToolResult {
        match call.tool.as_str() {
            "os:status" => Self::tool_os_status(),
            "proc:list" => Self::tool_proc_list(&call.params),
            "fs:query" => Self::tool_fs_query(&call.params),
            "fs:read" => Self::tool_fs_read(&call.params),
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
    fn tool_fs_query(params: &serde_json::Value) -> KernelToolResult {
        let requested = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let configured = std::env::var("SAIL_TOOL_ROOT").unwrap_or_else(|_| ".".into());
        let root = match fs::canonicalize(&configured) {
            Ok(p) => p,
            Err(e) => {
                return KernelToolResult {
                    tool: "fs:query".into(),
                    success: false,
                    output: serde_json::json!({"error":"The configured workspace root is unavailable","error_code":"invalid_workspace_root","workspace_root":configured,"detail":e.to_string()}),
                };
            }
        };
        let candidate = Path::new(requested);
        let candidate = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            root.join(candidate)
        };
        let failure = |code: &str, message: String| KernelToolResult {
            tool: "fs:query".into(),
            success: false,
            output: serde_json::json!({"error":message,"error_code":code,"requested_path":requested,"workspace_root":root,"hint":"Use a workspace-relative path, such as . or README.md. Do not retry an outside path."}),
        };
        let path = match fs::canonicalize(&candidate) {
            Ok(p) => p,
            Err(e) => {
                return failure(
                    if e.kind() == std::io::ErrorKind::NotFound {
                        "not_found"
                    } else {
                        "path_unavailable"
                    },
                    format!("Cannot resolve requested path: {e}"),
                );
            }
        };
        if !path.starts_with(&root) {
            return failure(
                "outside_workspace",
                "Path is outside the authorized workspace".into(),
            );
        }
        tracing::debug!(requested_path=requested,resolved_path=%path.display(),workspace_root=%root.display(),"Filesystem query passed workspace boundary check");
        let metadata = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => return failure("metadata_unavailable", e.to_string()),
        };
        if metadata.is_file() {
            return KernelToolResult {
                tool: "fs:query".into(),
                success: true,
                output: serde_json::json!({"type":"file","path":path,"size_bytes":metadata.len()}),
            };
        }
        let directory = match fs::read_dir(&path) {
            Ok(d) => d,
            Err(e) => return failure("directory_unavailable", e.to_string()),
        };
        let entries:Vec<_>=directory.filter_map(|entry|entry.ok()).take(20).map(|entry|serde_json::json!({"name":entry.file_name().to_string_lossy(),"is_dir":entry.file_type().is_ok_and(|t|t.is_dir())})).collect();
        KernelToolResult {
            tool: "fs:query".into(),
            success: true,
            output: serde_json::json!({"type":"directory","path":path,"entries":entries}),
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
    fn tool_fs_read(params: &serde_json::Value) -> KernelToolResult {
        let requested = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let configured = std::env::var("SAIL_TOOL_ROOT").unwrap_or_else(|_| ".".into());
        let root = match fs::canonicalize(&configured) {
            Ok(p) => p,
            Err(e) => {
                return Self::error("fs:read", &format!("Configured workspace root unavailable: {e}"));
            }
        };
        let candidate = Path::new(requested);
        let candidate = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            root.join(candidate)
        };
        let path = match fs::canonicalize(&candidate) {
            Ok(p) => p,
            Err(e) => {
                return Self::error("fs:read", &format!("Cannot resolve path: {e}"));
            }
        };
        if !path.starts_with(&root) {
            return Self::error("fs:read", "Path is outside the authorized workspace");
        }
        match fs::read_to_string(&path) {
            Ok(content) => KernelToolResult {
                tool: "fs:read".into(),
                success: true,
                output: serde_json::json!({
                    "path": path.display().to_string(),
                    "content": content,
                    "bytes": content.len(),
                }),
            },
            Err(e) => Self::error("fs:read", &format!("Failed to read file: {e}")),
        }
    }

    /// fs:write — Create or overwrite a file within the authorized workspace
    fn tool_fs_write(params: &serde_json::Value) -> KernelToolResult {
        let requested = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
        if requested.is_empty() {
            return Self::error("fs:write", "Missing path parameter");
        }
        let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let configured = std::env::var("SAIL_TOOL_ROOT").unwrap_or_else(|_| ".".into());
        let root = match fs::canonicalize(&configured) {
            Ok(p) => p,
            Err(e) => {
                return Self::error("fs:write", &format!("Configured workspace root unavailable: {e}"));
            }
        };
        let candidate = Path::new(requested);
        let target = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            root.join(candidate)
        };
        let target_norm = if let Ok(canon) = fs::canonicalize(&target) {
            canon
        } else if let Some(parent) = target.parent() {
            if let Ok(canon_parent) = fs::canonicalize(parent) {
                canon_parent.join(target.file_name().unwrap_or_default())
            } else {
                target.clone()
            }
        } else {
            target.clone()
        };
        if !target_norm.starts_with(&root) {
            return Self::error("fs:write", "Target path is outside the authorized workspace");
        }
        if let Some(parent) = target_norm.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            return Self::error("fs:write", &format!("Failed to create parent directory: {e}"));
        }
        match fs::write(&target_norm, content) {
            Ok(()) => KernelToolResult {
                tool: "fs:write".into(),
                success: true,
                output: serde_json::json!({
                    "path": target_norm.display().to_string(),
                    "bytes_written": content.len(),
                    "success": true,
                }),
            },
            Err(e) => Self::error("fs:write", &format!("Failed to write file: {e}")),
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
                    v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
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
            Err(e) => Self::error("forge:enqueue", &format!("Failed to enqueue Forge job: {e}")),
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
        let reason = params.get("reason").and_then(|v| v.as_str()).unwrap_or("User requested cancellation");
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

    /// Extracts priority rank (0=P0, 1=P1, 2=P2, 3=P3, 4=P4, 99=unspecified) and label.
    pub fn extract_issue_priority(title: &str, labels: &[serde_json::Value]) -> (u8, &'static str) {
        let upper_title = title.to_ascii_uppercase();
        for (rank, label) in [(0u8, "P0"), (1u8, "P1"), (2u8, "P2"), (3u8, "P3"), (4u8, "P4")] {
            let patterns = [
                format!("[{label}"),
                format!("({label}"),
                format!("{label}:"),
                format!("{label} ·"),
                format!("· {label}"),
                format!(" {label} "),
                format!("{label}]"),
                format!("{label})"),
            ];
            if patterns.iter().any(|pat| upper_title.contains(pat)) {
                return (rank, label);
            }
        }

        for l in labels {
            if let Some(name) = l.get("name").and_then(|v| v.as_str()) {
                let upper_name = name.to_ascii_uppercase();
                if upper_name.contains("P0") || upper_name.contains("CRITICAL") || upper_name.contains("BLOCKER") {
                    return (0, "P0");
                }
                if upper_name.contains("P1") || upper_name.contains("URGENT") {
                    return (1, "P1");
                }
                if upper_name.contains("P2") {
                    return (2, "P2");
                }
                if upper_name.contains("P3") {
                    return (3, "P3");
                }
                if upper_name.contains("P4") {
                    return (4, "P4");
                }
            }
        }

        (99, "P?")
    }

    /// github:issues — Fetch open issues from GitHub CLI sorted by priority (P0 -> P1 -> P2 -> P3 -> P4)
    fn tool_github_issues(params: &serde_json::Value) -> KernelToolResult {
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(15)
            .clamp(1, 50) as usize;
        let priority_filter = params
            .get("priority")
            .and_then(|v| v.as_str())
            .map(|s| s.to_ascii_uppercase());

        // Fetch a broad pool of open issues (up to 100) so high-priority issues aren't missed
        let output = std::process::Command::new("gh")
            .args(["issue", "list", "--state", "open", "--json", "number,title,state,labels", "--limit", "100"])
            .output();

        match output {
            Ok(out) if out.status.success() => {
                if let Ok(raw_issues) = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout) {
                    let mut scored_issues: Vec<(u8, i64, serde_json::Value)> = raw_issues
                        .into_iter()
                        .filter_map(|mut item| {
                            let title = item.get("title").and_then(|v| v.as_str()).unwrap_or_default();
                            let empty_vec = Vec::new();
                            let labels = item.get("labels").and_then(|v| v.as_array()).unwrap_or(&empty_vec);
                            let (rank, prio_label) = Self::extract_issue_priority(title, labels);

                            if let Some(ref filter) = priority_filter
                                && prio_label != filter
                                && format!("P{rank}") != *filter
                            {
                                return None;
                            }

                            if let Some(obj) = item.as_object_mut() {
                                obj.insert("priority".into(), serde_json::json!(prio_label));
                                obj.insert("priority_rank".into(), serde_json::json!(rank));
                            }

                            let number = item.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
                            // Sort by priority_rank ascending, then by issue number descending
                            Some((rank, -number, item))
                        })
                        .collect();

                    scored_issues.sort_by_key(|a| (a.0, a.1));

                    let total_found = scored_issues.len();
                    let final_issues: Vec<serde_json::Value> = scored_issues
                        .into_iter()
                        .take(limit)
                        .map(|(_, _, item)| item)
                        .collect();

                    KernelToolResult {
                        tool: "github:issues".into(),
                        success: true,
                        output: serde_json::json!({
                            "count": final_issues.len(),
                            "total_matched": total_found,
                            "sorted_by": "priority",
                            "issues": final_issues,
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
        self.runner
            .stream_chat(
                messages,
                if allow_tools {
                    tool_definitions()
                } else {
                    serde_json::json!([])
                },
                tx,
            )
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
        {"type":"function","function":{"name":"fs_query","description":"Inspect a file or directory inside the authorized workspace. Use relative paths; dot (.) means the workspace root. Never probe outside paths or retry a denied query.","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}},
        {"type":"function","function":{"name":"fs_read","description":"Read text contents of a file inside the authorized workspace.","parameters":{"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"Relative path to the file in the workspace"}}}}},
        {"type":"function","function":{"name":"fs_write","description":"Create or write a file inside the authorized workspace. Requires explicit user approval.","parameters":{"type":"object","required":["path","content"],"properties":{"path":{"type":"string","description":"Relative path to write"},"content":{"type":"string","description":"File content to write"}}}}},
        {"type":"function","function":{"name":"vision_frame","description":"Measure luminance and edge density of the currently attached image. For semantic questions reason directly from the image input.","parameters":{"type":"object","properties":{}}}},
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
