//! K4: diff a freshly parsed feed against the mirror.

use chrono::{DateTime, Utc};

use crate::contracts::{Event, FeedChange};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncDiff {
    pub upserts: Vec<Event>,
    pub removed: Vec<String>,
    pub changes: Vec<FeedChange>,
}

/// Diff old mirror against new feed. Stub until K4.
pub fn diff(old: &[Event], new: &[Event], now: DateTime<Utc>, owner_tz: chrono_tz::Tz) -> SyncDiff {
    let _ = (old, new, now, owner_tz);
    SyncDiff::default()
}
