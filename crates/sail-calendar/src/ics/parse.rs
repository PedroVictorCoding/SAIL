//! K1: unfold and parse an ICS calendar into events.

use chrono::{DateTime, Utc};

use crate::contracts::Event;

/// Parsed events plus plain-words warnings and the truncation flag.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCalendar {
    pub events: Vec<Event>,
    pub warnings: Vec<String>,
    pub truncated: bool,
}

/// Parse ICS text. Stub until K1.
pub fn parse(
    text: &str,
    source_id: &str,
    owner_tz: chrono_tz::Tz,
    now: DateTime<Utc>,
) -> ParsedCalendar {
    let _ = (text, source_id, owner_tz, now);
    ParsedCalendar {
        events: Vec::new(),
        warnings: Vec::new(),
        truncated: false,
    }
}
