//! Durable attention inbox (Antigravity). Spec: the module doc in `attention/mod.rs`.
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use super::{
    AttentionContext, AttentionItem, AttentionSettings, AttentionStats, DigestBatch, Feedback,
    Preference, Signal, Source,
};
use crate::Result;

pub struct AttentionStore {
    #[allow(dead_code)]
    conn: Mutex<Connection>,
}

impl AttentionStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let _ = path;
        unimplemented!("Antigravity: AttentionStore::open")
    }
    pub fn open_in_memory() -> Result<Self> {
        unimplemented!("Antigravity: AttentionStore::open_in_memory")
    }
    pub fn settings(&self) -> Result<AttentionSettings> {
        unimplemented!("Antigravity: AttentionStore::settings")
    }
    pub fn save_settings(&self, settings: &AttentionSettings) -> Result<()> {
        let _ = settings;
        unimplemented!("Antigravity: AttentionStore::save_settings")
    }
    /// Normalize, assess (with the stored preference and cooldown history) and store.
    /// `context.interrupts_last_hour` / `suggestions_today` are overwritten from the store's
    /// own counts. A `Drop` is stored too (state `Dismissed`) so it is observable.
    pub fn ingest(&self, signal: Signal, context: AttentionContext) -> Result<AttentionItem> {
        let _ = (signal, context);
        unimplemented!("Antigravity: AttentionStore::ingest")
    }
    /// `Interrupt` items still `Pending`, plus `Snoozed` items whose time has come; marks
    /// them `Delivered`. Oldest first.
    pub fn deliverable(&self, now: DateTime<Utc>) -> Result<Vec<AttentionItem>> {
        let _ = now;
        unimplemented!("Antigravity: AttentionStore::deliverable")
    }
    /// Collect pending `Digest` items into one batch (marks them `Digested`). `None` when empty.
    pub fn release_digest(&self, now: DateTime<Utc>) -> Result<Option<DigestBatch>> {
        let _ = now;
        unimplemented!("Antigravity: AttentionStore::release_digest")
    }
    /// Top pending `Suggest` items by score, not marked (the caller marks the one it raises).
    pub fn suggestions(&self, limit: usize) -> Result<Vec<AttentionItem>> {
        let _ = limit;
        unimplemented!("Antigravity: AttentionStore::suggestions")
    }
    pub fn mark_delivered(&self, id: &str, now: DateTime<Utc>) -> Result<()> {
        let _ = (id, now);
        unimplemented!("Antigravity: AttentionStore::mark_delivered")
    }
    /// Record feedback, update the learned preference and re-schedule the item.
    pub fn feedback(
        &self,
        id: &str,
        feedback: Feedback,
        now: DateTime<Utc>,
    ) -> Result<AttentionItem> {
        let _ = (id, feedback, now);
        unimplemented!("Antigravity: AttentionStore::feedback")
    }
    pub fn get(&self, id: &str) -> Result<AttentionItem> {
        let _ = id;
        unimplemented!("Antigravity: AttentionStore::get")
    }
    /// Items not `Dismissed`/`Done`, newest first.
    pub fn inbox(&self, limit: usize) -> Result<Vec<AttentionItem>> {
        let _ = limit;
        unimplemented!("Antigravity: AttentionStore::inbox")
    }
    pub fn preferences(&self) -> Result<Vec<Preference>> {
        unimplemented!("Antigravity: AttentionStore::preferences")
    }
    pub fn unmute(&self, source: Source, kind: &str) -> Result<()> {
        let _ = (source, kind);
        unimplemented!("Antigravity: AttentionStore::unmute")
    }
    pub fn stats(&self, now: DateTime<Utc>) -> Result<AttentionStats> {
        let _ = now;
        unimplemented!("Antigravity: AttentionStore::stats")
    }
    /// Delete `Dismissed`/`Done`/`Digested` items older than `keep_days`.
    pub fn prune(&self, keep_days: u32, now: DateTime<Utc>) -> Result<usize> {
        let _ = (keep_days, now);
        unimplemented!("Antigravity: AttentionStore::prune")
    }
}
