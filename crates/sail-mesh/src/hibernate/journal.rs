use std::io::Write;
use std::path::{Path, PathBuf};

use super::{JournalEntry, ProcInfo};
use crate::{MeshError, Result, write_private};

pub fn journal_path(data_dir: &Path) -> PathBuf {
    data_dir.join("hibernate.json")
}

pub fn apps_path(data_dir: &Path) -> PathBuf {
    data_dir.join("hibernate_apps.json")
}

/// Load journal entries; missing or malformed files return an empty list (never panic).
pub fn load_journal(path: &Path) -> Vec<JournalEntry> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Atomically write and fsync the hibernation journal before suspending processes (Invariant 3).
pub fn save_journal(path: &Path, entries: &[JournalEntry]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MeshError::new(e.to_string()))?;
    }
    let json = serde_json::to_vec_pretty(entries).map_err(|e| MeshError::new(e.to_string()))?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp).map_err(|e| MeshError::new(e.to_string()))?;
        file.write_all(&json)
            .map_err(|e| MeshError::new(e.to_string()))?;
        file.sync_all().map_err(|e| MeshError::new(e.to_string()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| MeshError::new(e.to_string()))?;
    Ok(())
}

pub fn load_listed(path: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn save_listed(path: &Path, names: &[String]) -> Result<()> {
    let json = serde_json::to_vec_pretty(names).map_err(|e| MeshError::new(e.to_string()))?;
    write_private(path, &json).map_err(|e| MeshError::new(e.to_string()))
}

/// Pure: (to resume, stale to drop) given the live list; a match needs pid AND start_time.
pub fn recovery_plan(
    journal: &[JournalEntry],
    live: &[ProcInfo],
) -> (Vec<JournalEntry>, Vec<JournalEntry>) {
    let mut to_resume = Vec::new();
    let mut stale = Vec::new();
    for entry in journal {
        let matches = live
            .iter()
            .any(|p| p.pid == entry.pid && p.start_time == entry.start_time);
        if matches {
            to_resume.push(entry.clone());
        } else {
            stale.push(entry.clone());
        }
    }
    (to_resume, stale)
}
