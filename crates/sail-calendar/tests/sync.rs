//! K4: feed diff upserts, removals and moved/cancelled changes.

mod common;

use chrono::Duration;
use common::{at, now, timed_event};
use sail_calendar::contracts::{EventStatus, FeedChangeKind};
use sail_calendar::sync::diff;

#[test]
fn new_changed_and_removed_events_are_reported() {
    let old = vec![
        timed_event("fe_a", "A", at(25, 9, 0), at(25, 10, 0)),
        timed_event("fe_b", "B", at(26, 9, 0), at(26, 10, 0)),
    ];
    let mut moved = timed_event("fe_a", "A", at(27, 9, 0), at(27, 10, 0));
    moved.updated_at = now();
    let new = vec![
        moved.clone(),
        timed_event("fe_c", "C", at(28, 9, 0), at(28, 10, 0)),
    ];
    let delta = diff(&old, &new, now(), chrono_tz::Europe::Lisbon);
    assert_eq!(delta.upserts.len(), 2);
    assert_eq!(delta.removed, vec!["fe_b"]);
    // A moved, and B vanished from the feed inside the window: Google and Outlook drop
    // deleted events instead of marking them CANCELLED, so that is a cancellation.
    assert_eq!(delta.changes.len(), 2);
    assert_eq!(delta.changes[0].kind, FeedChangeKind::Moved);
    assert_eq!(delta.changes[0].old_start, Some(at(25, 9, 0)));
    assert_eq!(delta.changes[0].new_start, Some(at(27, 9, 0)));
    assert_eq!(delta.changes[1].event_id, "fe_b");
    assert_eq!(delta.changes[1].kind, FeedChangeKind::Cancelled);
    assert_eq!(delta.changes[1].old_start, Some(at(26, 9, 0)));
}

#[test]
fn cancellations_inside_fourteen_days_are_reported_once() {
    let old = vec![timed_event("fe_a", "A", at(25, 9, 0), at(25, 10, 0))];
    let mut cancelled = old[0].clone();
    cancelled.status = EventStatus::Cancelled;
    let delta = diff(&old, &[cancelled], now(), chrono_tz::Europe::Lisbon);
    assert_eq!(delta.changes.len(), 1);
    assert_eq!(delta.changes[0].kind, FeedChangeKind::Cancelled);
    assert_eq!(delta.changes[0].old_start, Some(at(25, 9, 0)));
    assert_eq!(delta.changes[0].new_start, None);

    let unchanged = diff(&old, &old, now(), chrono_tz::Europe::Lisbon);
    assert!(unchanged.upserts.is_empty());
    assert!(unchanged.changes.is_empty());
}

#[test]
fn changes_beyond_fourteen_days_are_not_announced() {
    let old = vec![timed_event("fe_a", "A", at(20, 9, 0), at(20, 10, 0))];
    let mut late = old[0].clone();
    late.time = sail_calendar::contracts::EventTime::Timed {
        start: at(20, 9, 0) + Duration::days(15),
        end: at(20, 10, 0) + Duration::days(15),
        tz: "Europe/Lisbon".into(),
    };
    let delta = diff(&old, &[late], now(), chrono_tz::Europe::Lisbon);
    assert!(delta.changes.is_empty());
}
