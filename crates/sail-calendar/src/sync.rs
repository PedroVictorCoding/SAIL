//! K4: diff a freshly parsed feed against its mirror and describe moves/cancellations.

use std::collections::HashMap;

use chrono::{DateTime, Duration, TimeZone, Utc};
use chrono_tz::Tz;

use crate::contracts::{Event, EventStatus, EventTime, FeedChange, FeedChangeKind};

/// A feed change is reported only when it starts within the next 14 days (fixed a priori).
const CHANGE_WINDOW_DAYS: i64 = 14;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncDiff {
    pub upserts: Vec<Event>,
    pub removed: Vec<String>,
    pub changes: Vec<FeedChange>,
}

/// Diff the mirror against the new parse. Pure.
pub fn diff(old: &[Event], new: &[Event], now: DateTime<Utc>, owner_tz: Tz) -> SyncDiff {
    let old_by_id: HashMap<&str, &Event> =
        old.iter().map(|event| (event.id.as_str(), event)).collect();
    let new_by_id: HashMap<&str, &Event> =
        new.iter().map(|event| (event.id.as_str(), event)).collect();
    let mut upserts = Vec::new();
    let mut changes = Vec::new();
    for event in new {
        let previous = old_by_id.get(event.id.as_str()).copied();
        if previous != Some(event) {
            upserts.push(event.clone());
        }
        if let Some(previous) = previous {
            if previous.status != EventStatus::Cancelled && event.status == EventStatus::Cancelled {
                if let Some(start) = event_start(previous, owner_tz)
                    && in_change_window(start, now)
                {
                    changes.push(FeedChange {
                        event_id: event.id.clone(),
                        source_id: event.source_id.clone(),
                        title: event.title.clone(),
                        kind: FeedChangeKind::Cancelled,
                        old_start: Some(start),
                        new_start: None,
                        observed_at: now,
                    });
                }
            } else if positive_status(previous) && positive_status(event) {
                let old_start = event_start(previous, owner_tz);
                let new_start = event_start(event, owner_tz);
                if old_start != new_start
                    && let Some(new_start_value) = new_start
                    && in_change_window(new_start_value, now)
                {
                    changes.push(FeedChange {
                        event_id: event.id.clone(),
                        source_id: event.source_id.clone(),
                        title: event.title.clone(),
                        kind: FeedChangeKind::Moved,
                        old_start,
                        new_start: Some(new_start_value),
                        observed_at: now,
                    });
                }
            }
        }
    }
    let mut removed: Vec<String> = old_by_id
        .keys()
        .filter(|id| !new_by_id.contains_key(**id))
        .map(|id| (*id).to_string())
        .collect();
    removed.sort();
    SyncDiff {
        upserts,
        removed,
        changes,
    }
}

fn positive_status(event: &Event) -> bool {
    matches!(
        event.status,
        EventStatus::Confirmed | EventStatus::Tentative
    )
}

fn in_change_window(start: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    start >= now && start <= now + Duration::days(CHANGE_WINDOW_DAYS)
}

/// All-day events use local midnight in the owner zone.
pub(crate) fn event_start(event: &Event, owner_tz: Tz) -> Option<DateTime<Utc>> {
    match &event.time {
        EventTime::Timed { start, .. } => Some(*start),
        EventTime::AllDay { start_date, .. } => owner_tz
            .from_local_datetime(&start_date.and_hms_opt(0, 0, 0)?)
            .earliest()
            .map(|value| value.with_timezone(&Utc)),
    }
}
