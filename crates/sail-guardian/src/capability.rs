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
                SystemCapability::ProcessKill => RiskLevel::High,
                SystemCapability::NetworkAccess { .. } => RiskLevel::High,
            },
            Capability::Tool(t) => match t {
                ToolCapability::KernelTool(name) => match name.as_str() {
                    "os:status" => RiskLevel::Low,
                    "proc:list" | "fs:query" => RiskLevel::Medium,
                    "vision:frame" => RiskLevel::High,
                    _ => RiskLevel::High,
                },
                ToolCapability::CliCommand(_) => RiskLevel::Critical,
            },
        }
    }
}
