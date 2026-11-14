//! The schedule grammar: `in 30m`, `every 2h`, `weekdays at 9am` and five-field cron.
//!
//! Everything here is pure. Local wall-clock times are resolved in the schedule's IANA
//! zone: a nonexistent time (spring forward) runs at the first valid instant after it,
//! and an ambiguous time (fall back) runs once at the earlier instant.
use chrono::{Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// A parsed schedule, with the IANA zone it is evaluated in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    pub when: When,
    /// IANA zone, e.g. `Europe/Lisbon`.
    pub tz: String,
}

/// The four schedule shapes SAIL understands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum When {
    /// One run at a unix timestamp.
    Once { at: i64 },
    /// Slots are `anchor + k * secs`, `k >= 1`; at least five minutes apart.
    Every { secs: u64, anchor: i64 },
    /// Local times in `tz`, sorted and deduplicated, at most 12.
    Weekly {
        days: Weekdays,
        times: Vec<(u8, u8)>,
    },
    /// Normalized five-field cron text, evaluated in `tz`.
    Cron { expr: String },
}

/// A bit set of weekdays, Monday is bit 0, Sunday bit 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Weekdays(
    /// Bit set: Monday is bit 0, Sunday bit 6.
    pub u8,
);

impl Weekdays {
    /// Monday through Sunday.
    pub const ALL: Weekdays = Weekdays(0b111_1111);
    /// Monday through Friday.
    pub const WORKDAYS: Weekdays = Weekdays(0b001_1111);
    /// Saturday and Sunday.
    pub const WEEKEND: Weekdays = Weekdays(0b110_0000);
}

const MIN_EVERY_SECS: u64 = 5 * 60;
const MAX_TIMES: usize = 12;
const MAX_TEXT_CHARS: usize = 200;
const FIVE_YEARS_DAYS: i64 = 366 * 5 + 1;

/// Parse one schedule. `anchor` is the job's creation time and the base for
/// `in …`, `every …` and `tomorrow`.
pub fn parse(text: &str, tz: &str, anchor: i64) -> Result<Spec, String> {
    let raw = text.trim();
    if raw.chars().count() > MAX_TEXT_CHARS {
        return Err("the schedule is too long; keep it under 200 characters".into());
    }
    if raw.is_empty() {
        return Err(unknown(raw));
    }
    let zone: Tz = tz
        .trim()
        .parse()
        .map_err(|_| format!("unknown time zone \"{tz}\""))?;
    let lower = raw.to_lowercase();

    if let Some(rest) = lower.strip_prefix("@") {
        let expr = match rest.trim() {
            "hourly" => "0 * * * *",
            "daily" => "0 0 * * *",
            "weekly" => "0 0 * * 0",
            "monthly" => "0 0 1 * *",
            _ => return Err(unknown(raw)),
        };
        return cron_spec(expr, tz);
    }

    if let Some(rest) = lower.strip_prefix("in ") {
        let secs = parse_duration(rest.trim()).ok_or_else(|| unknown(raw))?;
        if secs == 0 {
            return Err("choose a time in the future".into());
        }
        let at = i128::from(anchor) + i128::from(secs);
        let at = i64::try_from(at).map_err(|_| "that time is too far away".to_string())?;
        return Ok(Spec {
            when: When::Once { at },
            tz: tz.trim().to_string(),
        });
    }

    if let Some(rest) = lower.strip_prefix("at ") {
        return once_at(rest.trim(), raw, &zone, tz);
    }

    if let Some(rest) = lower.strip_prefix("tomorrow at ") {
        let (hour, minute) = parse_time(rest.trim()).ok_or_else(|| unknown(raw))?;
        let local = zone
            .timestamp_opt(anchor, 0)
            .single()
            .ok_or_else(|| "that date is out of range".to_string())?;
        let date = local.date_naive() + Duration::days(1);
        let naive = date
            .and_hms_opt(u32::from(hour), u32::from(minute), 0)
            .ok_or_else(|| unknown(raw))?;
        let at = resolve_local(zone, naive)
            .ok_or_else(|| "that local time does not exist".to_string())?;
        return Ok(Spec {
            when: When::Once { at },
            tz: tz.trim().to_string(),
        });
    }

    if let Some(rest) = lower.strip_prefix("every ") {
        let rest = rest.trim();
        if rest == "day" {
            return Ok(Spec {
                when: When::Every {
                    secs: 86_400,
                    anchor,
                },
                tz: tz.trim().to_string(),
            });
        }
        if let Some((days, times)) = split_days_times(rest) {
            return weekly_spec(days, times, raw, tz);
        }
        let secs = parse_duration(rest).ok_or_else(|| unknown(raw))?;
        if secs < MIN_EVERY_SECS {
            return Err("the shortest interval is 5 minutes".into());
        }
        return Ok(Spec {
            when: When::Every { secs, anchor },
            tz: tz.trim().to_string(),
        });
    }

    if let Some((days, times)) = split_days_times(&lower) {
        return weekly_spec(days, times, raw, tz);
    }

    if looks_like_datetime(&lower) {
        return once_at(&lower, raw, &zone, tz);
    }

    if lower.split_whitespace().count() == 5 {
        return cron_spec(&lower, tz);
    }

    Err(unknown(raw))
}

/// The first slot strictly after `after`, or `None` for a past `Once` and for a cron
/// with no slot in the next five years.
pub fn next_after(spec: &Spec, after: i64) -> Option<i64> {
    match &spec.when {
        When::Once { at } => (*at > after).then_some(*at),
        When::Every { secs, anchor } => {
            if *secs == 0 {
                return None;
            }
            let span = i128::from(after) - i128::from(*anchor);
            let k = if span < 0 {
                1
            } else {
                span / i128::from(*secs) + 1
            };
            let slot = i128::from(*anchor) + k * i128::from(*secs);
            i64::try_from(slot).ok()
        }
        When::Weekly { days, times } => {
            let zone: Tz = spec.tz.parse().ok()?;
            next_weekly(zone, *days, times, after)
        }
        When::Cron { expr } => {
            let zone: Tz = spec.tz.parse().ok()?;
            let cron = Cron::parse(expr).ok()?;
            next_cron(zone, &cron, after)
        }
    }
}

/// Canonical owner-facing text, always followed by the zone.
pub fn describe(spec: &Spec) -> String {
    let zone: Option<Tz> = spec.tz.parse().ok();
    let body = match &spec.when {
        When::Once { at } => match zone.and_then(|z| z.timestamp_opt(*at, 0).single()) {
            Some(local) => format!("once on {}", local.format("%a %d %b %Y at %H:%M")),
            None => format!("once at {at}"),
        },
        When::Every { secs, .. } => {
            let secs = *secs;
            if secs == 86_400 {
                "every day".to_string()
            } else if secs % 3600 == 0 {
                every_text(secs / 3600, "hour")
            } else if secs % 60 == 0 {
                every_text(secs / 60, "minute")
            } else {
                every_text(secs, "second")
            }
        }
        When::Weekly { days, times } => {
            let day_text = match *days {
                Weekdays::ALL => "every day".to_string(),
                Weekdays::WORKDAYS => "weekdays".to_string(),
                Weekdays::WEEKEND => "weekends".to_string(),
                Weekdays(bits) => {
                    let names = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
                    let list: Vec<&str> = (0..7)
                        .filter(|i| bits & (1 << i) != 0)
                        .map(|i| names[i])
                        .collect();
                    list.join(", ")
                }
            };
            let times_text = times
                .iter()
                .map(|(h, m)| format!("{h:02}:{m:02}"))
                .collect::<Vec<_>>()
                .join(" and ");
            format!("{day_text} at {times_text}")
        }
        When::Cron { expr } => format!("cron {expr}"),
    };
    format!("{body} ({})", spec.tz)
}

fn every_text(n: u64, unit: &str) -> String {
    if n == 1 {
        format!("every 1 {unit}")
    } else {
        format!("every {n} {unit}s")
    }
}

fn unknown(raw: &str) -> String {
    format!("I don't understand \"{raw}\"; try \"every day at 9am\"")
}

fn cron_spec(expr: &str, tz: &str) -> Result<Spec, String> {
    let cron = Cron::parse(expr)?;
    Ok(Spec {
        when: When::Cron {
            expr: cron.normalized,
        },
        tz: tz.trim().to_string(),
    })
}

fn once_at(rest: &str, raw: &str, zone: &Tz, tz: &str) -> Result<Spec, String> {
    let naive = parse_datetime(rest).ok_or_else(|| unknown(raw))?;
    let at =
        resolve_local(*zone, naive).ok_or_else(|| "that local time does not exist".to_string())?;
    Ok(Spec {
        when: When::Once { at },
        tz: tz.trim().to_string(),
    })
}

fn weekly_spec(days: Weekdays, times: Vec<(u8, u8)>, raw: &str, tz: &str) -> Result<Spec, String> {
    if days.0 == 0 {
        return Err(unknown(raw));
    }
    let mut times = times;
    times.sort_unstable();
    times.dedup();
    if times.is_empty() {
        return Err(unknown(raw));
    }
    if times.len() > MAX_TIMES {
        return Err("use at most 12 times a day".into());
    }
    Ok(Spec {
        when: When::Weekly { days, times },
        tz: tz.trim().to_string(),
    })
}

/// Split `"<days> at <time>[ and <time>]"` and parse both halves.
fn split_days_times(text: &str) -> Option<(Weekdays, Vec<(u8, u8)>)> {
    let (days_text, times_text) = text.rsplit_once(" at ")?;
    let days = parse_days(days_text.trim())?;
    let mut times = Vec::new();
    for part in times_text.split(" and ").flat_map(|p| p.split(',')) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        times.push(parse_time(part)?);
    }
    Some((days, times))
}

fn parse_days(text: &str) -> Option<Weekdays> {
    if text.is_empty() {
        return None;
    }
    let text = text.strip_prefix("every ").unwrap_or(text).trim();
    match text {
        "daily" | "day" | "every day" => return Some(Weekdays::ALL),
        "weekdays" | "weekday" | "workdays" => return Some(Weekdays::WORKDAYS),
        "weekends" | "weekend" => return Some(Weekdays::WEEKEND),
        _ => {}
    }
    let mut bits = 0u8;
    for part in text.split(',').flat_map(|p| p.split(" and ")) {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        bits |= 1 << day_index(part)?;
    }
    (bits != 0).then_some(Weekdays(bits))
}

fn day_index(token: &str) -> Option<u8> {
    let token = token.trim().trim_end_matches('s');
    match token {
        "monday" | "mon" => Some(0),
        "tuesday" | "tue" | "tues" => Some(1),
        "wednesday" | "wed" | "weds" => Some(2),
        "thursday" | "thu" | "thur" | "thurs" => Some(3),
        "friday" | "fri" => Some(4),
        "saturday" | "sat" => Some(5),
        "sunday" | "sun" => Some(6),
        _ => None,
    }
}

fn parse_time(text: &str) -> Option<(u8, u8)> {
    let value = text.trim().to_lowercase();
    match value.as_str() {
        "noon" => return Some((12, 0)),
        "midnight" => return Some((0, 0)),
        _ => {}
    }
    let (body, meridiem) = if let Some(body) = value.strip_suffix("am") {
        (body.trim(), Some(false))
    } else if let Some(body) = value.strip_suffix("pm") {
        (body.trim(), Some(true))
    } else {
        (value.as_str(), None)
    };
    let (hour_text, minute_text) = if let Some((h, m)) = body.split_once(':') {
        (h, Some(m))
    } else if let Some((h, m)) = body.split_once('.') {
        (h, Some(m))
    } else {
        (body, None)
    };
    if hour_text.is_empty() || hour_text.len() > 2 || !hour_text.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut hour: u32 = hour_text.parse().ok()?;
    let minute: u32 = match minute_text {
        Some(m) if m.is_empty() || m.len() > 2 || !m.bytes().all(|b| b.is_ascii_digit()) => {
            return None;
        }
        Some(m) => m.parse().ok()?,
        None => 0,
    };
    if minute > 59 {
        return None;
    }
    match meridiem {
        Some(pm) => {
            if !(1..=12).contains(&hour) {
                return None;
            }
            if hour == 12 {
                hour = 0;
            }
            if pm {
                hour += 12;
            }
        }
        None => {
            if hour > 23 {
                return None;
            }
        }
    }
    Some((hour as u8, minute as u8))
}

fn parse_datetime(text: &str) -> Option<NaiveDateTime> {
    let text = text.trim().replace('t', " ");
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(value) = NaiveDateTime::parse_from_str(&text, format) {
            return Some(value);
        }
    }
    None
}

fn looks_like_datetime(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 10
        && bytes[0].is_ascii_digit()
        && bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
}

fn parse_duration(text: &str) -> Option<u64> {
    let text = text.trim();
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let number: u64 = digits.parse().ok()?;
    let unit = text[digits.len()..].trim().to_lowercase();
    let secs = match unit.trim_end_matches('s') {
        "m" | "min" | "minute" => 60,
        "h" | "hr" | "hour" => 3600,
        "d" | "day" => 86_400,
        _ => return None,
    };
    number.checked_mul(secs)
}

fn resolve_local(zone: Tz, naive: NaiveDateTime) -> Option<i64> {
    match zone.from_local_datetime(&naive) {
        LocalResult::Single(value) => Some(value.timestamp()),
        LocalResult::Ambiguous(first, second) => Some(first.timestamp().min(second.timestamp())),
        LocalResult::None => {
            let mut probe = naive;
            for _ in 0..(3 * 60) {
                probe += Duration::minutes(1);
                match zone.from_local_datetime(&probe) {
                    LocalResult::Single(value) => return Some(value.timestamp()),
                    LocalResult::Ambiguous(first, second) => {
                        return Some(first.timestamp().min(second.timestamp()));
                    }
                    LocalResult::None => {}
                }
            }
            None
        }
    }
}

fn next_weekly(zone: Tz, days: Weekdays, times: &[(u8, u8)], after: i64) -> Option<i64> {
    let local = zone.timestamp_opt(after, 0).single()?;
    let start = local.date_naive();
    for offset in 0..=FIVE_YEARS_DAYS {
        let date = start.checked_add_signed(Duration::days(offset))?;
        let weekday = date.weekday().num_days_from_monday();
        if days.0 & (1 << weekday) == 0 {
            continue;
        }
        let mut best: Option<i64> = None;
        for (hour, minute) in times {
            let Some(naive) = date.and_hms_opt(u32::from(*hour), u32::from(*minute), 0) else {
                continue;
            };
            if let Some(slot) = resolve_local(zone, naive)
                && slot > after
                && best.is_none_or(|current| slot < current)
            {
                best = Some(slot);
            }
        }
        if best.is_some() {
            return best;
        }
    }
    None
}

fn next_cron(zone: Tz, cron: &Cron, after: i64) -> Option<i64> {
    let local = zone.timestamp_opt(after, 0).single()?;
    let start = local.date_naive();
    for offset in 0..=FIVE_YEARS_DAYS {
        let date = start.checked_add_signed(Duration::days(offset))?;
        if !cron.day_matches(date) {
            continue;
        }
        let mut best: Option<i64> = None;
        for hour in &cron.hour {
            for minute in &cron.minute {
                let Some(naive) = date.and_hms_opt(*hour, *minute, 0) else {
                    continue;
                };
                if let Some(slot) = resolve_local(zone, naive)
                    && slot > after
                    && best.is_none_or(|current| slot < current)
                {
                    best = Some(slot);
                }
            }
        }
        if best.is_some() {
            return best;
        }
    }
    None
}

#[derive(Debug, Clone)]
struct Cron {
    normalized: String,
    minute: Vec<u32>,
    hour: Vec<u32>,
    dom: Vec<u32>,
    month: Vec<u32>,
    dow: Vec<u32>,
    dom_star: bool,
    dow_star: bool,
}

impl Cron {
    fn parse(expr: &str) -> Result<Self, String> {
        let text = expr.trim().to_lowercase();
        let text = match text.as_str() {
            "@hourly" => "0 * * * *",
            "@daily" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            _ => text.as_str(),
        };
        let fields: Vec<&str> = text.split_whitespace().collect();
        if fields.len() != 5 {
            return Err("cron needs five fields, like \"30 9 * * 1-5\"".into());
        }
        let minute = parse_cron_field(fields[0], 0, 59, &[], false)?;
        let hour = parse_cron_field(fields[1], 0, 23, &[], false)?;
        let dom = parse_cron_field(fields[2], 1, 31, &[], false)?;
        let month = parse_cron_field(fields[3], 1, 12, MONTHS, false)?;
        let dow = parse_cron_field(fields[4], 0, 7, WEEKDAYS, true)?;
        Ok(Self {
            normalized: fields.join(" "),
            minute,
            hour,
            dom,
            month,
            dow,
            dom_star: fields[2] == "*",
            dow_star: fields[4] == "*",
        })
    }

    fn day_matches(&self, date: NaiveDate) -> bool {
        if !self.month.contains(&date.month()) {
            return false;
        }
        let day = date.day();
        if day > days_in_month(date.year(), date.month()) {
            return false;
        }
        let dom_ok = self.dom.contains(&day);
        let dow_ok = self.dow.contains(&date.weekday().num_days_from_sunday());
        if !self.dom_star && !self.dow_star {
            dom_ok || dow_ok
        } else {
            dom_ok && dow_ok
        }
    }
}

const MONTHS: &[(&str, u32)] = &[
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
];

const WEEKDAYS: &[(&str, u32)] = &[
    ("sun", 0),
    ("mon", 1),
    ("tue", 2),
    ("wed", 3),
    ("thu", 4),
    ("fri", 5),
    ("sat", 6),
];

fn parse_cron_field(
    text: &str,
    min: u32,
    max: u32,
    names: &[(&str, u32)],
    sunday_seven: bool,
) -> Result<Vec<u32>, String> {
    let mut values = Vec::new();
    for part in text.split(',') {
        if part.is_empty() {
            return Err(format!("invalid cron field \"{text}\""));
        }
        let (base, step) = match part.split_once('/') {
            Some((base, step)) => {
                let step: u32 = step
                    .parse()
                    .map_err(|_| format!("invalid cron step \"{part}\""))?;
                if step == 0 {
                    return Err("cron steps must be at least 1".into());
                }
                (base, step)
            }
            None => (part, 1),
        };
        let stepped = part.contains('/');
        let (start, end) = if base == "*" {
            (min, max)
        } else if let Some((start, end)) = base.split_once('-') {
            (
                cron_value(start, min, max, names)?,
                cron_value(end, min, max, names)?,
            )
        } else {
            let value = cron_value(base, min, max, names)?;
            (value, if stepped { max } else { value })
        };
        if start > end {
            return Err(format!("invalid cron range \"{part}\""));
        }
        let mut value = start;
        while value <= end {
            values.push(if sunday_seven && value == 7 { 0 } else { value });
            value += step;
        }
    }
    values.sort_unstable();
    values.dedup();
    Ok(values)
}

fn cron_value(text: &str, min: u32, max: u32, names: &[(&str, u32)]) -> Result<u32, String> {
    let value = if let Ok(number) = text.parse::<u32>() {
        number
    } else if let Some((_, value)) = names.iter().find(|(name, _)| *name == text) {
        *value
    } else {
        return Err(format!("invalid cron value \"{text}\""));
    };
    if value < min || value > max {
        return Err(format!("cron value \"{text}\" is out of range"));
    }
    Ok(value)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 31,
    }
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}
