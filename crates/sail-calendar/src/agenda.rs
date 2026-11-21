//! K5: deterministic agenda lines for voice and Telegram.

use crate::contracts::{EventTime, Occurrence};

/// One line per occurrence, e.g. "09:00–10:30 Algorithms lecture (ISCTE)". Pure.
pub fn agenda_lines(
    occurrences: &[Occurrence],
    owner_tz: chrono_tz::Tz,
    max: usize,
) -> Vec<String> {
    occurrences
        .iter()
        .take(max)
        .map(|occurrence| match &occurrence.time {
            EventTime::Timed { start, end, .. } => {
                let start = start.with_timezone(&owner_tz);
                let end = end.with_timezone(&owner_tz);
                let location = occurrence
                    .location
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| format!(" ({value})"))
                    .unwrap_or_default();
                format!(
                    "{}–{} {}{}",
                    start.format("%H:%M"),
                    end.format("%H:%M"),
                    occurrence.title,
                    location
                )
            }
            EventTime::AllDay { start_date, .. } => {
                let _ = owner_tz;
                let location = occurrence
                    .location
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| format!(" ({value})"))
                    .unwrap_or_default();
                format!("All day {} ({}){}", occurrence.title, start_date, location)
            }
        })
        .collect()
}
