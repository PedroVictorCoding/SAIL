//! K2: expand recurrence rules, EXDATEs and RECURRENCE-ID overrides into occurrences.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use rrule::{RRule, RRuleSet};

use crate::contracts::{Event, EventStatus, EventTime, Occurrence, RepeatPreset};

/// At most 5 000 occurrences are returned per response (invariant 7).
const MAX_OCCURRENCES: usize = 5_000;
/// At most 5 000 instances are generated for one series (invariant 7).
const MAX_SERIES: u16 = 5_000;

/// Expand events into occurrences overlapping `[start, end)`. Pure.
pub fn expand(
    events: &[Event],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    owner_tz: Tz,
    include_cancelled: bool,
) -> (Vec<Occurrence>, bool) {
    if end <= start {
        return (Vec::new(), false);
    }
    let mut bases = Vec::new();
    let mut overrides: HashMap<(String, String), Event> = HashMap::new();
    for event in events {
        match &event.recurrence_id {
            Some(original) => {
                if let Some(key) = recurrence_key(event, original, owner_tz) {
                    overrides.insert((event.uid.clone(), key), event.clone());
                }
            }
            None => bases.push(event),
        }
    }

    let mut occurrences = Vec::new();
    let mut truncated = false;
    for base in bases {
        if base.status == EventStatus::Cancelled && !include_cancelled {
            continue;
        }
        let starts = if let Some(rule) = &base.rrule {
            match rule_starts(base, rule, start, end, owner_tz) {
                Some((starts, limited)) => {
                    truncated |= limited;
                    starts
                }
                None => continue,
            }
        } else {
            vec![instance_start(base, owner_tz)]
        };

        let mut series_count = 0usize;
        for original_start in starts {
            series_count += 1;
            if series_count > MAX_SERIES as usize {
                truncated = true;
                break;
            }
            let key_part = match &base.time {
                EventTime::AllDay { .. } => original_start
                    .with_timezone(&owner_tz)
                    .date_naive()
                    .format("%Y-%m-%d")
                    .to_string(),
                EventTime::Timed { .. } => start_key(&original_start, owner_tz),
            };
            let override_event = overrides
                .get(&(base.uid.clone(), key_part.clone()))
                .cloned();
            let (template, instance_time, status) = match override_event {
                Some(override_event) => {
                    if override_event.status == EventStatus::Cancelled && !include_cancelled {
                        continue;
                    }
                    (
                        override_event.clone(),
                        override_event.time.clone(),
                        override_event.status,
                    )
                }
                None => (
                    base.clone(),
                    shift_time(&base.time, &original_start, owner_tz),
                    base.status,
                ),
            };
            if !overlaps(&instance_time, start, end, owner_tz) {
                continue;
            }
            if status == EventStatus::Cancelled && !include_cancelled {
                continue;
            }
            occurrences.push(Occurrence {
                key: format!("{}@{}", base.id, key_part),
                event_id: base.id.clone(),
                source_id: base.source_id.clone(),
                title: template.title,
                time: instance_time,
                location: template.location,
                status,
                recurring: base.rrule.is_some(),
                prepare: base.prepare || template.prepare,
                writable: base.writable,
            });
            if occurrences.len() > MAX_OCCURRENCES {
                truncated = true;
                break;
            }
        }
        if occurrences.len() > MAX_OCCURRENCES {
            break;
        }
    }
    occurrences.sort_by(|a, b| {
        occurrence_start(&a.time, owner_tz)
            .cmp(&occurrence_start(&b.time, owner_tz))
            .then_with(|| a.key.cmp(&b.key))
    });
    if occurrences.len() > MAX_OCCURRENCES {
        occurrences.truncate(MAX_OCCURRENCES);
        truncated = true;
    }
    (occurrences, truncated)
}

/// The RRULE for a UI preset. Pure.
pub fn preset_rrule(
    repeat: RepeatPreset,
    until: Option<NaiveDate>,
    first: &EventTime,
) -> Option<String> {
    let mut rule = match repeat {
        RepeatPreset::None => return None,
        RepeatPreset::Daily => "FREQ=DAILY".to_string(),
        RepeatPreset::Weekdays => "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".to_string(),
        RepeatPreset::Weekly => {
            let weekday = first_weekday(first);
            format!("FREQ=WEEKLY;BYDAY={weekday}")
        }
        RepeatPreset::Monthly => "FREQ=MONTHLY".to_string(),
        RepeatPreset::Yearly => "FREQ=YEARLY".to_string(),
    };
    if let Some(until) = until {
        rule.push_str(&format!(";UNTIL={}", until.format("%Y%m%d")));
    }
    Some(rule)
}

fn first_weekday(time: &EventTime) -> &'static str {
    let weekday = match time {
        EventTime::Timed { start, .. } => start.weekday(),
        EventTime::AllDay { start_date, .. } => start_date.weekday(),
    };
    match weekday {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

/// Generate original starts for one recurring base. `(local start, out-of-range flag)`.
fn rule_starts(
    base: &Event,
    rule: &str,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    owner_tz: Tz,
) -> Option<(Vec<DateTime<Utc>>, bool)> {
    let duration = event_duration(base);
    let lower = window_start - duration;
    let dt_start = match &base.time {
        EventTime::Timed { start, tz, .. } => {
            let zone = tz.parse::<Tz>().unwrap_or(owner_tz);
            start.with_timezone(&rrule_zone(zone))
        }
        EventTime::AllDay { start_date, .. } => owner_tz
            .from_local_datetime(&start_date.and_hms_opt(0, 0, 0)?)
            .earliest()?
            .with_timezone(&rrule_zone(owner_tz)),
    };
    if let Some((starts, limited)) = fast_sub_daily(rule, dt_start, lower, window_end, owner_tz) {
        return Some((starts, limited));
    }
    let rrule = rule.parse::<RRule<_>>().ok()?.validate(dt_start).ok()?;
    let mut set = RRuleSet::new(dt_start).rrule(rrule);
    for exdate in &base.exdates {
        if let Some(value) = exdate_value(exdate, base, owner_tz) {
            set = set.exdate(value.with_timezone(&rrule_zone(owner_tz)));
        }
    }
    let result = set
        .after(lower.with_timezone(&rrule_zone(owner_tz)))
        .before(window_end.with_timezone(&rrule_zone(owner_tz)))
        .all(MAX_SERIES);
    let starts = result
        .dates
        .into_iter()
        .map(|date| date.with_timezone(&Utc))
        .collect();
    Some((starts, result.limited))
}

/// FREQ=SECONDLY/MINUTELY/HOURLY without BY*/COUNT/UNTIL can be stepped directly from the
/// window, avoiding an eager walk from a years-old DTSTART. Bounded by MAX_SERIES.
fn fast_sub_daily(
    rule: &str,
    dt_start: DateTime<rrule::Tz>,
    lower: DateTime<Utc>,
    window_end: DateTime<Utc>,
    owner_tz: Tz,
) -> Option<(Vec<DateTime<Utc>>, bool)> {
    let upper = rule.to_ascii_uppercase();
    if upper.contains("BY") || upper.contains("COUNT=") || upper.contains("UNTIL=") {
        return None;
    }
    let step = if upper.contains("FREQ=SECONDLY") {
        1i64
    } else if upper.contains("FREQ=MINUTELY") {
        60
    } else if upper.contains("FREQ=HOURLY") {
        3_600
    } else {
        return None;
    };
    let multiplier = rule
        .split(';')
        .find_map(|part| part.strip_prefix("INTERVAL="))
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(1)
        .max(1);
    let step = step.saturating_mul(multiplier);
    let start_utc = dt_start.with_timezone(&Utc);
    let delta = (lower - start_utc).num_seconds();
    let first = if delta <= 0 {
        start_utc
    } else {
        let steps = (delta + step - 1) / step;
        start_utc + Duration::seconds(steps.saturating_mul(step))
    };
    let mut starts = Vec::new();
    let mut current = first;
    let mut limited = false;
    while current < window_end {
        if starts.len() >= MAX_SERIES as usize {
            limited = true;
            break;
        }
        starts.push(current);
        current += Duration::seconds(step);
    }
    let _ = owner_tz;
    Some((starts, limited))
}

fn event_duration(event: &Event) -> Duration {
    match &event.time {
        EventTime::Timed { start, end, .. } => {
            let duration = *end - *start;
            if duration > Duration::zero() {
                duration
            } else {
                Duration::hours(1)
            }
        }
        EventTime::AllDay {
            start_date,
            end_date,
        } => Duration::days((*end_date - *start_date).num_days().max(1)),
    }
}

fn instance_start(event: &Event, owner_tz: Tz) -> DateTime<Utc> {
    match &event.time {
        EventTime::Timed { start, .. } => *start,
        EventTime::AllDay { start_date, .. } => owner_tz
            .from_local_datetime(&start_date.and_hms_opt(0, 0, 0).unwrap_or_default())
            .earliest()
            .map(|date| date.with_timezone(&Utc))
            .unwrap_or_else(|| {
                Utc.from_utc_datetime(&start_date.and_hms_opt(0, 0, 0).unwrap_or_default())
            }),
    }
}

fn shift_time(time: &EventTime, original_start: &DateTime<Utc>, owner_tz: Tz) -> EventTime {
    match time {
        EventTime::Timed { start, end, tz } => {
            let delta = *original_start - *start;
            EventTime::Timed {
                start: *original_start,
                end: *end + delta,
                tz: tz.clone(),
            }
        }
        EventTime::AllDay {
            start_date,
            end_date,
        } => {
            let _ = owner_tz;
            if original_start.date_naive() == *start_date {
                time.clone()
            } else {
                let days = (*end_date - *start_date).num_days();
                EventTime::AllDay {
                    start_date: original_start.date_naive(),
                    end_date: original_start.date_naive() + Duration::days(days),
                }
            }
        }
    }
}

fn start_key(start: &DateTime<Utc>, owner_tz: Tz) -> String {
    let _ = owner_tz;
    start.to_rfc3339()
}

fn recurrence_key(event: &Event, original: &str, owner_tz: Tz) -> Option<String> {
    let _ = (event, owner_tz);
    if let Ok(datetime) = DateTime::parse_from_rfc3339(original) {
        return Some(datetime.with_timezone(&Utc).to_rfc3339());
    }
    if let Ok(date) = NaiveDate::parse_from_str(original, "%Y-%m-%d") {
        return Some(date.format("%Y-%m-%d").to_string());
    }
    let value = original.trim().trim_end_matches('Z');
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S") {
        let utc = if original.trim().ends_with('Z') {
            Utc.from_utc_datetime(&naive)
        } else {
            owner_tz
                .from_local_datetime(&naive)
                .earliest()
                .map(|date| date.with_timezone(&Utc))
                .unwrap_or_else(|| Utc.from_utc_datetime(&naive))
        };
        return Some(utc.to_rfc3339());
    }
    NaiveDate::parse_from_str(value, "%Y%m%d")
        .ok()
        .map(|date| date.format("%Y-%m-%d").to_string())
}

fn exdate_value(value: &str, event: &Event, owner_tz: Tz) -> Option<DateTime<Utc>> {
    if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
        return Some(datetime.with_timezone(&Utc));
    }
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return owner_tz
            .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
            .earliest()
            .map(|value| value.with_timezone(&Utc));
    }
    let _ = event;
    None
}

fn rrule_zone(tz: Tz) -> rrule::Tz {
    tz.into()
}

fn occurrence_start(time: &EventTime, owner_tz: Tz) -> DateTime<Utc> {
    match time {
        EventTime::Timed { start, .. } => *start,
        EventTime::AllDay { start_date, .. } => owner_tz
            .from_local_datetime(&start_date.and_hms_opt(0, 0, 0).unwrap_or_default())
            .earliest()
            .map(|date| date.with_timezone(&Utc))
            .unwrap_or_else(|| {
                Utc.from_utc_datetime(&start_date.and_hms_opt(0, 0, 0).unwrap_or_default())
            }),
    }
}

fn end_datetime(time: &EventTime, owner_tz: Tz) -> DateTime<Utc> {
    match time {
        EventTime::Timed { end, .. } => *end,
        EventTime::AllDay { end_date, .. } => owner_tz
            .from_local_datetime(&end_date.and_hms_opt(0, 0, 0).unwrap_or_default())
            .earliest()
            .map(|date| date.with_timezone(&Utc))
            .unwrap_or_else(|| {
                Utc.from_utc_datetime(&end_date.and_hms_opt(0, 0, 0).unwrap_or_default())
            }),
    }
}

fn overlaps(
    time: &EventTime,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    owner_tz: Tz,
) -> bool {
    occurrence_start(time, owner_tz) < window_end && end_datetime(time, owner_tz) > window_start
}
