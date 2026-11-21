//! Reversible hibernation of the owner's programs while the PC lends compute.
//!
//! Journal before suspend, resume everything on start, and never touch the system, the
//! worker itself, music apps, full-screen windows or anything the owner did not list.
use sail_protocol::mesh::HibernatedApp;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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

/// Pure: which processes may be hibernated now. `own` = the worker's PID and descendants.
pub fn eligible(procs: &[ProcInfo], listed: &[String], own: &[u32]) -> Vec<ProcInfo> {
    let _ = (procs, listed, own);
    Vec::new()
}

/// Pure: (to resume, stale to drop) given the live list; a match needs pid AND start_time.
pub fn recovery_plan(
    journal: &[JournalEntry],
    live: &[ProcInfo],
) -> (Vec<JournalEntry>, Vec<JournalEntry>) {
    let _ = (journal, live);
    (Vec::new(), Vec::new())
}

/// Owns the OS control, the owner's app list and the journal.
pub struct Hibernator {
    control: Box<dyn ProcessControl>,
    data_dir: PathBuf,
    listed: Vec<String>,
}

impl Hibernator {
    pub fn new(control: Box<dyn ProcessControl>, data_dir: &Path) -> Self {
        Self {
            control,
            data_dir: data_dir.to_path_buf(),
            listed: Vec::new(),
        }
    }

    /// Owner's list, persisted in `hibernate_apps.json` (names compared via `rest::normalise`).
    pub fn listed(&self) -> Vec<String> {
        self.listed.clone()
    }

    pub fn add(&mut self, names: &[String]) -> crate::Result<()> {
        let _ = names;
        Err(crate::MeshError::new("hibernation is not implemented yet"))
    }

    pub fn remove(&mut self, names: &[String]) -> crate::Result<()> {
        let _ = names;
        Err(crate::MeshError::new("hibernation is not implemented yet"))
    }

    /// Called first thing on worker start. Returns processes resumed.
    pub fn recover(&mut self) -> crate::Result<u32> {
        Err(crate::MeshError::new("hibernation is not implemented yet"))
    }

    pub fn hibernate(&mut self, own: &[u32]) -> crate::Result<Vec<HibernatedApp>> {
        let _ = own;
        Err(crate::MeshError::new("hibernation is not implemented yet"))
    }

    pub fn resume_all(&mut self) -> crate::Result<u32> {
        Err(crate::MeshError::new("hibernation is not implemented yet"))
    }

    /// True if a hibernated process got focus or went full-screen (owner is back).
    pub fn owner_returned(&mut self) -> bool {
        false
    }

    pub fn hibernated(&self) -> Vec<HibernatedApp> {
        Vec::new()
    }
}

/// The platform implementation (Windows or Unix).
pub fn system_control() -> Box<dyn ProcessControl> {
    Box::new(Unsupported)
}

/// Placeholder until the platform implementation lands.
struct Unsupported;

impl ProcessControl for Unsupported {
    fn list(&mut self) -> Vec<ProcInfo> {
        Vec::new()
    }

    fn suspend(&mut self, _pid: u32) -> std::io::Result<()> {
        Err(std::io::Error::other("hibernation is not implemented yet"))
    }

    fn resume(&mut self, _pid: u32) -> std::io::Result<()> {
        Err(std::io::Error::other("hibernation is not implemented yet"))
    }

    fn trim(&mut self, _pid: u32) -> std::io::Result<u64> {
        Err(std::io::Error::other("hibernation is not implemented yet"))
    }

    fn free_commit(&mut self) -> Option<u64> {
        None
    }
}
