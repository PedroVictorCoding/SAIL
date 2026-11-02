use sail_mind::HardwareProbe;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[test]
fn ram_probe_reads_non_zero_on_linux() {
    let (total, free) = HardwareProbe::read_ram_mb();
    // On Linux systems, total RAM should be non-zero
    if Path::new("/proc/meminfo").exists() {
        assert!(total > 0);
        assert!(free > 0);
        assert!(total >= free);
    }
}

#[test]
fn render_device_node_check_returns_remediation_on_permission_denial() {
    let render_node = Path::new("/dev/dri/renderD128");
    let (accessible, remediation) = HardwareProbe::check_render_device_access(render_node);
    if render_node.exists() && !accessible {
        assert!(remediation.is_some());
        let msg = remediation.unwrap();
        assert!(msg.contains("sudo usermod -a -G render,video"));
    }
}

#[tokio::test]
async fn verify_vulkan_capability_fails_closed_when_device_inaccessible() {
    let server_bin = PathBuf::from("/home/main/llama.cpp/build/bin/llama-server");
    let result =
        HardwareProbe::verify_vulkan_capability(&server_bin, "Vulkan0", Duration::from_millis(500))
            .await;

    // In current environment where /dev/dri/renderD128 is not accessible by main user
    // or llama-server lacks Vulkan0, verify_vulkan_capability MUST return an error.
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(
        err.code,
        sail_mind::protocol::ErrorCode::UnsupportedCapability
    );
}

#[tokio::test]
async fn hardware_diagnostics_reports_all_system_parameters() {
    let server_bin = PathBuf::from("/home/main/llama.cpp/build/bin/llama-server");
    let diag = HardwareProbe::diagnose(&server_bin).await;

    assert!(diag.cpu_cores >= 1);
    assert!(diag.cpu_threads >= diag.cpu_cores);
    if Path::new("/proc/meminfo").exists() {
        assert!(diag.total_ram_mb > 0);
    }
}
