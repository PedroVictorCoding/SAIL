use super::ProcInfo;
use crate::rest::{MUSIC_APPS, normalise};

/// System and session process names that must never be hibernated (Invariant 4).
const SYSTEM_PROTECTED: &[&str] = &[
    "system",
    "smss",
    "csrss",
    "wininit",
    "winlogon",
    "services",
    "lsass",
    "svchost",
    "dwm",
    "explorer",
    "fontdrvhost",
    "sihost",
    "ctfmon",
    "audiodg",
    "msmpeng",
    "idle",
    "registry",
    "memory compression",
    "systemd",
    "init",
    "kthreadd",
];

fn is_system_protected(name: &str) -> bool {
    let norm = normalise(name);
    SYSTEM_PROTECTED.contains(&norm.as_str())
}

fn is_music_app(name: &str) -> bool {
    let norm = normalise(name);
    MUSIC_APPS
        .iter()
        .any(|(prefix, _)| norm.starts_with(prefix))
}

/// Pure: which processes may be hibernated now. `own` = the worker's PID and descendants.
pub fn eligible(procs: &[ProcInfo], listed: &[String], own: &[u32]) -> Vec<ProcInfo> {
    let listed_norm: Vec<String> = listed
        .iter()
        .map(|s| normalise(s))
        .filter(|s| !s.is_empty())
        .collect();

    if listed_norm.is_empty() {
        return Vec::new();
    }

    procs
        .iter()
        .filter(|p| {
            p.pid > 1
                && !own.contains(&p.pid)
                && !p.full_screen
                && !is_system_protected(&p.name)
                && !is_music_app(&p.name)
                && listed_norm.iter().any(|item| *item == normalise(&p.name))
        })
        .cloned()
        .collect()
}
