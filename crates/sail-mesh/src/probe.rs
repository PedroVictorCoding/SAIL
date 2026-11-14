//! Platform sampling for the usage monitor: how much the PC is used by programs other than
//! the worker and its llama-server. Turned into decisions by [`crate::rest::RestPolicy`].
//!
//! - CPU, memory and the process list come from `sysinfo` (Windows and Linux).
//! - GPU load comes from NVIDIA's driver library (`nvml.dll` / `libnvidia-ml.so.1`) when it
//!   is installed; without it the GPU is simply not measured.
//! - On Windows, the shell reports when a full-screen Direct3D game or another full-screen
//!   app is in front.
use crate::rest::{Reading, music_app, normalise};
use std::collections::HashMap;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

/// Process names the owner listed with `--pause-for`, normalised.
#[derive(Debug, Clone, Default)]
pub struct PauseList(Vec<(String, String)>);

impl PauseList {
    pub fn new(names: &[String]) -> Self {
        Self(
            names
                .iter()
                .map(|name| (normalise(name), name.trim().to_owned()))
                .filter(|(key, _)| !key.is_empty())
                .collect(),
        )
    }
    fn matches(&self, process_name: &str) -> Option<&str> {
        let name = normalise(process_name);
        self.0
            .iter()
            .find(|(key, _)| name.starts_with(key.as_str()))
            .map(|(_, label)| label.as_str())
    }
}

/// Why a process means full rest, if it does.
pub fn exclusive_process(process_name: &str, pause: &PauseList) -> Option<String> {
    if let Some(label) = pause.matches(process_name) {
        return Some(format!("paused for {label}"));
    }
    music_app(process_name).map(|label| format!("making music ({label})"))
}

struct Gpu {
    nvml: nvml_wrapper::Nvml,
    devices: u32,
    /// Newest per-process sample already read (NVML timestamps, microseconds).
    seen: u64,
}

pub struct SystemProbe {
    system: Option<System>,
    cores: f32,
    gpu: Option<Gpu>,
    pause: PauseList,
}

impl SystemProbe {
    pub fn new(pause: PauseList) -> Self {
        let mut system = System::new().ok();
        if let Some(system) = system.as_mut() {
            system.refresh_cpu_usage();
        }
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f32;
        let gpu = nvml_wrapper::Nvml::init().ok().and_then(|nvml| {
            let devices = nvml.device_count().ok().filter(|&n| n > 0)?;
            Some(Gpu {
                nvml,
                devices,
                seen: 0,
            })
        });
        Self {
            system,
            cores,
            gpu,
            pause,
        }
    }

    /// What the monitor can see, for the start-up message.
    pub fn describe(&self) -> String {
        let mut parts = vec![];
        if self.system.is_some() {
            parts.push("CPU, memory, programs");
        }
        if self.gpu.is_some() {
            parts.push("NVIDIA GPU");
        }
        if cfg!(windows) {
            parts.push("full-screen games");
        }
        if parts.is_empty() {
            "nothing (this system cannot be measured)".into()
        } else {
            parts.join(", ")
        }
    }

    /// Sample the PC. `own` are the worker's and its llama-server's process ids; their use
    /// is left out. `generating` says whether the worker is running a request now (used
    /// when the GPU cannot be measured per process).
    pub fn sample(&mut self, own: &[u32], generating: bool) -> Reading {
        let mut reading = Reading::default();
        if let Some(system) = self.system.as_mut() {
            system.refresh_cpu_usage();
            system.refresh_memory();
            system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing().with_cpu(),
            );
            // Per-process CPU is a share of one core; the global figure is a share of all.
            let own_cpu: f32 = own
                .iter()
                .filter_map(|pid| system.process(sysinfo::Pid::from_u32(*pid)))
                .map(|p| p.cpu_usage())
                .sum::<f32>()
                / self.cores;
            reading.other_cpu_percent = (system.global_cpu_usage() - own_cpu).clamp(0.0, 100.0);
            reading.free_ram_mb = Some(system.available_memory() / (1024 * 1024));
            reading.exclusive = system
                .processes()
                .values()
                .filter(|p| !own.contains(&p.pid().as_u32()))
                .find_map(|p| exclusive_process(&p.name().to_string_lossy(), &self.pause));
        }
        reading.other_gpu_percent = self.gpu_others(own, generating);
        let screen = full_screen();
        if screen == FullScreen::Game && reading.exclusive.is_none() {
            reading.exclusive = Some("gaming (full-screen game)".into());
        }
        reading.full_screen = screen != FullScreen::No;
        reading
    }

    /// GPU use by other programs, as the busiest GPU's share (0–100).
    fn gpu_others(&mut self, own: &[u32], generating: bool) -> Option<f32> {
        let gpu = self.gpu.as_mut()?;
        let mut busiest: Option<f32> = None;
        let mut newest = gpu.seen;
        for index in 0..gpu.devices {
            let Ok(device) = gpu.nvml.device_by_index(index) else {
                continue;
            };
            let others = match device.process_utilization_stats(gpu.seen) {
                Ok(samples) => {
                    // Several samples per process can arrive; keep each one's highest.
                    let mut per_process: HashMap<u32, u32> = HashMap::new();
                    for sample in samples {
                        newest = newest.max(sample.timestamp);
                        if !own.contains(&sample.pid) {
                            let entry = per_process.entry(sample.pid).or_default();
                            *entry = (*entry).max(sample.sm_util);
                        }
                    }
                    Some(per_process.values().sum::<u32>().min(100) as f32)
                }
                // No per-process figures (some drivers): the total is only usable while the
                // worker itself is idle.
                Err(_) if !generating => device.utilization_rates().ok().map(|u| u.gpu as f32),
                Err(_) => None,
            };
            if let Some(others) = others {
                busiest = Some(busiest.map_or(others, |b: f32| b.max(others)));
            }
        }
        gpu.seen = newest;
        busiest
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum FullScreen {
    No,
    /// A full-screen window: a video, a presentation, or a borderless game.
    App,
    /// Exclusive full-screen Direct3D: a game.
    Game,
}

#[cfg(windows)]
fn full_screen() -> FullScreen {
    use windows_sys::Win32::UI::Shell::{
        QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
        SHQueryUserNotificationState,
    };
    let mut state = 0;
    // SAFETY: the function writes one i32 through the pointer we pass.
    let result = unsafe { SHQueryUserNotificationState(&mut state) };
    if result != 0 {
        return FullScreen::No;
    }
    match state {
        QUNS_RUNNING_D3D_FULL_SCREEN => FullScreen::Game,
        QUNS_BUSY | QUNS_PRESENTATION_MODE => FullScreen::App,
        _ => FullScreen::No,
    }
}

#[cfg(not(windows))]
fn full_screen() -> FullScreen {
    FullScreen::No
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_listed_programs_pause_the_worker() {
        let pause = PauseList::new(&["Cyberpunk2077.exe".into(), "  ".into(), "obs64".into()]);
        assert_eq!(
            exclusive_process("Cyberpunk2077.exe", &pause).as_deref(),
            Some("paused for Cyberpunk2077.exe")
        );
        assert_eq!(
            exclusive_process("OBS64.EXE", &pause).as_deref(),
            Some("paused for obs64")
        );
        assert_eq!(
            exclusive_process("FL64.exe", &pause).as_deref(),
            Some("making music (FL Studio)")
        );
        assert_eq!(exclusive_process("chrome.exe", &pause), None);
    }

    #[test]
    fn sampling_this_machine_gives_sane_numbers() {
        let mut probe = SystemProbe::new(PauseList::default());
        let reading = probe.sample(&[std::process::id()], false);
        assert!((0.0..=100.0).contains(&reading.other_cpu_percent));
        assert!(reading.free_ram_mb.is_some_and(|mb| mb > 0));
        if let Some(gpu) = reading.other_gpu_percent {
            assert!((0.0..=100.0).contains(&gpu));
        }
    }
}
