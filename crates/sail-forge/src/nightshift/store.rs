//! K1: the errand store and its change broadcast.
//!
//! Stub in K0. The real store persists `<state_dir>/nightshift/errands.json` atomically
//! (tmp + rename, like [`crate::nightly::NightlyState::save`]) and keeps each patch at
//! `<state_dir>/nightshift/patches/<errand id>.patch`.

use chrono::{DateTime, Utc};

use super::{Errand, ErrandId, ErrandSource, ErrandStore, Transition};

impl ErrandStore {
    /// Open the store in `state_dir`. Stub until K1.
    pub fn open(state_dir: &std::path::Path) -> Result<Self, String> {
        let _ = state_dir;
        Err("the errand store is not implemented yet".into())
    }

    /// Add an errand. Stub until K1.
    pub fn add(
        &self,
        ask: &str,
        source: ErrandSource,
        now: DateTime<Utc>,
        rand6: [u8; 6],
    ) -> Result<Errand, String> {
        let _ = (ask, source, now, rand6);
        Err("the errand store is not implemented yet".into())
    }

    /// Look one errand up by id. Stub until K1.
    pub fn get(&self, id: &ErrandId) -> Option<Errand> {
        let _ = id;
        None
    }

    /// Errands, newest first. Stub until K1.
    pub fn list(&self) -> Vec<Errand> {
        Vec::new()
    }

    /// Apply a legal transition. Stub until K1.
    pub fn transition(
        &self,
        id: &ErrandId,
        now: DateTime<Utc>,
        f: Transition,
    ) -> Result<Errand, String> {
        let _ = (id, now, f);
        Err("the errand store is not implemented yet".into())
    }

    /// Store a patch (≤ 512 KB) and return its size. Stub until K1.
    pub fn save_patch(&self, id: &ErrandId, patch: &str) -> Result<u64, String> {
        let _ = (id, patch);
        Err("the errand store is not implemented yet".into())
    }

    /// Read a stored patch. Stub until K1.
    pub fn patch(&self, id: &ErrandId) -> Option<String> {
        let _ = id;
        None
    }

    /// Subscribe to every change. Stub until K1.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Errand> {
        self.events.subscribe()
    }
}
