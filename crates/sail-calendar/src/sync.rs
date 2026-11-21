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
        if !previous.is_some_and(|previous| same_content(previous, event)) {
            upserts.push(event.clone());
        }
        if previous.is_none()
            && let Some(change) = new_override_change(event, now, owner_tz)
        {
            changes.push(change);
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
    // Google and Outlook drop a deleted event from the feed instead of marking it
    // CANCELLED, so a vanished one-off event in the window is a cancellation too.
    // Whole recurring series that vanish are not reported (no single instance to name).
    for id in &removed {
        let Some(previous) = old_by_id.get(id.as_str()) else {
            continue;
        };
        if previous.rrule.is_some()
            || previous.recurrence_id.is_some()
            || !positive_status(previous)
        {
            continue;
        }
        if let Some(start) = event_start(previous, owner_tz)
            && in_change_window(start, now)
        {
            changes.push(FeedChange {
                event_id: previous.id.clone(),
                source_id: previous.source_id.clone(),
                title: previous.title.clone(),
                kind: FeedChangeKind::Cancelled,
                old_start: Some(start),
                new_start: None,
                observed_at: now,
            });
        }
    }
    SyncDiff {
        upserts,
        removed,
        changes,
    }
}

/// Equal for the mirror's purposes. `updated_at` follows DTSTAMP, which Google rewrites
/// on every export, and `prepare` is SAIL's own annotation (stored apart), so neither
/// makes a feed event "changed".
fn same_content(old: &Event, new: &Event) -> bool {
    let mut old = old.clone();
    old.updated_at = new.updated_at;
    old.prepare = new.prepare;
    &old == new
}

/// A first-seen override (RECURRENCE-ID) is how Google and Outlook move or cancel one
/// instance of a series. Report it when its instance is in the window and it really
/// changes the start time or cancels the instance.
fn new_override_change(event: &Event, now: DateTime<Utc>, owner_tz: Tz) -> Option<FeedChange> {
    let original = event.recurrence_id.as_deref()?;
    let original_key = crate::recur::recurrence_key(event, original, owner_tz)?;
    let original_start = DateTime::parse_from_rfc3339(&original_key)
        .map(|value| value.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(&original_key, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .and_then(|midnight| owner_tz.from_local_datetime(&midnight).earliest())
                .map(|value| value.with_timezone(&Utc))
        })?;
    if event.status == EventStatus::Cancelled {
        return in_change_window(original_start, now).then(|| FeedChange {
            event_id: event.id.clone(),
            source_id: event.source_id.clone(),
            title: event.title.clone(),
            kind: FeedChangeKind::Cancelled,
            old_start: Some(original_start),
            new_start: None,
            observed_at: now,
        });
    }
    let new_start = event_start(event, owner_tz)?;
    if new_start == original_start || !in_change_window(new_start, now) {
        return None;
    }
    Some(FeedChange {
        event_id: event.id.clone(),
        source_id: event.source_id.clone(),
        title: event.title.clone(),
        kind: FeedChangeKind::Moved,
        old_start: Some(original_start),
        new_start: Some(new_start),
        observed_at: now,
    })
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
