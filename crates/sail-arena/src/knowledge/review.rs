//! The owner's review queue for model-proposed memory changes.
//!
//! When review is enabled, a change set waits in `<dir>/memory-review.json` (0600,
//! atomic) until the owner saves or discards it. This module only keeps the queue; the
//! caller validates and applies a saved change through the normal knowledge path.
use super::memory_scan::ScanFinding;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

const MAX_PENDING: usize = 100;
const MAX_AGE_SECS: i64 = 14 * 24 * 60 * 60;

/// A model-proposed change set waiting for the owner, when review is on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingChange {
    /// 32 lowercase hex characters, given by the caller.
    pub id: String,
    /// The MemoryChangeSet JSON, opaque here.
    pub change: Value,
    /// One line per operation for the owner, rendered by Claude.
    pub summary: Vec<String>,
    /// What the injection scan found, if anything.
    pub findings: Vec<ScanFinding>,
    /// The session that proposed the change.
    pub session: String,
    /// Unix seconds.
    pub proposed_at: i64,
}

/// The persisted queue.
#[derive(Debug, Serialize, Deserialize)]
struct ReviewFile {
    version: u32,
    enabled: bool,
    pending: Vec<PendingChange>,
}

impl Default for ReviewFile {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: false,
            pending: Vec::new(),
        }
    }
}

/// `<dir>/memory-review.json` (0o600). One process-wide mutex.
pub struct ReviewQueue {
    /// Directory holding `memory-review.json`.
    pub dir: PathBuf,
}

static REVIEW_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

impl ReviewQueue {
    /// Whether review is on. A missing file means off.
    pub fn enabled(&self) -> Result<bool, String> {
        let _guard = lock();
        Ok(self.load()?.enabled)
    }

    /// Turn review on or off.
    pub fn set_enabled(&self, on: bool) -> Result<(), String> {
        let _guard = lock();
        let mut file = self.load()?;
        file.enabled = on;
        self.save(&file)
    }

    /// Queue a change. When the queue is full, the oldest change is dropped and returned.
    pub fn push(&self, change: PendingChange) -> Result<Option<PendingChange>, String> {
        let _guard = lock();
        let mut file = self.load()?;
        let dropped = if file.pending.len() >= MAX_PENDING {
            let oldest = file
                .pending
                .iter()
                .enumerate()
                .min_by_key(|(_, pending)| pending.proposed_at)
                .map(|(index, _)| index)
                .unwrap_or(0);
            Some(file.pending.remove(oldest))
        } else {
            None
        };
        file.pending.push(change);
        self.save(&file)?;
        Ok(dropped)
    }

    /// Pending changes, newest first.
    pub fn list(&self) -> Result<Vec<PendingChange>, String> {
        let _guard = lock();
        let mut pending = self.load()?.pending;
        pending.reverse();
        pending.sort_by_key(|left| std::cmp::Reverse(left.proposed_at));
        Ok(pending)
    }

    /// Remove and return one change (the caller then commits it through validation).
    pub fn take(&self, id: &str) -> Result<PendingChange, String> {
        let _guard = lock();
        let mut file = self.load()?;
        let Some(position) = file.pending.iter().position(|change| change.id == id) else {
            return Err("that change is not waiting".to_string());
        };
        let change = file.pending.remove(position);
        self.save(&file)?;
        Ok(change)
    }

    /// Discard one change.
    pub fn reject(&self, id: &str) -> Result<(), String> {
        let _guard = lock();
        let mut file = self.load()?;
        let Some(position) = file.pending.iter().position(|change| change.id == id) else {
            return Err("that change is not waiting".to_string());
        };
        file.pending.remove(position);
        self.save(&file)
    }

    /// Drop pending changes older than 14 days; returns how many were dropped.
    pub fn prune(&self, now: i64) -> Result<usize, String> {
        let _guard = lock();
        let mut file = self.load()?;
        let before = file.pending.len();
        file.pending
            .retain(|change| now.saturating_sub(change.proposed_at) <= MAX_AGE_SECS);
        let removed = before - file.pending.len();
        if removed > 0 {
            self.save(&file)?;
        }
        Ok(removed)
    }

    fn load(&self) -> Result<ReviewFile, String> {
        let path = self.dir.join("memory-review.json");
        let file = read_json::<ReviewFile>(&path)?.unwrap_or_default();
        if file.version != 1 {
            return Err(format!("unsupported review version {}", file.version));
        }
        Ok(file)
    }

    fn save(&self, file: &ReviewFile) -> Result<(), String> {
        self.write_json(&self.dir.join("memory-review.json"), file)
    }

    fn write_json<T: Serialize>(&self, path: &std::path::Path, value: &T) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        let mut name = path.as_os_str().to_owned();
        name.push(".tmp");
        let tmp = PathBuf::from(name);
        let _ = fs::remove_file(&tmp);
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .map_err(|error| format!("cannot write memory review: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        }
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);
        fs::rename(&tmp, path).map_err(|error| error.to_string())?;
        if let Some(parent) = path.parent()
            && let Ok(dir) = File::open(parent)
        {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    match REVIEW_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Result<Option<T>, String> {
    match fs::read(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read memory review: {error}")),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("corrupt memory review: {error}")),
    }
}
