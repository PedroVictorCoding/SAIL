use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "category", content = "detail", rename_all = "snake_case")]
pub enum Capability {
    Device(DeviceCapability),
    Filesystem(FsCapability),
    System(SystemCapability),
    Tool(ToolCapability),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceCapability {
    Microphone,
    Camera,
    Speaker,
    Display,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsCapability {
    Read { path: PathBuf },
    Write { path: PathBuf },
    Execute { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemCapability {
    OsTelemetry,
    ProcessInspect,
    ProcessKill,
    NetworkAccess { host: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCapability {
    KernelTool(String),
    CliCommand(String),
    SandboxCommand {
        request_digest: String,
        request: String,
    },
    Invocation {
        tool: String,
        arguments: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

impl Capability {
    /// Evaluate intrinsic risk level of this capability
    pub fn risk_level(&self) -> RiskLevel {
        match self {
            Capability::Device(d) => match d {
                DeviceCapability::Speaker | DeviceCapability::Display => RiskLevel::Low,
                DeviceCapability::Microphone => RiskLevel::Medium,
                DeviceCapability::Camera => RiskLevel::High,
            },
            Capability::Filesystem(fs) => match fs {
                FsCapability::Read { .. } => RiskLevel::Medium,
                FsCapability::Write { .. } => RiskLevel::High,
                FsCapability::Execute { .. } => RiskLevel::Critical,
            },
            Capability::System(s) => match s {
                SystemCapability::OsTelemetry => RiskLevel::Low,
                SystemCapability::ProcessInspect => RiskLevel::Medium,
                SystemCapability::ProcessKill => RiskLevel::Critical,
                SystemCapability::NetworkAccess { .. } => RiskLevel::High,
            },
            Capability::Tool(t) => match t {
                ToolCapability::KernelTool(name) => match name.as_str() {
                    "os:status"
                    | "forge:status"
                    | "forge:list"
                    | "forge:cancel"
                    | "forge:clear"
                    | "github:issues"
                    | "web:read"
                    | "web:speak"
                    | "studio:audio_analyze" => RiskLevel::Low,
                    "proc:list" | "fs:query" | "fs:read" => RiskLevel::Medium,
                    "vision:frame" | "fs:write" | "forge:enqueue" => RiskLevel::High,
                    _ => RiskLevel::High,
                },
                ToolCapability::Invocation { tool, arguments }
                    if tool == "local:time" && arguments == "{}" =>
                {
                    RiskLevel::Low
                }
                ToolCapability::Invocation { tool, .. }
                    if tool == "canvas:publish"
                        || tool == "github:issues"
                        || tool == "web:read"
                        || tool == "web:speak"
                        || tool == "forge:status"
                        || tool == "forge:list"
                        || tool == "forge:cancel"
                        || tool == "forge:clear"
                        || tool == "pulse:create"
                        || tool == "pulse:list"
                        || tool == "pulse:cancel"
                        || tool == "pulse:snooze"
                        || tool == "studio:audio_analyze" =>
                {
                    RiskLevel::Low
                }
                ToolCapability::Invocation { tool, .. } if tool == "command:execute" => {
                    RiskLevel::Critical
                }
                ToolCapability::Invocation { .. } => RiskLevel::High,
                ToolCapability::CliCommand(_) | ToolCapability::SandboxCommand { .. } => {
                    RiskLevel::Critical
                }
            },
        }
    }
}

/// Bind a tool request to every argument. Large media stays out of prompts/audit;
/// its immutable digest is the resource shown in the approval.
pub fn bound_tool_invocation(tool: &str, input: &serde_json::Value) -> Capability {
    let bytes = serde_json::to_vec(input).expect("JSON serialization");
    let arguments = if tool == "vision:frame" || bytes.len() > 8192 {
        let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
        let digest: String = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        serde_json::json!({"sha256":digest,"bytes":bytes.len(),"width":input.get("width"),"height":input.get("height"),"format":input.get("format")}).to_string()
    } else {
        String::from_utf8(bytes).expect("JSON UTF-8")
    };
    Capability::Tool(ToolCapability::Invocation {
        tool: tool.into(),
        arguments,
    })
}
