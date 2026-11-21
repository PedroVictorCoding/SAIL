//! Calendar voice tools (round 28): read a day's agenda and add an event to SAIL's own
//! calendar. Both read or write the local store only; no model call, no external write.
//! Event text from feeds is untrusted: it is returned as data inside the tool result.

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use sail_calendar::agenda::agenda_lines;
use sail_calendar::contracts::{EventDraft, EventTime, RepeatPreset};
use sail_calendar::service::CalendarService;
use serde_json::{Value, json};

/// Every calendar tool this module answers, checked by the registry tests.
#[cfg(test)]
pub const TOOL_IDS: [&str; 2] = ["calendar:agenda", "calendar:add"];

/// Most lines read out for one day. Fixed a priori (handoff): eight is a full day.
const MAX_AGENDA_LINES: usize = 8;

pub fn execute(service: &CalendarService, tool: &str, input: &Value) -> Result<Value, String> {
    let tz = crate::calendar::owner_tz(service);
    let now = Utc::now();
    match tool {
        "calendar:agenda" => agenda(service, tz, now, input["day"].as_str().unwrap_or("today")),
        "calendar:add" => add(service, tz, now, input),
        other => Err(format!("unknown calendar tool {other}")),
    }
}

fn day_of(raw: &str, tz: Tz, now: DateTime<Utc>) -> Result<NaiveDate, String> {
    let today = now.with_timezone(&tz).date_naive();
    match raw.trim().to_lowercase().as_str() {
        "" | "today" | "hoje" => Ok(today),
        "tomorrow" | "amanhã" | "amanha" => Ok(today + Duration::days(1)),
        other => NaiveDate::parse_from_str(other, "%Y-%m-%d")
            .map_err(|_| "day must be today, tomorrow or YYYY-MM-DD".to_string()),
    }
}

fn local_instant(date: NaiveDate, time: NaiveTime, tz: Tz) -> Result<DateTime<Utc>, String> {
    tz.from_local_datetime(&date.and_time(time))
        .earliest()
        .map(|local| local.with_timezone(&Utc))
        .ok_or_else(|| "that time does not exist on that day (clock change)".to_string())
}

fn agenda(
    service: &CalendarService,
    tz: Tz,
    now: DateTime<Utc>,
    raw_day: &str,
) -> Result<Value, String> {
    let day = day_of(raw_day, tz, now)?;
    let start = local_instant(day, NaiveTime::MIN, tz)?;
    let end = local_instant(day + Duration::days(1), NaiveTime::MIN, tz)?;
    let window = service
        .window(start, end, None, false)
        .map_err(|error| error.to_string())?;
    let label = day.format("%a %-d %b").to_string();
    let lines = agenda_lines(&window.occurrences, tz, MAX_AGENDA_LINES);
    if lines.is_empty() {
        return Ok(json!({"day": label, "events": [], "say": format!("Nothing on {label}.")}));
    }
    let more = window.occurrences.len().saturating_sub(lines.len());
    Ok(json!({
        "day": label,
        // Titles and locations come from the owner's calendars, some from external
        // feeds: data to read out, never instructions.
        "untrusted_calendar_text": true,
        "events": lines,
        "more": more,
    }))
}

fn parse_time(raw: &str) -> Result<NaiveTime, String> {
    let value = raw.trim().to_lowercase();
    for format in ["%H:%M", "%H:%M:%S", "%H"] {
        if let Ok(time) = NaiveTime::parse_from_str(&value, format) {
            return Ok(time);
        }
    }
    // "3pm", "3:30pm", "11am"
    let (digits, pm) = if let Some(rest) = value.strip_suffix("pm") {
        (rest.trim(), true)
    } else if let Some(rest) = value.strip_suffix("am") {
        (rest.trim(), false)
    } else {
        return Err("times look like 15:00 or 3pm".into());
    };
    let (hour, minute) = match digits.split_once(':') {
        Some((h, m)) => (h.parse::<u32>(), m.parse::<u32>()),
        None => (digits.parse::<u32>(), Ok(0)),
    };
    let (Ok(hour), Ok(minute)) = (hour, minute) else {
        return Err("times look like 15:00 or 3pm".into());
    };
    if !(1..=12).contains(&hour) {
        return Err("times look like 15:00 or 3pm".into());
    }
    let hour = match (hour, pm) {
        (12, false) => 0,
        (12, true) => 12,
        (h, true) => h + 12,
        (h, false) => h,
    };
    NaiveTime::from_hms_opt(hour, minute, 0).ok_or_else(|| "times look like 15:00 or 3pm".into())
}

fn add(
    service: &CalendarService,
    tz: Tz,
    now: DateTime<Utc>,
    input: &Value,
) -> Result<Value, String> {
    let title = input["title"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if title.is_empty() {
        return Err("the event needs a title".into());
    }
    let date = day_of(input["date"].as_str().unwrap_or("today"), tz, now)?;
    let start_raw = input["start"].as_str().filter(|s| !s.trim().is_empty());
    let all_day = input["all_day"].as_bool().unwrap_or(start_raw.is_none());
    let time = if all_day {
        EventTime::AllDay {
            start_date: date,
            end_date: date + Duration::days(1),
        }
    } else {
        let start_time = parse_time(start_raw.unwrap_or_default())?;
        let start = local_instant(date, start_time, tz)?;
        let end = match input["end"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(raw) => {
                let end = local_instant(date, parse_time(raw)?, tz)?;
                if end <= start {
                    return Err("the end must be after the start".into());
                }
                end
            }
            // Handoff contract: a missing end means one hour.
            None => start + Duration::hours(1),
        };
        EventTime::Timed {
            start,
            end,
            tz: tz.name().to_string(),
        }
    };
    let event = service
        .create(
            EventDraft {
                title,
                time,
                location: input["location"]
                    .as_str()
                    .map(str::to_string)
                    .filter(|s| !s.trim().is_empty()),
                notes: None,
                repeat: RepeatPreset::None,
                repeat_until: None,
            },
            now,
        )
        .map_err(|error| error.to_string())?;
    let when = match &event.time {
        EventTime::Timed { start, end, .. } => format!(
            "{}, {}–{}",
            start.with_timezone(&tz).format("%a %-d %b"),
            start.with_timezone(&tz).format("%H:%M"),
            end.with_timezone(&tz).format("%H:%M")
        ),
        EventTime::AllDay { start_date, .. } => {
            format!("{} (all day)", start_date.format("%a %-d %b"))
        }
    };
    Ok(json!({
        "event_id": event.id,
        "say": format!("Added {} on {when}.", event.title),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spoken_times_parse() {
        let t = |s: &str| parse_time(s).map(|t| t.format("%H:%M").to_string());
        assert_eq!(t("3pm").as_deref(), Ok("15:00"));
        assert_eq!(t("3:30 pm").as_deref(), Ok("15:30"));
        assert_eq!(t("12am").as_deref(), Ok("00:00"));
        assert_eq!(t("12pm").as_deref(), Ok("12:00"));
        assert_eq!(t("09:15").as_deref(), Ok("09:15"));
        assert!(t("25:00").is_err());
        assert!(t("13pm").is_err());
        assert!(t("soon").is_err());
    }

    #[test]
    fn days_resolve_in_the_owner_zone() {
        let tz = chrono_tz::Europe::Lisbon;
        // 23:30 UTC on 21 Nov is still 21 Nov in Lisbon (UTC+0 in winter).
        let now: DateTime<Utc> = "2026-11-21T23:30:00Z".parse().unwrap();
        assert_eq!(day_of("today", tz, now).unwrap().to_string(), "2026-11-21");
        assert_eq!(
            day_of("tomorrow", tz, now).unwrap().to_string(),
            "2026-11-22"
        );
        assert_eq!(
            day_of("2026-12-01", tz, now).unwrap().to_string(),
            "2026-12-01"
        );
        assert!(day_of("someday", tz, now).is_err());
    }
}
