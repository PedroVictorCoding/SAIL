//! K2: recurrence expansion and preset RRULEs.

use chrono::{DateTime, NaiveDate, Utc};

use crate::contracts::{Event, EventTime, Occurrence, RepeatPreset};

/// Expand events into occurrences. Stub until K2.
pub fn expand(
    events: &[Event],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    owner_tz: chrono_tz::Tz,
    include_cancelled: bool,
) -> (Vec<Occurrence>, bool) {
    let _ = (events, start, end, owner_tz, include_cancelled);
    (Vec::new(), false)
}

/// The RRULE for a preset. Stub until K2.
pub fn preset_rrule(
    repeat: RepeatPreset,
    until: Option<NaiveDate>,
    first: &EventTime,
) -> Option<String> {
    let _ = (repeat, until, first);
    None
}
