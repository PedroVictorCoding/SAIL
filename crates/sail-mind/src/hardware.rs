use sail_protocol::{ErrorCode, HardwareDiagnostics, InferenceError, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

pub struct HardwareProbe;

impl HardwareProbe {
    /// Read total and free/available memory in MB from /proc/meminfo.
    pub fn read_ram_mb() -> (u64, u64) {
        if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
            let mut total_kb = 0u64;
            let mut avail_kb = 0u64;
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:")
                    && let Some(val) = rest
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                {
                    total_kb = val;
                } else if let Some(rest) = line.strip_prefix("MemAvailable:")
                    && let Some(val) = rest
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                {
                    avail_kb = val;
                }
            }
            if total_kb > 0 {
                return (total_kb / 1024, avail_kb / 1024);
            }
        }
        (0, 0)
    }

    /// Check if /dev/dri/renderD* or specific device is accessible for reading.
    pub fn check_render_device_access(path: &Path) -> (bool, Option<String>) {
        if !path.exists() {
            return (
                false,
                Some(format!("Device node {} does not exist", path.display())),
            );
        }
        match std::fs::OpenOptions::new().read(true).open(path) {
            Ok(_) => (true, None),
            Err(e) => {
                let user = std::env::var("USER").unwrap_or_else(|_| "current user".into());
                let rem = format!(
                    "User '{}' cannot access {} ({}). Ensure user belongs to the 'render' and 'video' groups: sudo usermod -a -G render,video {}",
                    user,
                    path.display(),
                    e,
                    user
                );
                (false, Some(rem))
            }
        }
    }

    /// Query llama-server for supported compute devices.
    pub async fn query_devices(server_bin: &Path, timeout: Duration) -> Vec<String> {
        if !server_bin.is_file() {
            return vec![];
        }
        let output = match tokio::time::timeout(
            timeout,
            Command::new(server_bin)
                .arg("--list-devices")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await
        {
            Ok(Ok(out)) => out,
            _ => return vec![],
        };

        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let mut devices = Vec::new();
        for line in combined.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("Available devices:")
                || trimmed.contains("(none)")
            {
                continue;
            }
            devices.push(trimmed.to_string());
        }
        devices
    }

    /// Check if AMD GPU is detected in system.
    pub fn detect_gpu_name() -> (bool, Option<String>) {
        // Inspect /sys/bus/pci/devices or try lspci
        if let Ok(output) = std::process::Command::new("lspci").output() {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                if (line.contains("VGA compatible controller")
                    || line.contains("Display controller")
                    || line.contains("3D controller"))
                    && (line.contains("AMD")
                        || line.contains("Advanced Micro Devices")
                        || line.contains("Radeon"))
                {
                    return (true, Some(line.trim().to_string()));
                }
            }
        }
        (false, None)
    }

    /// Complete hardware diagnostics for local system and given server binary.
    pub async fn diagnose(server_bin: &Path) -> HardwareDiagnostics {
        let cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let cpu_threads = num_cpus();
        let (total_ram_mb, free_ram_mb) = Self::read_ram_mb();
        let (gpu_detected, gpu_name) = Self::detect_gpu_name();

        let render_path = PathBuf::from("/dev/dri/renderD128");
        let (render_device_accessible, permission_remediation) =
            Self::check_render_device_access(&render_path);

        let supported_devices = Self::query_devices(server_bin, Duration::from_secs(3)).await;

        HardwareDiagnostics {
            cpu_cores,
            cpu_threads,
            total_ram_mb,
            free_ram_mb,
            gpu_detected,
            gpu_name,
            render_device_accessible,
            permission_remediation,
            supported_devices,
        }
    }

    /// Verify whether a requested Vulkan device can be executed.
    pub async fn verify_vulkan_capability(
        server_bin: &Path,
        requested_device: &str,
        timeout: Duration,
    ) -> Result<()> {
        let render_path = PathBuf::from("/dev/dri/renderD128");
        let (accessible, remediation) = Self::check_render_device_access(&render_path);
        if !accessible {
            let msg = remediation.unwrap_or_else(|| "Render device is inaccessible".into());
            tracing::warn!(device = %requested_device, remediation = %msg, "Vulkan device node access failed");
            return Err(InferenceError::new(ErrorCode::UnsupportedCapability, msg));
        }

        let devices = Self::query_devices(server_bin, timeout).await;
        if devices.is_empty() || !devices.iter().any(|d| d.starts_with(requested_device)) {
            return Err(InferenceError::new(
                ErrorCode::UnsupportedCapability,
                format!(
                    "Requested Vulkan device '{}' not found in llama-server devices: {:?}",
                    requested_device, devices
                ),
            ));
        }

        Ok(())
    }
}

fn num_cpus() -> usize {
    if let Ok(content) = std::fs::read_to_string("/proc/cpuinfo") {
        content
            .lines()
            .filter(|l| l.starts_with("processor\t:"))
            .count()
            .max(1)
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    }
}
