use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::rest::normalise;
use crate::{MeshError, Result};

mod journal;
mod policy;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

pub use journal::recovery_plan;
pub use policy::eligible;

/// Fixed a priori: require at least 2 GiB free commit after trimming working sets
/// so the OS pagefile never starves when suspending apps.
pub const MIN_FREE_COMMIT_HEADROOM_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// One hibernated program, for display (the protocol type).
pub use sail_protocol::mesh::HibernatedApp;

/// One process as the OS reports it. `start_time` disambiguates reused PIDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcInfo {
    pub pid: u32,
    pub parent: Option<u32>,
    pub name: String,
    pub start_time: u64,
    pub working_set: u64,
    pub full_screen: bool,
    pub foreground: bool,
}

/// OS operations, faked in tests.
pub trait ProcessControl: Send {
    fn list(&mut self) -> Vec<ProcInfo>;
    fn suspend(&mut self, pid: u32) -> std::io::Result<()>;
    fn resume(&mut self, pid: u32) -> std::io::Result<()>;
    /// Release the working set to the pagefile. Returns bytes released (0 if unsupported).
    fn trim(&mut self, pid: u32) -> std::io::Result<u64>;
    /// Free commit (pagefile + RAM) in bytes, if known.
    fn free_commit(&mut self) -> Option<u64>;
}

/// Journal entry, written (fsync) before suspend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub pid: u32,
    pub name: String,
    pub start_time: u64,
    pub since: String,
    pub released_bytes: u64,
}

pub struct Hibernator {
    control: Box<dyn ProcessControl>,
    data_dir: PathBuf,
    listed: Vec<String>,
    journal: Vec<JournalEntry>,
}

impl Hibernator {
    pub fn new(control: Box<dyn ProcessControl>, data_dir: &Path) -> Self {
        let listed = journal::load_listed(&journal::apps_path(data_dir));
        let journal_entries = journal::load_journal(&journal::journal_path(data_dir));
        Self {
            control,
            data_dir: data_dir.to_path_buf(),
            listed,
            journal: journal_entries,
        }
    }

    /// Owner's list, persisted in `hibernate_apps.json` (names compared via `rest::normalise`).
    pub fn listed(&self) -> Vec<String> {
        self.listed.clone()
    }

    pub fn add(&mut self, names: &[String]) -> Result<()> {
        for raw in names {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let norm = normalise(trimmed);
            if !self
                .listed
                .iter()
                .any(|existing| normalise(existing) == norm)
            {
                self.listed.push(trimmed.to_owned());
            }
        }
        journal::save_listed(&journal::apps_path(&self.data_dir), &self.listed)
    }

    pub fn remove(&mut self, names: &[String]) -> Result<()> {
        let to_remove: Vec<String> = names.iter().map(|n| normalise(n)).collect();
        self.listed
            .retain(|existing| !to_remove.contains(&normalise(existing)));
        journal::save_listed(&journal::apps_path(&self.data_dir), &self.listed)
    }

    /// Called first thing on worker start. Returns processes resumed.
    pub fn recover(&mut self) -> Result<u32> {
        self.resume_all()
    }

    pub fn hibernate(&mut self, own: &[u32]) -> Result<Vec<HibernatedApp>> {
        let procs = self.control.list();
        let candidates = eligible(&procs, &self.listed, own);
        let pending: Vec<ProcInfo> = candidates
            .into_iter()
            .filter(|p| {
                !self
                    .journal
                    .iter()
                    .any(|j| j.pid == p.pid && j.start_time == p.start_time)
            })
            .collect();

        if pending.is_empty() {
            return Ok(self.hibernated());
        }

        let needed_ws: u64 = pending.iter().map(|p| p.working_set).sum();
        if let Some(free) = self.control.free_commit() {
            let required = needed_ws.saturating_add(MIN_FREE_COMMIT_HEADROOM_BYTES);
            if free < required {
                return Err(MeshError::new(format!(
                    "not enough free commit memory to hibernate apps (free {free} B < required {required} B)"
                )));
            }
        }

        let jpath = journal::journal_path(&self.data_dir);
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        for proc_ in pending {
            let entry = JournalEntry {
                pid: proc_.pid,
                name: proc_.name.clone(),
                start_time: proc_.start_time,
                since: now.clone(),
                released_bytes: 0,
            };
            self.journal.push(entry);
            journal::save_journal(&jpath, &self.journal)?;

            if let Err(err) = self.control.suspend(proc_.pid) {
                self.journal
                    .retain(|j| !(j.pid == proc_.pid && j.start_time == proc_.start_time));
                let _ = journal::save_journal(&jpath, &self.journal);
                return Err(MeshError::new(format!(
                    "failed to suspend {} ({}): {err}",
                    proc_.name, proc_.pid
                )));
            }

            if let Ok(released) = self.control.trim(proc_.pid)
                && let Some(last) = self
                    .journal
                    .iter_mut()
                    .find(|j| j.pid == proc_.pid && j.start_time == proc_.start_time)
            {
                last.released_bytes = released;
                let _ = journal::save_journal(&jpath, &self.journal);
            }
        }

        Ok(self.hibernated())
    }

    pub fn resume_all(&mut self) -> Result<u32> {
        if self.journal.is_empty() {
            return Ok(0);
        }
        let live = self.control.list();
        let (to_resume, stale) = recovery_plan(&self.journal, &live);
        let jpath = journal::journal_path(&self.data_dir);

        // Drop stale entries (vanished or reused PIDs).
        if !stale.is_empty() {
            self.journal.retain(|j| {
                !stale
                    .iter()
                    .any(|s| s.pid == j.pid && s.start_time == j.start_time)
            });
            journal::save_journal(&jpath, &self.journal)?;
        }

        let mut resumed = 0u32;
        let mut first_err: Option<MeshError> = None;
        for entry in to_resume {
            match self.control.resume(entry.pid) {
                Ok(()) => {
                    resumed += 1;
                    self.journal
                        .retain(|j| !(j.pid == entry.pid && j.start_time == entry.start_time));
                    journal::save_journal(&jpath, &self.journal)?;
                }
                Err(err) => {
                    if first_err.is_none() {
                        first_err = Some(MeshError::new(format!(
                            "failed to resume {} ({}): {err}",
                            entry.name, entry.pid
                        )));
                    }
                }
            }
        }

        if let Some(err) = first_err {
            return Err(err);
        }
        Ok(resumed)
    }

    /// True if a hibernated process got focus or went full-screen (owner is back).
    pub fn owner_returned(&mut self) -> bool {
        if self.journal.is_empty() {
            return false;
        }
        let live = self.control.list();
        self.journal.iter().any(|entry| {
            live.iter().any(|p| {
                p.pid == entry.pid
                    && p.start_time == entry.start_time
                    && (p.foreground || p.full_screen)
            })
        })
    }

    pub fn hibernated(&self) -> Vec<HibernatedApp> {
        let mut grouped: BTreeMap<String, HibernatedApp> = BTreeMap::new();
        for entry in &self.journal {
            let key = normalise(&entry.name);
            grouped
                .entry(key)
                .and_modify(|agg| {
                    agg.processes += 1;
                    agg.released_bytes = agg.released_bytes.saturating_add(entry.released_bytes);
                    if entry.since < agg.since {
                        agg.since = entry.since.clone();
                    }
                })
                .or_insert_with(|| HibernatedApp {
                    name: entry.name.clone(),
                    processes: 1,
                    released_bytes: entry.released_bytes,
                    since: entry.since.clone(),
                });
        }
        grouped.into_values().collect()
    }
}

/// The platform implementation (Windows or Unix).
pub fn system_control() -> Box<dyn ProcessControl> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsProcessControl::new())
    }
    #[cfg(unix)]
    {
        Box::new(unix::UnixProcessControl::new())
    }
}
