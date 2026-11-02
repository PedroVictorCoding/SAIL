use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

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
            "proc:list" | "forge:enqueue" | "forge:cancel" | "forge:clear" | "github:issues"
            | "web:read" => {
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
            let raw_path = call
                .params
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or(".");
            // Normalize path: if absolute path inside workspace root, make it relative to root
            let workspace_root = std::env::var("SAIL_TOOL_ROOT")
                .map(PathBuf::from)
                .ok()
                .or_else(|| std::env::current_dir().ok());
            let normalized_path = if let Some(ref root) = workspace_root {
                let p = Path::new(raw_path);
                if let Ok(rel) = p.strip_prefix(root) {
                    rel.to_path_buf()
                } else if p == root {
                    PathBuf::from(".")
                } else {
                    p.to_path_buf()
                }
            } else {
                PathBuf::from(raw_path)
            };
            let normalized_path = if normalized_path.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                normalized_path
            };
            let target_path = &normalized_path;
            // Option A: If the target path is inside the authorized workspace, pre-grant authorization
            // so reading files within the project never blocks the user for owner tokens.
            let auto_approved_token =
                if token.is_none() && broker.sandbox().validate_path(target_path, false).is_ok() {
                    let session = broker.approval().create_session(
                        caller,
                        cap.clone(),
                        "Workspace descriptor read pre-authorized",
                    );
                    if let Ok(tok) = broker.approval().resolve_session(session.session_id, true) {
                        Some(tok.token_id)
                    } else {
                        None
                    }
                } else {
                    token
                };

            broker.authorize_read(
                caller,
                target_path,
                auto_approved_token,
                correlation,
                |file| {
                    if call.tool == "fs:read" {
                        Self::tool_fs_read(file)
                    } else {
                        Self::tool_fs_query(file)
                    }
                },
            )
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
            "forge:clear" => Self::tool_forge_clear(&call.params),
            "github:issues" => Self::tool_github_issues(&call.params),
            "web:read" => Self::tool_web_read(&call.params),
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

        let loadavg_raw = fs::read_to_string("/proc/loadavg").unwrap_or_default();
        let loadavg_parts: Vec<f64> = loadavg_raw
            .split_whitespace()
            .take(3)
            .filter_map(|s| s.parse().ok())
            .collect();
        let loadavg = if !loadavg_parts.is_empty() {
            loadavg_parts
                .iter()
                .map(|f| format!("{:.2}", f))
                .collect::<Vec<String>>()
                .join(" ")
        } else {
            loadavg_raw
                .split_whitespace()
                .take(3)
                .map(|s| s.to_string())
                .collect::<Vec<String>>()
                .join(" ")
        };

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
        let logical_cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        // Compute CPU utilization from /proc/stat with fallback to normalized load average
        static PREV_CPU: std::sync::Mutex<Option<(u64, u64)>> = std::sync::Mutex::new(None);
        let mut cpu_percent = if let Some(first_load) = loadavg_parts.first() {
            ((*first_load / logical_cpus as f64) * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };

        if let Ok(stat) = fs::read_to_string("/proc/stat") {
            if let Some(line) = stat.lines().next() {
                if line.starts_with("cpu ") {
                    let ticks: Vec<u64> = line
                        .split_whitespace()
                        .skip(1)
                        .filter_map(|s| s.parse().ok())
                        .collect();
                    if ticks.len() >= 4 {
                        let idle = ticks[3] + ticks.get(4).copied().unwrap_or(0);
                        let total: u64 = ticks.iter().sum();
                        let work = total.saturating_sub(idle);
                        if let Ok(mut prev_guard) = PREV_CPU.lock() {
                            if let Some((prev_work, prev_total)) = *prev_guard {
                                let d_work = work.saturating_sub(prev_work);
                                let d_total = total.saturating_sub(prev_total);
                                if d_total > 0 {
                                    cpu_percent = ((d_work as f64) / (d_total as f64) * 100.0)
                                        .clamp(0.0, 100.0);
                                }
                            }
                            *prev_guard = Some((work, total));
                        }
                    }
                }
            }
        }

        KernelToolResult {
            tool: "os:status".to_string(),
            success: true,
            output: serde_json::json!({
                "os": std::env::consts::OS,
                "uptime_hours": format!("{:.1}", uptime / 3600.0),
                "load_average": loadavg,
                "load_averages": loadavg_parts,
                "memory_used_gb": format!("{:.2} / {:.2} GB", mem_used_gb, mem_total_gb),
                "memory_percent": format!("{:.1}%", (mem_used_gb / mem_total_gb.max(1.0)) * 100.0),
                "logical_cpus": logical_cpus,
                "cpu_percent": format!("{:.1}%", cpu_percent),
                "cpu_usage_raw": (cpu_percent * 10.0).round() / 10.0,
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
                    "job_id": status.job_id.clone(),
                    "issue_number": status.issue_number,
                    "state": status.state.as_str(),
                    "current_step": status.current_step.clone(),
                    "diff_stat": status.diff_stat.clone(),
                    "target_file": status.target_file.clone(),
                    "patch": status.patch.clone(),
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

    /// forge:clear — Clear, archive, or remove completed/blocked Forge jobs and delete their checkpoints
    fn tool_forge_clear(params: &serde_json::Value) -> KernelToolResult {
        let orch = match sail_forge::global_orchestrator() {
            Ok(o) => o,
            Err(e) => return Self::error("forge:clear", &e),
        };
        let job_id = params
            .get("job_id")
            .and_then(|v| v.as_str())
            .unwrap_or("all");
        match orch.clear_jobs(job_id) {
            Ok(cleared) => KernelToolResult {
                tool: "forge:clear".into(),
                success: true,
                output: serde_json::json!({
                    "cleared_jobs": cleared,
                    "count": cleared.len(),
                    "target": job_id,
                }),
            },
            Err(e) => Self::error("forge:clear", &format!("Failed to clear jobs: {e}")),
        }
    }

    /// Extracts priority rank (0=P0, 1=P1, 2=P2, 3=P3, 4=P4, 99=unspecified) and label.
    pub fn extract_issue_priority(title: &str, labels: &[serde_json::Value]) -> (u8, &'static str) {
        let upper_title = title.to_ascii_uppercase();
        for (rank, label) in [
            (0u8, "P0"),
            (1u8, "P1"),
            (2u8, "P2"),
            (3u8, "P3"),
            (4u8, "P4"),
        ] {
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
                if upper_name.contains("P0")
                    || upper_name.contains("CRITICAL")
                    || upper_name.contains("BLOCKER")
                {
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

    /// github:issues — Fetch open issues from GitHub CLI sorted by priority (P0 -> P1 -> P2 -> P3 -> P4),
    /// or fetch specific issue details if issue_number / url / number / id is provided.
    fn tool_github_issues(params: &serde_json::Value) -> KernelToolResult {
        // Extract issue target (number, string "#125", or full URL "https://github.com/owner/repo/issues/125")
        let target_spec = params
            .get("url")
            .or_else(|| params.get("issue_number"))
            .or_else(|| params.get("number"))
            .or_else(|| params.get("id"))
            .and_then(|v| {
                if let Some(n) = v.as_i64() {
                    Some(n.to_string())
                } else if let Some(s) = v.as_str() {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        Some(trimmed.to_string())
                    } else {
                        None
                    }
                } else {
                    None
                }
            });

        if let Some(spec) = target_spec {
            let view_arg = spec.trim_start_matches('#');
            let output = std::process::Command::new("gh")
                .args([
                    "issue",
                    "view",
                    view_arg,
                    "--json",
                    "number,title,body,state,labels,author,url,createdAt",
                ])
                .output();

            return match output {
                Ok(out) if out.status.success() => {
                    if let Ok(issue) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                        KernelToolResult {
                            tool: "github:issues".into(),
                            success: true,
                            output: serde_json::json!({
                                "found": true,
                                "issue": issue,
                                "number": issue.get("number").cloned().unwrap_or(serde_json::json!(view_arg)),
                                "title": issue.get("title").cloned().unwrap_or_default(),
                                "body": issue.get("body").cloned().unwrap_or_default(),
                                "state": issue.get("state").cloned().unwrap_or_default(),
                                "url": issue.get("url").cloned().unwrap_or_default(),
                                "labels": issue.get("labels").cloned().unwrap_or_default(),
                            }),
                        }
                    } else {
                        Self::error("github:issues", "Failed to parse GitHub CLI JSON output")
                    }
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    Self::error(
                        "github:issues",
                        &format!("GitHub issue '{spec}' not found: {stderr}"),
                    )
                }
                Err(e) => Self::error("github:issues", &format!("GitHub CLI unavailable: {e}")),
            };
        }

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
            .args([
                "issue",
                "list",
                "--state",
                "open",
                "--json",
                "number,title,state,labels",
                "--limit",
                "100",
            ])
            .output();

        match output {
            Ok(out) if out.status.success() => {
                if let Ok(raw_issues) =
                    serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout)
                {
                    let mut scored_issues: Vec<(u8, i64, serde_json::Value)> = raw_issues
                        .into_iter()
                        .filter_map(|mut item| {
                            let title = item
                                .get("title")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default();
                            let empty_vec = Vec::new();
                            let labels = item
                                .get("labels")
                                .and_then(|v| v.as_array())
                                .unwrap_or(&empty_vec);
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

    /// web:read — Read-only HTTP/HTTPS web fetcher for public documentation, web articles, and online URLs.
    /// Strictly read-only: no writes, no POST/PUT/DELETE, no local file modifications.
    fn tool_web_read(params: &serde_json::Value) -> KernelToolResult {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim();

        if url.is_empty() {
            return Self::error("web:read", "Parameter 'url' is required (e.g. https://...)");
        }

        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Self::error("web:read", "URL must start with http:// or https://");
        }

        // Restrict loopback and private address probing
        if url.contains("://127.")
            || url.contains("://localhost")
            || url.contains("://0.0.0.0")
            || url.contains("://[::1]")
        {
            return Self::error(
                "web:read",
                "Loopback and local addresses are restricted for web reading.",
            );
        }

        let output = std::process::Command::new("curl")
            .args([
                "-sL",
                "--max-time",
                "10",
                "--max-filesize",
                "5000000",
                "-H",
                "User-Agent: Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
                url,
            ])
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let raw_content = String::from_utf8_lossy(&out.stdout);
                let clean = Self::extract_text_from_html(&raw_content);
                let capped: String = clean.chars().take(24000).collect();
                KernelToolResult {
                    tool: "web:read".into(),
                    success: true,
                    output: serde_json::json!({
                        "url": url,
                        "bytes": raw_content.len(),
                        "content": capped,
                    }),
                }
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Self::error("web:read", &format!("HTTP request failed: {stderr}"))
            }
            Err(e) => Self::error("web:read", &format!("Failed to execute curl: {e}")),
        }
    }

    fn extract_text_from_html(html: &str) -> String {
        let mut in_tag = false;
        let mut in_script = false;
        let mut in_style = false;
        let mut result = String::with_capacity(html.len());
        let mut tag_name = String::new();

        for ch in html.chars() {
            if ch == '<' {
                in_tag = true;
                tag_name.clear();
            } else if ch == '>' {
                in_tag = false;
                let lower = tag_name.to_ascii_lowercase();
                if lower.starts_with("script") {
                    in_script = true;
                } else if lower.starts_with("/script") {
                    in_script = false;
                } else if lower.starts_with("style") {
                    in_style = true;
                } else if lower.starts_with("/style") {
                    in_style = false;
                } else if matches!(
                    lower.as_str(),
                    "p" | "/p"
                        | "div"
                        | "/div"
                        | "br"
                        | "h1"
                        | "/h1"
                        | "h2"
                        | "/h2"
                        | "h3"
                        | "/h3"
                        | "li"
                        | "/li"
                ) {
                    result.push('\n');
                }
            } else if in_tag {
                tag_name.push(ch);
            } else if !in_script && !in_style {
                result.push(ch);
            }
        }

        result
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
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
pub const MEMORY_RECALL_TOOL: &str = "memory_recall";

/// Keep the model service's schema and byte budget bounded as tool packs grow.
/// `memory_recall` is always offered once the context references archived `mem:` handles,
/// because compaction relies on the model being able to fetch what was moved out.
pub fn selected_tool_definitions(messages: &[serde_json::Value]) -> serde_json::Value {
    let has_handles = messages.iter().any(|m| m.to_string().contains("mem:"));
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
    if has_handles {
        if let Some(pos) = ranked
            .iter()
            .position(|(_, _, tool)| tool["function"]["name"] == MEMORY_RECALL_TOOL)
        {
            let recall = ranked.remove(pos);
            ranked.insert(0, recall);
        }
    }
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
        {"type":"function","function":{"name":"github_issues","description":"Retrieve open GitHub issues ordered by priority, or fetch full details of a specific issue by number or URL","parameters":{"type":"object","properties":{"url":{"type":"string","description":"Specific GitHub issue URL (e.g. https://github.com/owner/repo/issues/125)"},"issue_number":{"type":"integer","description":"Specific GitHub issue number to fetch full title, body, state and labels for"},"limit":{"type":"integer","minimum":1,"maximum":50},"priority":{"type":"string","maxLength":2,"enum":["P0","P1","P2","P3","P4","p0","p1","p2","p3","p4"]}},"additionalProperties":false}}},
        {"type":"function","function":{"name":"web_read","description":"Fetch and read the text contents of any public web page, URL, documentation, or online issue without write permissions","parameters":{"type":"object","required":["url"],"properties":{"url":{"type":"string","description":"The HTTP or HTTPS URL to fetch and read"}},"additionalProperties":false}}},
        {"type":"function","function":{"name":"forge_enqueue","description":"Enqueue an autonomous coding task or GitHub issue for Forge to solve in an isolated sandbox with automated testing, review, and PR drafting.","parameters":{"type":"object","properties":{"issue_number":{"type":"integer","description":"GitHub issue number to solve"},"repo":{"type":"string","description":"Repository name, defaults to PedroVictorCoding/SAIL"},"task":{"type":"string","description":"Optional description or instructions for the task"}}}}},
        {"type":"function","function":{"name":"forge_status","description":"Check the status, current step, test outcome, and diff of a Forge autonomous coding job. Accepts exact job IDs (e.g. job-5-61c992ad), prefix (e.g. job-5), or issue numbers (e.g. 5 or #5).","parameters":{"type":"object","required":["job_id"],"properties":{"job_id":{"type":"string","description":"The job ID or issue number (e.g. 5, #5, job-5, or job-5-61c992ad)"}}}}},
        {"type":"function","function":{"name":"forge_list","description":"List all active, recent, and stored Forge autonomous coding jobs and tasks.","parameters":{"type":"object","properties":{},"additionalProperties":false}}},
        {"type":"function","function":{"name":"forge_cancel","description":"Cancel or stop a running or in-progress Forge autonomous coding task.","parameters":{"type":"object","required":["job_id"],"properties":{"job_id":{"type":"string","description":"The job ID or issue number to cancel"},"reason":{"type":"string","description":"Optional cancellation reason"}}}}},
        {"type":"function","function":{"name":"forge_clear","description":"Remove, clear, or archive blocked, cancelled, or completed Forge coding jobs from the active queue and delete their local checkpoints. Pass 'all' to clear all finished/blocked jobs, or specify a job_id.","parameters":{"type":"object","properties":{"job_id":{"type":"string","description":"Specific job ID to remove, or 'all' to clear all finished and blocked jobs (defaults to 'all')"}}}}},
        {"type":"function","function":{"name":MEMORY_RECALL_TOOL,"description":"Read this conversation's archived memory: an earlier turn or a full tool result by id (mem:N), optionally only the parts matching query or a page from offset; or search the archive by query alone.","parameters":{"type":"object","properties":{"id":{"type":"string","description":"Archive handle such as mem:12"},"query":{"type":"string","maxLength":500,"description":"Words to look for"},"offset":{"type":"integer","minimum":0,"description":"Character offset to page through a long record"},"max_chars":{"type":"integer","minimum":200,"maximum":6000}},"additionalProperties":false}}}
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

    #[test]
    fn all_tools_are_represented_in_definitions() {
        let all = super::tool_definitions();
        let tools = all.as_array().expect("array of tools");
        assert_eq!(tools.len(), 17);

        let names: Vec<_> = tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();

        assert!(names.contains(&"os_status"));
        assert!(names.contains(&"proc_list"));
        assert!(names.contains(&"fs_query"));
        assert!(names.contains(&"fs_read"));
        assert!(names.contains(&"fs_write"));
        assert!(names.contains(&"canvas_publish"));
        assert!(names.contains(&"local_time"));
        assert!(names.contains(&"command_execute"));
        assert!(names.contains(&"vision_frame"));
        assert!(names.contains(&"github_issues"));
        assert!(names.contains(&"web_read"));
        assert!(names.contains(&"forge_enqueue"));
        assert!(names.contains(&"forge_status"));
        assert!(names.contains(&"forge_list"));
        assert!(names.contains(&"forge_cancel"));
        assert!(names.contains(&"forge_clear"));
        assert!(names.contains(&"memory_recall"));
    }

    #[test]
    fn memory_recall_is_offered_when_context_has_archive_handles() {
        let offered = |messages: &[serde_json::Value]| -> Vec<String> {
            super::selected_tool_definitions(messages)
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["function"]["name"].as_str().unwrap().to_string())
                .collect()
        };
        let plain = [serde_json::json!({"role":"user","content":"list forge jobs and processes"})];
        assert!(!offered(&plain).contains(&"memory_recall".to_string()));
        let with_handles = [
            serde_json::json!({"role":"system","content":"[\"[mem:4 t2] U: ...\"]"}),
            plain[0].clone(),
        ];
        assert_eq!(offered(&with_handles)[0], "memory_recall");
    }
}
