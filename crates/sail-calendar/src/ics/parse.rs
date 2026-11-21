//! ICS parsing: unfold, read VEVENTs, resolve dates and time zones, keep warnings.

use chrono::{DateTime, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use sha2::{Digest, Sha256};

use crate::contracts::{Event, EventStatus, EventTime, RepeatPreset};

/// At most 20 000 VEVENTs are parsed per feed (invariant 7); beyond this the result is
/// truncated.
const MAX_EVENTS: usize = 20_000;
/// Title limit (invariant 7).
const MAX_TITLE_CHARS: usize = 200;
/// Location limit (invariant 7).
const MAX_LOCATION_CHARS: usize = 300;
/// Notes limit (invariant 7).
const MAX_NOTES_CHARS: usize = 4_000;

/// Parsed events plus plain-words warnings and the truncation flag.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCalendar {
    pub events: Vec<Event>,
    pub warnings: Vec<String>,
    pub truncated: bool,
}

/// Parse ICS text. Deterministic: `now` only fills a missing DTSTAMP, and `owner_tz`
/// resolves floating times and unknown TZIDs.
pub fn parse(text: &str, source_id: &str, owner_tz: Tz, now: DateTime<Utc>) -> ParsedCalendar {
    let mut warnings = Vec::new();
    let unfolded = unfold(text);
    if !unfolded.iter().any(|line| line == "BEGIN:VCALENDAR") {
        warnings.push("the file does not contain VCALENDAR".into());
    }
    let mut events = Vec::new();
    let mut current: Option<Vec<String>> = None;
    let mut truncated = false;
    for line in unfolded {
        match line.as_str() {
            "BEGIN:VEVENT" => current = Some(Vec::new()),
            "END:VEVENT" => {
                if let Some(lines) = current.take() {
                    match parse_event(&lines, source_id, owner_tz, now, &mut warnings) {
                        Some(event) => {
                            if events.len() >= MAX_EVENTS {
                                truncated = true;
                                break;
                            }
                            events.push(event);
                        }
                        None => warnings.push("an event was skipped".into()),
                    }
                }
            }
            _ => {
                if let Some(lines) = current.as_mut() {
                    lines.push(line);
                }
            }
        }
    }
    if current.is_some() {
        warnings.push("the file ended inside an event".into());
    }
    ParsedCalendar {
        events,
        warnings,
        truncated,
    }
}

/// Unfold RFC 5545 lines and normalize CRLF/LF. Lines starting with space or tab continue
/// the previous line.
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix([' ', '\t'])
            && let Some(last) = lines.last_mut()
        {
            last.push_str(rest);
            continue;
        }
        lines.push(line.to_string());
    }
    lines
}

struct Property<'a> {
    name: &'a str,
    params: Vec<(&'a str, &'a str)>,
    value: &'a str,
}

fn property(line: &str) -> Option<Property<'_>> {
    let colon = line.find(':')?;
    let head = &line[..colon];
    let value = &line[colon + 1..];
    let mut parts = head.split(';');
    let name = parts.next()?;
    if name.is_empty() {
        return None;
    }
    let params = parts
        .filter_map(|part| {
            let (key, val) = part.split_once('=')?;
            Some((key, val.trim_matches('"')))
        })
        .collect();
    Some(Property {
        name,
        params,
        value,
    })
}

fn param<'a>(prop: &'a Property<'a>, name: &str) -> Option<&'a str> {
    prop.params
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| *value)
}

fn parse_event(
    lines: &[String],
    source_id: &str,
    owner_tz: Tz,
    now: DateTime<Utc>,
    warnings: &mut Vec<String>,
) -> Option<Event> {
    let mut uid = None;
    let mut summary = String::new();
    let mut description = None;
    let mut location = None;
    let mut status = EventStatus::Confirmed;
    let mut rrule = None;
    let mut exdates = Vec::new();
    let mut recurrence_id = None;
    let mut dtstart = None;
    let mut dtend = None;
    let mut dtstamp = None;
    let mut tz_name = None;

    for line in lines {
        let Some(prop) = property(line) else {
            continue;
        };
        match prop.name.to_ascii_uppercase().as_str() {
            "UID" => uid = Some(prop.value.trim().to_string()),
            "SUMMARY" => summary = prop.value.to_string(),
            "DESCRIPTION" => description = Some(prop.value.to_string()),
            "LOCATION" => location = Some(prop.value.to_string()),
            "STATUS" => {
                status = match prop.value.trim().to_ascii_uppercase().as_str() {
                    "TENTATIVE" => EventStatus::Tentative,
                    "CANCELLED" => EventStatus::Cancelled,
                    _ => EventStatus::Confirmed,
                }
            }
            "RRULE" => rrule = Some(prop.value.trim().to_string()),
            "RECURRENCE-ID" => recurrence_id = Some(prop.value.trim().to_string()),
            "DTSTART" => {
                let (value, tz) = parse_date(prop.value, &prop, owner_tz, warnings);
                tz_name = tz;
                dtstart = value;
            }
            "DTEND" => dtend = parse_date(prop.value, &prop, owner_tz, warnings).0,
            "DTSTAMP" | "LAST-MODIFIED" => dtstamp = parse_utc(prop.value),
            "EXDATE" => {
                for raw in prop.value.split(',') {
                    if let Some(value) = normalize_exdate(raw, &prop, owner_tz) {
                        exdates.push(value);
                    }
                }
            }
            _ => {}
        }
    }

    let uid = uid.filter(|value| !value.trim().is_empty());
    let Some(uid) = uid else {
        warnings.push("an event without UID was skipped".into());
        return None;
    };
    let Some(time) = dtstart else {
        warnings.push(format!("{uid}: no DTSTART; the event was skipped"));
        return None;
    };
    let time = finish_time(time, dtend, &tz_name);
    if let EventTime::Timed { start, end, .. } = &time
        && end < start
    {
        warnings.push(format!(
            "{uid}: DTEND is before DTSTART; the event was skipped"
        ));
        return None;
    }
    if let EventTime::AllDay {
        start_date,
        end_date,
    } = &time
        && end_date <= start_date
    {
        warnings.push(format!(
            "{uid}: all-day end is not after start; the event was skipped"
        ));
        return None;
    }
    let repeat = rrule
        .as_deref()
        .and_then(preset_from_rrule)
        .unwrap_or(RepeatPreset::None);
    let id = feed_event_id(source_id, &uid, recurrence_id.as_deref());
    let updated_at = dtstamp.unwrap_or(now);
    Some(Event {
        id,
        source_id: source_id.to_string(),
        uid,
        recurrence_id,
        title: trim_chars(&unescape(&summary), MAX_TITLE_CHARS),
        time,
        location: location.map(|value| trim_chars(&unescape(&value), MAX_LOCATION_CHARS)),
        notes: description.map(|value| trim_chars(&unescape(&value), MAX_NOTES_CHARS)),
        status,
        rrule,
        exdates,
        repeat,
        repeat_until: None,
        prepare: false,
        writable: false,
        updated_at,
    })
}

enum ParsedDate {
    Date(NaiveDate),
    DateTime(DateTime<Utc>, String),
}

fn parse_date(
    value: &str,
    prop: &Property<'_>,
    owner_tz: Tz,
    warnings: &mut Vec<String>,
) -> (Option<ParsedDate>, Option<String>) {
    let value = value.trim();
    if param(prop, "VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE"))
        || (value.len() == 8 && !value.contains('T'))
    {
        return (
            NaiveDate::parse_from_str(value, "%Y%m%d")
                .ok()
                .map(ParsedDate::Date),
            None,
        );
    }
    let Some(naive) = parse_naive(value) else {
        warnings.push("an event had an unreadable date".into());
        return (None, None);
    };
    if let Some(rest) = value.strip_suffix('Z') {
        let _ = rest;
        return (
            Some(ParsedDate::DateTime(
                Utc.from_utc_datetime(&naive),
                "UTC".into(),
            )),
            None,
        );
    }
    let (tz, tz_label, unknown) = match param(prop, "TZID") {
        Some(raw) => match tzid(raw) {
            Some(tz) => (tz, Some(raw.to_string()), false),
            None => {
                warnings.push(format!("unknown TZID `{raw}`; using the owner zone"));
                (owner_tz, Some(owner_tz.name().to_string()), true)
            }
        },
        None => (owner_tz, None, false),
    };
    let _ = unknown;
    let datetime = local_to_utc(tz, naive, warnings);
    (
        Some(ParsedDate::DateTime(datetime, tz.name().to_string())),
        tz_label,
    )
}

fn parse_utc(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    let naive = parse_naive(value)?;
    Some(Utc.from_utc_datetime(&naive))
}

fn parse_naive(value: &str) -> Option<NaiveDateTime> {
    let value = value.trim().trim_end_matches('Z');
    NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M"))
        .ok()
}

fn local_to_utc(tz: Tz, naive: NaiveDateTime, warnings: &mut Vec<String>) -> DateTime<Utc> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(datetime) => datetime.with_timezone(&Utc),
        LocalResult::Ambiguous(first, _) => {
            warnings
                .push("an event fell in an ambiguous local hour; the earlier one was used".into());
            first.with_timezone(&Utc)
        }
        LocalResult::None => {
            warnings.push("an event fell in a skipped local hour; UTC was used".into());
            Utc.from_utc_datetime(&naive)
        }
    }
}

fn finish_time(start: ParsedDate, end: Option<ParsedDate>, tz_name: &Option<String>) -> EventTime {
    match (start, end) {
        (ParsedDate::Date(start_date), end) => {
            let end_date = match end {
                Some(ParsedDate::Date(date)) => date,
                _ => start_date + chrono::Duration::days(1),
            };
            EventTime::AllDay {
                start_date,
                end_date,
            }
        }
        (ParsedDate::DateTime(start, tz), end) => {
            let end = match end {
                Some(ParsedDate::DateTime(end, _)) => end,
                Some(ParsedDate::Date(date)) => {
                    let naive = date.and_hms_opt(0, 0, 0).unwrap_or_default();
                    Utc.from_utc_datetime(&naive)
                }
                None => start + chrono::Duration::hours(1),
            };
            EventTime::Timed {
                start,
                end,
                tz: tz_name.clone().unwrap_or(tz),
            }
        }
    }
}

fn normalize_exdate(raw: &str, prop: &Property<'_>, owner_tz: Tz) -> Option<String> {
    let mut warnings = Vec::new();
    match parse_date(raw, prop, owner_tz, &mut warnings).0? {
        ParsedDate::Date(date) => Some(date.format("%Y-%m-%d").to_string()),
        ParsedDate::DateTime(datetime, _) => Some(datetime.to_rfc3339()),
    }
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn trim_chars(value: &str, max: usize) -> String {
    value.trim().chars().take(max).collect()
}

fn preset_from_rrule(rule: &str) -> Option<RepeatPreset> {
    let upper = rule.to_ascii_uppercase();
    if upper.contains("FREQ=DAILY") && upper.contains("BYDAY=MO,TU,WE,TH,FR") {
        Some(RepeatPreset::Weekdays)
    } else if upper.contains("FREQ=DAILY") {
        Some(RepeatPreset::Daily)
    } else if upper.contains("FREQ=WEEKLY") {
        Some(RepeatPreset::Weekly)
    } else if upper.contains("FREQ=MONTHLY") {
        Some(RepeatPreset::Monthly)
    } else if upper.contains("FREQ=YEARLY") {
        Some(RepeatPreset::Yearly)
    } else {
        None
    }
}

fn tzid(raw: &str) -> Option<Tz> {
    let raw = raw.trim().trim_matches('"');
    if let Ok(tz) = raw.parse::<Tz>() {
        return Some(tz);
    }
    windows_zone(raw)
}

/// Common Outlook/Windows zone names. Fixed a priori from the Windows zone list.
fn windows_zone(name: &str) -> Option<Tz> {
    let iana = match name {
        "UTC" | "Coordinated Universal Time" => "UTC",
        "GMT Standard Time" => "Europe/London",
        "W. Europe Standard Time" => "Europe/Berlin",
        "Romance Standard Time" => "Europe/Paris",
        "Central Europe Standard Time" => "Europe/Budapest",
        "Central European Standard Time" => "Europe/Warsaw",
        "E. Europe Standard Time" => "Europe/Chisinau",
        "FLE Standard Time" => "Europe/Kyiv",
        "Russian Standard Time" => "Europe/Moscow",
        "Turkey Standard Time" => "Europe/Istanbul",
        "Israel Standard Time" => "Asia/Jerusalem",
        "Arabian Standard Time" => "Asia/Dubai",
        "India Standard Time" => "Asia/Kolkata",
        "China Standard Time" => "Asia/Shanghai",
        "Tokyo Standard Time" => "Asia/Tokyo",
        "Korea Standard Time" => "Asia/Seoul",
        "AUS Eastern Standard Time" => "Australia/Sydney",
        "New Zealand Standard Time" => "Pacific/Auckland",
        "Eastern Standard Time" => "America/New_York",
        "Central Standard Time" => "America/Chicago",
        "Mountain Standard Time" => "America/Denver",
        "Pacific Standard Time" => "America/Los_Angeles",
        "Atlantic Standard Time" => "America/Halifax",
        "SA Pacific Standard Time" => "America/Bogota",
        "E. South America Standard Time" => "America/Sao_Paulo",
        _ => return None,
    };
    iana.parse().ok()
}

/// Feed event id: `fe_` + first 16 hex of sha256(source + "\n" + uid + "\n" + recurrence).
pub(crate) fn feed_event_id(source_id: &str, uid: &str, recurrence_id: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(uid.as_bytes());
    hasher.update(b"\n");
    hasher.update(recurrence_id.unwrap_or("").as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("fe_{hex}")
}
