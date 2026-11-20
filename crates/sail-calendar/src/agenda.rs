//! K5: deterministic agenda lines.

use crate::contracts::Occurrence;

/// One line per occurrence, e.g. "09:00–10:30 Algorithms lecture (ISCTE)". Stub until K5.
pub fn agenda_lines(
    occurrences: &[Occurrence],
    owner_tz: chrono_tz::Tz,
    max: usize,
) -> Vec<String> {
    let _ = (occurrences, owner_tz, max);
    Vec::new()
}
