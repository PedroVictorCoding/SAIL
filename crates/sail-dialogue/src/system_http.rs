//! Host metrics for the Tools Map view, nested at `/api/system` (owner token required).
//!
//! `GET /metrics` reads Linux `/proc` and `/sys` directly: CPU use since the previous call,
//! memory, the fullest SAIL-relevant filesystems, block-device throughput and, where the
//! driver exposes it (amdgpu), GPU busy percent and VRAM. Anything unavailable is `null`.
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use sail_guardian::authority::ApprovalAuthority;
use serde::Serialize;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
struct Counters {
    at: Instant,
    cpu_total: u64,
    cpu_idle: u64,
    io_sectors: u64,
}

struct SystemState {
    authority: Option<ApprovalAuthority>,
    previous: Mutex<Option<Counters>>,
}

pub fn router(authority: Option<ApprovalAuthority>) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .with_state(Arc::new(SystemState {
            authority,
            previous: Mutex::new(None),
        }))
}

/// `(total, idle)` jiffies from the aggregate `cpu` line of `/proc/stat`. Idle includes iowait.
pub fn parse_proc_stat(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|x| x.parse().ok())
        .collect();
    if v.len() < 4 {
        return None;
    }
    // user nice system idle iowait irq softirq steal (guest time is already in user/nice)
    let total: u64 = v.iter().take(8).sum();
    let idle = v[3] + v.get(4).copied().unwrap_or(0);
    Some((total, idle))
}

/// `(total, available, swap_total, swap_free)` in bytes from `/proc/meminfo`.
pub fn parse_meminfo(text: &str) -> Option<(u64, u64, u64, u64)> {
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kb| kb.parse::<u64>().ok())
            .map(|kb| kb * 1024)
    };
    Some((
        field("MemTotal:")?,
        field("MemAvailable:")?,
        field("SwapTotal:").unwrap_or(0),
        field("SwapFree:").unwrap_or(0),
    ))
}

/// Sectors read plus written across whole disks named in `disks` (partitions would double count).
pub fn parse_diskstats(text: &str, disks: &HashSet<String>) -> u64 {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 10 || !disks.contains(f[2]) {
                return None;
            }
            Some(f[5].parse::<u64>().unwrap_or(0) + f[9].parse::<u64>().unwrap_or(0))
        })
        .sum()
}

fn whole_disks() -> HashSet<String> {
    std::fs::read_dir("/sys/block")
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| {
                    !n.starts_with("loop") && !n.starts_with("ram") && !n.starts_with("zram")
                })
                .collect()
        })
        .unwrap_or_default()
}

fn read_counters() -> Option<Counters> {
    let (cpu_total, cpu_idle) = parse_proc_stat(&std::fs::read_to_string("/proc/stat").ok()?)?;
    let io_sectors = std::fs::read_to_string("/proc/diskstats")
        .map(|t| parse_diskstats(&t, &whole_disks()))
        .unwrap_or(0);
    Some(Counters {
        at: Instant::now(),
        cpu_total,
        cpu_idle,
        io_sectors,
    })
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Disk {
    pub path: String,
    pub used_bytes: u64,
    pub total_bytes: u64,
}

#[cfg(unix)]
fn disk(path: &Path) -> Option<Disk> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `s` is a properly sized out-parameter.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let frsize = s.f_frsize as u64;
    let total = s.f_blocks as u64 * frsize;
    let free = s.f_bfree as u64 * frsize;
    Some(Disk {
        path: path.display().to_string(),
        used_bytes: total.saturating_sub(free),
        total_bytes: total,
    })
}

#[cfg(not(unix))]
fn disk(_: &Path) -> Option<Disk> {
    None
}

/// The root filesystem plus SAIL's runtime and data locations, one entry per device.
fn disks() -> Vec<Disk> {
    let mut paths = vec![PathBuf::from("/")];
    paths.push(
        std::env::var_os("SAIL_RUNTIME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/srv/sail")),
    );
    paths.push(sail_common::storage::data_root());
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in paths {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let Ok(meta) = std::fs::metadata(&p) else {
                continue;
            };
            if !seen.insert(meta.dev()) {
                continue;
            }
        }
        #[cfg(not(unix))]
        let _ = &mut seen;
        if let Some(d) = disk(&p).filter(|d| d.total_bytes > 0) {
            out.push(d);
        }
    }
    out
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Gpu {
    pub card: String,
    pub busy_percent: Option<f64>,
    pub vram_used_bytes: Option<u64>,
    pub vram_total_bytes: Option<u64>,
}

fn read_num(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// GPUs whose driver publishes utilisation in sysfs (amdgpu does). A GPU that has fallen
/// off the bus fails these reads and is omitted.
fn gpus() -> Vec<Gpu> {
    let Ok(dir) = std::fs::read_dir("/sys/class/drm") else {
        return vec![];
    };
    let mut cards: Vec<_> = dir
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("card") && !n.contains('-'))
        .collect();
    cards.sort();
    cards
        .into_iter()
        .filter_map(|card| {
            let dev = Path::new("/sys/class/drm").join(&card).join("device");
            let busy = read_num(&dev.join("gpu_busy_percent"));
            busy?;
            Some(Gpu {
                card,
                busy_percent: busy.map(|b| b as f64),
                vram_used_bytes: read_num(&dev.join("mem_info_vram_used")),
                vram_total_bytes: read_num(&dev.join("mem_info_vram_total")),
            })
        })
        .collect()
}

fn authorize(s: &SystemState, h: &HeaderMap) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let deny = |code| (code, Json(json!({"error":"Owner authentication required"})));
    let a = s
        .authority
        .as_ref()
        .ok_or_else(|| deny(StatusCode::SERVICE_UNAVAILABLE))?;
    let token = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| deny(StatusCode::UNAUTHORIZED))?;
    if !a.authenticates(token) {
        return Err(deny(StatusCode::UNAUTHORIZED));
    }
    Ok(())
}

/// CPU percent and IO rate between two counter samples.
pub fn rates(a: (u64, u64, u64), b: (u64, u64, u64), secs: f64) -> (Option<f64>, Option<f64>) {
    let (total, idle, io) = (
        b.0.saturating_sub(a.0),
        b.1.saturating_sub(a.1),
        b.2.saturating_sub(a.2),
    );
    let cpu = (total > 0).then(|| (total.saturating_sub(idle)) as f64 / total as f64 * 100.0);
    let io = (secs > 0.0).then(|| io as f64 * 512.0 / secs);
    (cpu, io)
}

async fn metrics(
    State(s): State<Arc<SystemState>>,
    h: HeaderMap,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    authorize(&s, &h)?;
    let mut previous = *s.previous.lock().unwrap_or_else(|e| e.into_inner());
    if previous.is_none_or(|p| p.at.elapsed() > Duration::from_secs(60)) {
        previous = read_counters();
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let now = read_counters();
    *s.previous.lock().unwrap_or_else(|e| e.into_inner()) = now;
    let (cpu_percent, io_bytes_per_sec) = match (previous, now) {
        (Some(a), Some(b)) => rates(
            (a.cpu_total, a.cpu_idle, a.io_sectors),
            (b.cpu_total, b.cpu_idle, b.io_sectors),
            b.at.duration_since(a.at).as_secs_f64(),
        ),
        _ => (None, None),
    };
    let mem = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| parse_meminfo(&t));
    let load1 = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|t| t.split_whitespace().next()?.parse::<f64>().ok());
    let uptime = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|t| t.split_whitespace().next()?.parse::<f64>().ok());
    let body = json!({
        "cpu_percent": cpu_percent,
        "cpu_cores": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "load1": load1,
        "ram_total_bytes": mem.map(|m| m.0),
        "ram_used_bytes": mem.map(|m| m.0.saturating_sub(m.1)),
        "swap_total_bytes": mem.map(|m| m.2),
        "swap_used_bytes": mem.map(|m| m.2.saturating_sub(m.3)),
        "io_bytes_per_sec": io_bytes_per_sec,
        "disks": disks(),
        "gpus": gpus(),
        "uptime_secs": uptime,
        "version": env!("CARGO_PKG_VERSION"),
    });
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[test]
    fn parses_proc_stat() {
        let t = "cpu  100 5 50 800 20 3 2 0 0 0\ncpu0 1 1 1 1\n";
        assert_eq!(parse_proc_stat(t), Some((980, 820)));
        assert_eq!(parse_proc_stat("intr 1 2"), None);
    }

    #[test]
    fn parses_meminfo() {
        let t = "MemTotal:       1000 kB\nMemFree: 10 kB\nMemAvailable:    400 kB\nSwapTotal: 200 kB\nSwapFree: 50 kB\n";
        assert_eq!(
            parse_meminfo(t),
            Some((1_024_000, 409_600, 204_800, 51_200))
        );
        assert_eq!(parse_meminfo("MemTotal: 5 kB"), None);
    }

    #[test]
    fn diskstats_counts_whole_disks_only() {
        let t = "   8       0 sda 10 0 100 0 5 0 50 0 0 0 0\n   8       1 sda1 10 0 100 0 5 0 50 0 0 0 0\n   7 0 loop0 1 0 999 0 1 0 999 0 0 0 0\n";
        let disks: HashSet<String> = ["sda".to_string()].into();
        assert_eq!(parse_diskstats(t, &disks), 150);
    }

    #[test]
    fn rates_between_samples() {
        let (cpu, io) = rates((1000, 800, 0), (1100, 850, 2048), 2.0);
        assert_eq!(cpu, Some(50.0));
        assert_eq!(io, Some(524_288.0));
        assert_eq!(rates((1, 1, 0), (1, 1, 0), 0.0), (None, None));
    }

    #[tokio::test]
    async fn metrics_require_owner_and_report_host() {
        const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";
        let app = router(Some(ApprovalAuthority::new(TOKEN).unwrap()));
        let r = app
            .clone()
            .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let r = app
            .oneshot(
                Request::get("/metrics")
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["disks"].is_array() && v["gpus"].is_array());
        if cfg!(target_os = "linux") {
            assert!(v["ram_total_bytes"].as_u64().unwrap() > 0);
            assert!(v["cpu_percent"].as_f64().is_some());
        }
    }
}
