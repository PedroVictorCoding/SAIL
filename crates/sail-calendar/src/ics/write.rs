//! ICS export: CRLF lines folded at 75 octets.

use chrono::{DateTime, Utc};

use crate::contracts::{Event, EventStatus, EventTime};

/// RFC 5545 lines are at most 75 octets, excluding CRLF (fixed by the standard).
const MAX_OCTETS: usize = 75;

/// Render events as an ICS document. Pure and deterministic for the same events.
pub fn write(events: &[Event], prodid: &str) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        format!("PRODID:-//SAIL//{prodid}//EN"),
        "CALSCALE:GREGORIAN".to_string(),
    ];
    for event in events {
        lines.push("BEGIN:VEVENT".to_string());
        lines.push(format!("UID:{}", event.uid));
        lines.push(format!(
            "DTSTAMP:{}",
            event.updated_at.format("%Y%m%dT%H%M%SZ")
        ));
        match &event.time {
            EventTime::Timed { start, end, tz } => {
                let zone = tz.parse::<chrono_tz::Tz>().unwrap_or(chrono_tz::UTC);
                lines.push(format_timed("DTSTART", *start, zone));
                lines.push(format_timed("DTEND", *end, zone));
            }
            EventTime::AllDay {
                start_date,
                end_date,
            } => {
                lines.push(format!(
                    "DTSTART;VALUE=DATE:{}",
                    start_date.format("%Y%m%d")
                ));
                lines.push(format!("DTEND;VALUE=DATE:{}", end_date.format("%Y%m%d")));
            }
        }
        lines.push(format!("SUMMARY:{}", escape(&event.title)));
        if let Some(location) = &event.location {
            lines.push(format!("LOCATION:{}", escape(location)));
        }
        if let Some(notes) = &event.notes {
            lines.push(format!("DESCRIPTION:{}", escape(notes)));
        }
        match event.status {
            EventStatus::Confirmed => {}
            EventStatus::Tentative => lines.push("STATUS:TENTATIVE".into()),
            EventStatus::Cancelled => lines.push("STATUS:CANCELLED".into()),
        }
        if let Some(rule) = &event.rrule {
            lines.push(format!("RRULE:{}", rule));
        }
        if !event.exdates.is_empty() {
            lines.push(format!("EXDATE:{}", event.exdates.join(",")));
        }
        if let Some(recurrence_id) = &event.recurrence_id {
            lines.push(format!("RECURRENCE-ID:{}", recurrence_id));
        }
        lines.push("END:VEVENT".to_string());
    }
    lines.push("END:VCALENDAR".to_string());

    let mut out = String::new();
    for line in lines {
        for folded in fold(&line) {
            out.push_str(&folded);
            out.push_str("\r\n");
        }
    }
    out
}

fn format_timed(name: &str, value: DateTime<Utc>, zone: chrono_tz::Tz) -> String {
    let local = value.with_timezone(&zone);
    format!(
        "{name};TZID={}:{}",
        zone.name(),
        local.format("%Y%m%dT%H%M%S")
    )
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            other => out.push(other),
        }
    }
    out
}

/// Fold one logical line at 75 UTF-8 octets. Continuation lines start with one space, so
/// they carry at most 74 content octets.
fn fold(line: &str) -> Vec<String> {
    if line.len() <= MAX_OCTETS {
        return vec![line.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in line.chars() {
        let limit = if out.is_empty() {
            MAX_OCTETS
        } else {
            MAX_OCTETS - 1
        };
        if current.len() + ch.len_utf8() > limit && !current.is_empty() {
            out.push(if out.is_empty() {
                current.clone()
            } else {
                format!(" {current}")
            });
            current.clear();
        }
        current.push(ch);
    }
    if !current.is_empty() {
        out.push(if out.is_empty() {
            current
        } else {
            format!(" {current}")
        });
    }
    out
}
