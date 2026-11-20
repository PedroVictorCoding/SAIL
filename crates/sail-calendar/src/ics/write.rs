//! K1: export events as a folded ICS document.

use crate::contracts::Event;

/// Render events as an ICS document. Stub until K1.
pub fn write(events: &[Event], prodid: &str) -> String {
    let _ = (events, prodid);
    String::new()
}
