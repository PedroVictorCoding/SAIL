//! Deterministic date and time mention extractor for Portuguese and English academic texts.
//!
//! Follows Portuguese day-first convention (`DD/MM/YYYY`), recognizes Portuguese and English
//! month names, times (`14h30`, `14:30`, `9:00-11:30`), date ranges (`12 a 16 de janeiro`),
//! infers missing years from term boundaries or observation time, and determines item kind hints
//! from surrounding keywords.

use crate::contracts::ItemKind;
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Utc};
use std::ops::Range;

/// Maximum date mentions returned from one document to keep work bounded.
pub const MAX_MENTIONS: usize = 200;

/// A date/time mention discovered in text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateMention {
    /// Byte offsets on UTF-8 character boundaries in the original text.
    pub span: Range<usize>,
    /// The primary date.
    pub date: NaiveDate,
    /// Time of the event, when specified.
    pub time: Option<NaiveTime>,
    /// End date for multi-day ranges.
    pub end_date: Option<NaiveDate>,
    /// End time for time intervals (`9:00-11:30`).
    pub end_time: Option<NaiveTime>,
    /// Likely academic item kind inferred from keywords within ±80 characters.
    pub kind_hint: Option<ItemKind>,
    /// Verbatim substring matched.
    pub raw: String,
}

/// Month specification parsed from text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MonthInfo {
    month: u32,
    len: usize,
}

/// Find all valid dates, time mentions and ranges in `text`.
pub fn find_dates(
    text: &str,
    observed: DateTime<Utc>,
    term: Option<(NaiveDate, NaiveDate)>,
) -> Vec<DateMention> {
    let mut mentions = Vec::new();
    let mut i = 0;

    while i < text.len() && mentions.len() < MAX_MENTIONS {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }

        // Try matching a date mention starting around position i
        if let Some(mention) = try_parse_mention_at(text, i, observed, term) {
            let next_pos = mention.span.end;
            mentions.push(mention);
            i = next_pos.max(i + 1);
        } else {
            i += 1;
        }
    }

    mentions
}

/// Try to parse a date mention at position `start_idx`.
fn try_parse_mention_at(
    text: &str,
    start_idx: usize,
    observed: DateTime<Utc>,
    term: Option<(NaiveDate, NaiveDate)>,
) -> Option<DateMention> {
    let slice = &text[start_idx..];

    // Must start on word boundary (or punctuation start)
    if start_idx > 0 {
        let prev_char = text[..start_idx].chars().next_back().unwrap_or(' ');
        if prev_char.is_alphanumeric() {
            return None;
        }
    }

    // Check optional weekday prefix, e.g. "segunda-feira, ", "Mon "
    let (prefix_len, _has_weekday) = match_weekday_prefix(slice);
    let after_prefix = &slice[prefix_len..];
    let candidate_start = start_idx + prefix_len;

    // Pattern 1: ISO date `YYYY-MM-DD`
    if let Some((date, match_len)) = try_parse_iso_date(after_prefix) {
        let span_end = candidate_start + match_len;
        return build_mention(text, start_idx, span_end, date, None, None, None);
    }

    // Pattern 2: English "Month DD, YYYY" or "Month DD"
    if let Some((date, match_len)) = try_parse_en_month_day(after_prefix, observed, term) {
        let span_end = candidate_start + match_len;
        return build_mention(text, start_idx, span_end, date, None, None, None);
    }

    // Pattern 3: Range "DD a DD de Month [de YYYY]" or "DD-DD de Month" or "DD to DD Month"
    if let Some((start_date, end_date, match_len)) =
        try_parse_day_range(after_prefix, observed, term)
    {
        let span_end = candidate_start + match_len;
        return build_mention(
            text,
            start_idx,
            span_end,
            start_date,
            None,
            Some(end_date),
            None,
        );
    }

    // Pattern 4: Textual month "DD de Month [de YYYY]" or "DD Month [YYYY]"
    if let Some((date, match_len)) = try_parse_day_month_text(after_prefix, observed, term) {
        let span_end = candidate_start + match_len;
        return build_mention(text, start_idx, span_end, date, None, None, None);
    }

    // Pattern 5: Numeric date `DD/MM/YYYY`, `DD-MM-YYYY`, `DD.MM.YY`, `DD/MM`
    if let Some((date, match_len)) = try_parse_numeric_date(after_prefix, observed, term) {
        let span_end = candidate_start + match_len;
        return build_mention(text, start_idx, span_end, date, None, None, None);
    }

    None
}

/// Helper to build a DateMention, scanning attached times and computing kind_hint.
fn build_mention(
    text: &str,
    start_idx: usize,
    date_end_idx: usize,
    date: NaiveDate,
    time: Option<NaiveTime>,
    end_date: Option<NaiveDate>,
    end_time: Option<NaiveTime>,
) -> Option<DateMention> {
    // Check if time information immediately follows or precedes date
    let (final_end, parsed_time, parsed_end_time) =
        check_following_time(&text[date_end_idx..], date_end_idx, time, end_time);

    let raw = text[start_idx..final_end].to_string();
    let span = start_idx..final_end;
    let kind_hint = find_kind_hint(text, &span);

    Some(DateMention {
        span,
        date,
        time: parsed_time,
        end_date,
        end_time: parsed_end_time,
        kind_hint,
        raw,
    })
}

/// Check for following time specification like ", às 14h30", " 14:30", " das 9h às 11h30", " at 2 pm"
fn check_following_time(
    slice: &str,
    current_end: usize,
    existing_time: Option<NaiveTime>,
    existing_end_time: Option<NaiveTime>,
) -> (usize, Option<NaiveTime>, Option<NaiveTime>) {
    if existing_time.is_some() {
        return (current_end, existing_time, existing_end_time);
    }

    let mut advance = 0;
    // Skip optional spaces, comma, prepositions like "às", "as", "at", "pelas", "das"
    let mut rest = slice;

    // Trim leading whitespace or comma
    while let Some(ch) = rest.chars().next() {
        if ch.is_whitespace() || ch == ',' {
            let len = ch.len_utf8();
            rest = &rest[len..];
            advance += len;
        } else {
            break;
        }
    }

    // Check prepositions
    let prep_prefixes = [
        "às ",
        "as ",
        "at ",
        "pelas ",
        "pela ",
        "das ",
        "desde as ",
        "de ",
    ];
    for prep in prep_prefixes {
        if rest.to_lowercase().starts_with(prep) {
            let len = prep.len();
            rest = &rest[len..];
            advance += len;
            break;
        }
    }

    // Now try parse time from `rest`
    if let Some((t_start, t_end, t_len)) = try_parse_time_expr(rest) {
        return (current_end + advance + t_len, Some(t_start), t_end);
    }

    (current_end, None, None)
}

/// Try parsing time expression: "14h30", "14:30", "14h", "9:00-11:30", "9h às 11h30", "2 pm"
fn try_parse_time_expr(slice: &str) -> Option<(NaiveTime, Option<NaiveTime>, usize)> {
    let (t1, len1) = parse_single_time(slice)?;
    let mut total_len = len1;
    let mut end_time = None;

    let rest = &slice[len1..];
    // Check range separators: "-", " - ", " às ", " as ", " to ", " até às ", " ate as "
    let sep_options = [
        " - ",
        "-",
        " às ",
        " as ",
        " to ",
        " ate as ",
        " até às ",
        " até as ",
    ];
    for sep in sep_options {
        if rest.to_lowercase().starts_with(sep) {
            let after_sep = &rest[sep.len()..];
            if let Some((t2, len2)) = parse_single_time(after_sep) {
                end_time = Some(t2);
                total_len = len1 + sep.len() + len2;
                break;
            }
        }
    }

    Some((t1, end_time, total_len))
}

/// Parse a single time string like "14h", "14h30", "14:30", "09:00", "2 pm", "2:30 pm"
fn parse_single_time(slice: &str) -> Option<(NaiveTime, usize)> {
    if slice.is_empty() {
        return None;
    }

    let mut i = 0;
    while i < slice.len() && slice.as_bytes()[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 || i > 2 {
        return None;
    }

    let hour: u32 = slice[..i].parse().ok()?;
    let rest = &slice[i..];

    // Check separator: 'h' / 'H' / ':'
    if let Some(after_h) = rest.strip_prefix('h').or_else(|| rest.strip_prefix('H')) {
        let mut min_len = 0;
        while min_len < after_h.len() && after_h.as_bytes()[min_len].is_ascii_digit() {
            min_len += 1;
        }
        let minute = if min_len == 0 {
            0
        } else if min_len == 2 {
            after_h[..2].parse().ok()?
        } else {
            return None;
        };
        let t = NaiveTime::from_hms_opt(hour, minute, 0)?;
        Some((t, i + 1 + min_len))
    } else if let Some(after_colon) = rest.strip_prefix(':') {
        let mut min_len = 0;
        while min_len < after_colon.len() && after_colon.as_bytes()[min_len].is_ascii_digit() {
            min_len += 1;
        }
        if min_len != 2 {
            return None;
        }
        let minute: u32 = after_colon[..2].parse().ok()?;
        let mut consumed = i + 1 + 2;
        let mut h = hour;

        // Check optional am/pm
        let remainder = &after_colon[2..];
        let trimmed_rem = remainder.trim_start();
        let diff = remainder.len() - trimmed_rem.len();
        if trimmed_rem.to_ascii_lowercase().starts_with("pm") {
            if h < 12 {
                h += 12;
            }
            consumed += diff + 2;
        } else if trimmed_rem.to_ascii_lowercase().starts_with("am") {
            if h == 12 {
                h = 0;
            }
            consumed += diff + 2;
        }

        let t = NaiveTime::from_hms_opt(h, minute, 0)?;
        Some((t, consumed))
    } else {
        // Check "2 pm" or "2 am"
        let trimmed_rest = rest.trim_start();
        let diff = rest.len() - trimmed_rest.len();
        if trimmed_rest.to_ascii_lowercase().starts_with("pm") {
            let mut h = hour;
            if h < 12 {
                h += 12;
            }
            let t = NaiveTime::from_hms_opt(h, 0, 0)?;
            Some((t, i + diff + 2))
        } else if trimmed_rest.to_ascii_lowercase().starts_with("am") {
            let mut h = hour;
            if h == 12 {
                h = 0;
            }
            let t = NaiveTime::from_hms_opt(h, 0, 0)?;
            Some((t, i + diff + 2))
        } else {
            None
        }
    }
}

/// Matches optional weekday prefix.
fn match_weekday_prefix(slice: &str) -> (usize, bool) {
    let weekdays = [
        "segunda-feira",
        "terça-feira",
        "terca-feira",
        "quarta-feira",
        "quinta-feira",
        "sexta-feira",
        "segunda",
        "terça",
        "terca",
        "quarta",
        "quinta",
        "sexta",
        "sábado",
        "sabado",
        "domingo",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
        "mon",
        "tue",
        "wed",
        "thu",
        "fri",
        "sat",
        "sun",
    ];

    let lower = slice.to_lowercase();
    for wd in weekdays {
        if lower.starts_with(wd) {
            let len = wd.len();
            // Optional comma or separator
            let rest = &slice[len..];
            let extra = rest
                .chars()
                .take_while(|c| c.is_whitespace() || *c == ',' || *c == ':')
                .map(|c| c.len_utf8())
                .sum::<usize>();
            return (len + extra, true);
        }
    }

    (0, false)
}

/// Parse ISO date `YYYY-MM-DD`
fn try_parse_iso_date(slice: &str) -> Option<(NaiveDate, usize)> {
    if slice.len() < 10 {
        return None;
    }
    let b = slice.as_bytes();
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    if !b[0..4].iter().all(u8::is_ascii_digit)
        || !b[5..7].iter().all(u8::is_ascii_digit)
        || !b[8..10].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    // Word boundary after DD
    if slice.len() > 10 && (b[10].is_ascii_alphanumeric() || b[10] == b'-') {
        return None;
    }

    let y: i32 = slice[0..4].parse().ok()?;
    let m: u32 = slice[5..7].parse().ok()?;
    let d: u32 = slice[8..10].parse().ok()?;

    let date = NaiveDate::from_ymd_opt(y, m, d)?;
    Some((date, 10))
}

/// Parse English "Month DD, YYYY" or "Month DD" (e.g. "December 12, 2026")
fn try_parse_en_month_day(
    slice: &str,
    observed: DateTime<Utc>,
    term: Option<(NaiveDate, NaiveDate)>,
) -> Option<(NaiveDate, usize)> {
    let month_info = match_month_name(slice)?;
    let mut consumed = month_info.len;
    let rest = &slice[consumed..];

    // Must have whitespace
    let trimmed = rest.trim_start();
    let space_len = rest.len() - trimmed.len();
    if space_len == 0 {
        return None;
    }
    consumed += space_len;

    // Read day DD
    let mut day_len = 0;
    while day_len < trimmed.len() && trimmed.as_bytes()[day_len].is_ascii_digit() {
        day_len += 1;
    }
    if day_len == 0 || day_len > 2 {
        return None;
    }
    let day: u32 = trimmed[..day_len].parse().ok()?;
    consumed += day_len;

    // Optional ordinal suffix "st", "nd", "rd", "th"
    let after_day = &trimmed[day_len..];
    let mut rem = after_day;
    for suffix in ["st", "nd", "rd", "th"] {
        if rem.to_ascii_lowercase().starts_with(suffix) {
            consumed += 2;
            rem = &rem[2..];
            break;
        }
    }

    // Optional year ", YYYY" or " YYYY"
    let mut year = None;
    let trimmed_rem = rem.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
    let skipped = rem.len() - trimmed_rem.len();
    if skipped > 0 && trimmed_rem.len() >= 4 {
        let b = trimmed_rem.as_bytes();
        if b[0..4].iter().all(u8::is_ascii_digit)
            && (trimmed_rem.len() == 4 || !b[4].is_ascii_digit())
            && let Ok(y) = trimmed_rem[..4].parse::<i32>()
        {
            year = Some(y);
            consumed += skipped + 4;
        }
    }

    let final_year = match year {
        Some(y) => y,
        None => infer_year(month_info.month, day, observed, term)?,
    };

    let date = NaiveDate::from_ymd_opt(final_year, month_info.month, day)?;
    Some((date, consumed))
}

/// Parse day range "12 a 16 de janeiro [de 2026]" or "12-16 de janeiro" or "12 to 16 January"
fn try_parse_day_range(
    slice: &str,
    observed: DateTime<Utc>,
    term: Option<(NaiveDate, NaiveDate)>,
) -> Option<(NaiveDate, NaiveDate, usize)> {
    // Read first day
    let mut d1_len = 0;
    while d1_len < slice.len() && slice.as_bytes()[d1_len].is_ascii_digit() {
        d1_len += 1;
    }
    if d1_len == 0 || d1_len > 2 {
        return None;
    }
    let day1: u32 = slice[..d1_len].parse().ok()?;
    let rest = &slice[d1_len..];

    // Range separator
    let seps = [" a ", " to ", "-", " ate ", " até "];
    let mut matched_sep = None;
    let mut after_sep_len = 0;
    for s in seps {
        if rest.to_lowercase().starts_with(s) {
            matched_sep = Some(s);
            after_sep_len = s.len();
            break;
        }
    }
    matched_sep?;

    let after_sep = &rest[after_sep_len..];
    let mut d2_len = 0;
    while d2_len < after_sep.len() && after_sep.as_bytes()[d2_len].is_ascii_digit() {
        d2_len += 1;
    }
    if d2_len == 0 || d2_len > 2 {
        return None;
    }
    let day2: u32 = after_sep[..d2_len].parse().ok()?;
    let after_d2 = &after_sep[d2_len..];

    // Now look for "de <month>" or " <month>"
    let mut rem = after_d2.trim_start();
    let mut consumed = d1_len + after_sep_len + d2_len + (after_d2.len() - rem.len());

    let de_prefix = ["de ", "of "];
    for d_p in de_prefix {
        if rem.to_lowercase().starts_with(d_p) {
            consumed += d_p.len();
            rem = &rem[d_p.len()..];
            break;
        }
    }

    let month_info = match_month_name(rem)?;
    consumed += month_info.len;
    rem = &rem[month_info.len..];

    // Optional year
    let (year, year_len) = parse_optional_year(rem);
    consumed += year_len;

    let final_year = match year {
        Some(y) => y,
        None => infer_year(month_info.month, day1, observed, term)?,
    };

    let date1 = NaiveDate::from_ymd_opt(final_year, month_info.month, day1)?;
    let date2 = NaiveDate::from_ymd_opt(final_year, month_info.month, day2)?;

    Some((date1, date2, consumed))
}

/// Parse "DD de Month [de YYYY]" or "DD Month [YYYY]" or "DD dez"
fn try_parse_day_month_text(
    slice: &str,
    observed: DateTime<Utc>,
    term: Option<(NaiveDate, NaiveDate)>,
) -> Option<(NaiveDate, usize)> {
    let mut d_len = 0;
    while d_len < slice.len() && slice.as_bytes()[d_len].is_ascii_digit() {
        d_len += 1;
    }
    if d_len == 0 || d_len > 2 {
        return None;
    }
    let day: u32 = slice[..d_len].parse().ok()?;
    let after_day = &slice[d_len..];

    let mut rem = after_day.trim_start();
    let mut consumed = d_len + (after_day.len() - rem.len());

    let de_prefix = ["de ", "of "];
    for d_p in de_prefix {
        if rem.to_lowercase().starts_with(d_p) {
            consumed += d_p.len();
            rem = &rem[d_p.len()..];
            break;
        }
    }

    let month_info = match_month_name(rem)?;
    consumed += month_info.len;
    rem = &rem[month_info.len..];

    // Optional year
    let (year, year_len) = parse_optional_year(rem);
    consumed += year_len;

    let final_year = match year {
        Some(y) => y,
        None => infer_year(month_info.month, day, observed, term)?,
    };

    let date = NaiveDate::from_ymd_opt(final_year, month_info.month, day)?;
    Some((date, consumed))
}

/// Parse numeric date with Portuguese convention: Day first!
/// Recognizes `DD/MM/YYYY`, `DD-MM-YYYY`, `DD.MM.YY`, `DD/MM`
fn try_parse_numeric_date(
    slice: &str,
    observed: DateTime<Utc>,
    term: Option<(NaiveDate, NaiveDate)>,
) -> Option<(NaiveDate, usize)> {
    let mut d_len = 0;
    while d_len < slice.len() && slice.as_bytes()[d_len].is_ascii_digit() {
        d_len += 1;
    }
    if d_len == 0 || d_len > 2 {
        return None;
    }
    let day: u32 = slice[..d_len].parse().ok()?;
    let rest = &slice[d_len..];

    if rest.is_empty() {
        return None;
    }
    let sep = rest.as_bytes()[0];
    if sep != b'/' && sep != b'-' && sep != b'.' {
        return None;
    }

    let after_sep1 = &rest[1..];
    let mut m_len = 0;
    while m_len < after_sep1.len() && after_sep1.as_bytes()[m_len].is_ascii_digit() {
        m_len += 1;
    }
    if m_len == 0 || m_len > 2 {
        return None;
    }
    let month: u32 = after_sep1[..m_len].parse().ok()?;
    let mut consumed = d_len + 1 + m_len;

    let after_m = &after_sep1[m_len..];
    let year = if !after_m.is_empty() && after_m.as_bytes()[0] == sep {
        let after_sep2 = &after_m[1..];
        let mut y_len = 0;
        while y_len < after_sep2.len() && after_sep2.as_bytes()[y_len].is_ascii_digit() {
            y_len += 1;
        }
        if y_len == 4 {
            let y: i32 = after_sep2[..4].parse().ok()?;
            consumed += 1 + 4;
            Some(y)
        } else if y_len == 2 {
            let y2: i32 = after_sep2[..2].parse().ok()?;
            consumed += 1 + 2;
            Some(2000 + y2)
        } else {
            None
        }
    } else {
        None
    };

    // Word boundary after match: cannot be immediately followed by digit or letter
    if slice.len() > consumed {
        let next_c = slice[consumed..].chars().next().unwrap_or(' ');
        if next_c.is_ascii_alphanumeric() {
            return None;
        }
    }

    let final_year = match year {
        Some(y) => y,
        None => infer_year(month, day, observed, term)?,
    };

    let date = NaiveDate::from_ymd_opt(final_year, month, day)?;
    Some((date, consumed))
}

/// Matches Portuguese and English month names and abbreviations.
fn match_month_name(slice: &str) -> Option<MonthInfo> {
    let months = [
        // Portuguese full
        ("janeiro", 1),
        ("fevereiro", 2),
        ("março", 3),
        ("marco", 3),
        ("abril", 4),
        ("maio", 5),
        ("junho", 6),
        ("julho", 7),
        ("agosto", 8),
        ("setembro", 9),
        ("outubro", 10),
        ("novembro", 11),
        ("dezembro", 12),
        // English full
        ("january", 1),
        ("february", 2),
        ("march", 3),
        ("april", 4),
        ("may", 5),
        ("june", 6),
        ("july", 7),
        ("august", 8),
        ("september", 9),
        ("october", 10),
        ("november", 11),
        ("december", 12),
        // Portuguese abbreviations
        ("jan", 1),
        ("fev", 2),
        ("mar", 3),
        ("abr", 4),
        ("mai", 5),
        ("jun", 6),
        ("jul", 7),
        ("ago", 8),
        ("set", 9),
        ("out", 10),
        ("nov", 11),
        ("dez", 12),
        // English abbreviations
        ("feb", 2),
        ("apr", 4),
        ("aug", 8),
        ("sep", 9),
        ("sept", 9),
        ("oct", 10),
        ("dec", 12),
    ];

    let lower = slice.to_lowercase();
    for (name, m) in months {
        if lower.starts_with(name) {
            let mut len = name.len();
            // Trailing dot for abbreviation, e.g. "dez."
            if slice[len..].starts_with('.') {
                len += 1;
            }
            // Ensure word boundary: next char cannot be alphanumeric
            if slice.len() > len {
                let next_char = slice[len..].chars().next().unwrap_or(' ');
                if next_char.is_alphanumeric() {
                    continue;
                }
            }
            return Some(MonthInfo { month: m, len });
        }
    }
    None
}

/// Parse optional year following month: " de 2026", " 2026", " 26"
fn parse_optional_year(slice: &str) -> (Option<i32>, usize) {
    let mut rem = slice;
    let mut consumed = 0;

    let trimmed = rem.trim_start();
    consumed += rem.len() - trimmed.len();
    rem = trimmed;

    let de_prefix = ["de ", "of "];
    for d_p in de_prefix {
        if rem.to_lowercase().starts_with(d_p) {
            consumed += d_p.len();
            rem = &rem[d_p.len()..];
            break;
        }
    }

    let trimmed2 = rem.trim_start();
    consumed += rem.len() - trimmed2.len();
    rem = trimmed2;

    let mut y_len = 0;
    while y_len < rem.len() && rem.as_bytes()[y_len].is_ascii_digit() {
        y_len += 1;
    }

    if y_len == 4 {
        if let Ok(y) = rem[..4].parse::<i32>() {
            return (Some(y), consumed + 4);
        }
    } else if y_len == 2 {
        return (
            rem[..2].parse::<i32>().ok().map(|y2| 2000 + y2),
            consumed + 2,
        );
    }

    (None, 0)
}

/// Year inference when missing:
/// "pick the year that puts the date inside `term`, or else the closest one on or after `observed - 60 days`."
fn infer_year(
    month: u32,
    day: u32,
    observed: DateTime<Utc>,
    term: Option<(NaiveDate, NaiveDate)>,
) -> Option<i32> {
    let obs_year = observed.year();

    // 1. If term is provided, check if any candidate year lands inside term
    if let Some((term_start, term_end)) = term {
        let min_y = term_start.year().min(obs_year - 1);
        let max_y = term_end.year().max(obs_year + 1);
        for y in min_y..=max_y {
            if NaiveDate::from_ymd_opt(y, month, day)
                .is_some_and(|d| d >= term_start && d <= term_end)
            {
                return Some(y);
            }
        }
    }

    // 2. Otherwise pick the closest one on or after `observed - 60 days`
    let threshold = observed.date_naive() - Duration::days(60);
    let candidate_years = [obs_year, obs_year + 1, obs_year - 1, obs_year + 2];

    let mut valid_candidates = Vec::new();
    for &y in &candidate_years {
        if let Some(d) = NaiveDate::from_ymd_opt(y, month, day).filter(|d| *d >= threshold) {
            let dist = (d - observed.date_naive()).num_days().abs();
            valid_candidates.push((dist, y));
        }
    }

    valid_candidates.sort_by_key(|&(dist, _)| dist);
    if let Some(&(_, y)) = valid_candidates.first() {
        return Some(y);
    }

    // Fallback: closest candidate regardless of threshold
    let mut all_candidates = Vec::new();
    for &y in &candidate_years {
        if let Some(d) = NaiveDate::from_ymd_opt(y, month, day) {
            let dist = (d - observed.date_naive()).num_days().abs();
            all_candidates.push((dist, y));
        }
    }
    all_candidates.sort_by_key(|&(dist, _)| dist);
    all_candidates.first().map(|&(_, y)| y)
}

/// Find kind hint by searching keywords in a ±80 character window around `span`.
fn find_kind_hint(text: &str, span: &Range<usize>) -> Option<ItemKind> {
    // Window boundaries on valid UTF-8 char boundaries
    let mut win_start = span.start.saturating_sub(80);
    while !text.is_char_boundary(win_start) {
        win_start = win_start.saturating_sub(1);
    }

    let mut win_end = (span.end + 80).min(text.len());
    while !text.is_char_boundary(win_end) {
        win_end = (win_end + 1).min(text.len());
    }

    let window_text = &text[win_start..win_end];
    let lower = window_text.to_lowercase();

    // Check keywords with priority and proximity
    let keyword_rules: &[(&[&str], ItemKind)] = &[
        (
            &[
                "exame",
                "época normal",
                "epoca normal",
                "época de recurso",
                "epoca de recurso",
                "recurso",
                "época especial",
                "epoca especial",
                "exam",
            ],
            ItemKind::Exam,
        ),
        (
            &[
                "mini-teste",
                "miniteste",
                "teste",
                "frequência",
                "frequencia",
                "midterm",
                "test",
            ],
            ItemKind::Test,
        ),
        (
            &[
                "entrega",
                "prazo",
                "submissão",
                "submissao",
                "data limite",
                "limite",
                "deadline",
                "due",
                "trabalho",
            ],
            ItemKind::Deadline,
        ),
        (
            &["apresentação", "apresentacao", "defesa", "presentation"],
            ItemKind::Presentation,
        ),
        (&["quiz", "questionário", "questionario"], ItemKind::Quiz),
        (
            &[
                "laboratório",
                "laboratorio",
                "aula prática",
                "aula pratica",
                "lab",
            ],
            ItemKind::Lab,
        ),
        (
            &[
                "aula cancelada",
                "cancelada",
                "cancelado",
                "adiada",
                "sem aula",
                "reposição",
                "reposicao",
                "postponed",
                "rescheduled",
            ],
            ItemKind::ScheduleChange,
        ),
        (
            &[
                "inscrição",
                "inscricao",
                "inscrições",
                "inscricoes",
                "registration",
                "enrollment",
            ],
            ItemKind::Registration,
        ),
    ];

    // Score = characters between the keyword and the date, plus a large penalty for each
    // sentence boundary in between, plus a small one for keywords after the date. In
    // "Mini-teste no dia 19/11. Exame a 15 de janeiro" the 19/11 belongs to the mini-teste
    // even though "Exame" starts closer. Offsets index `lower`, which can differ in length
    // from `window_text` only for exotic case mappings; scores then stay approximate.
    let mut best_match: Option<(usize, ItemKind)> = None;
    let date_start = span.start - win_start;
    let date_end = (span.end - win_start).min(lower.len());

    for (words, kind) in keyword_rules {
        for word in *words {
            let mut search_from = 0;
            while let Some(pos) = lower[search_from..].find(word) {
                let kw_start = search_from + pos;
                let kw_end = kw_start + word.len();
                let (gap_start, gap_end, after) = if kw_end <= date_start {
                    (kw_end, date_start, false)
                } else if kw_start >= date_end {
                    (date_end, kw_start, true)
                } else {
                    (date_start, date_start, false)
                };
                let between = lower.get(gap_start..gap_end).unwrap_or("");
                let boundaries = between.matches(". ").count()
                    + between.matches('\n').count()
                    + between.matches("; ").count();
                let score = (gap_end - gap_start) + boundaries * 1_000 + usize::from(after) * 5;
                if best_match.is_none_or(|(best, _)| score < best) {
                    best_match = Some((score, *kind));
                }
                search_from = kw_end;
            }
        }
    }

    best_match.map(|(_, kind)| kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_observed() -> DateTime<Utc> {
        DateTime::from_timestamp(1765540000, 0).unwrap() // Dec 2025
    }

    #[test]
    fn test_numeric_date_portuguese_convention() {
        let text = "Data limite: 03/04/2027 às 14:30.";
        let observed = sample_observed();
        let hits = find_dates(text, observed, None);
        assert_eq!(hits.len(), 1);
        // Must be 3rd of April (Day-first always), NOT March 4th
        assert_eq!(hits[0].date, NaiveDate::from_ymd_opt(2027, 4, 3).unwrap());
        assert_eq!(
            hits[0].time,
            Some(NaiveTime::from_hms_opt(14, 30, 0).unwrap())
        );
        assert_eq!(hits[0].kind_hint, Some(ItemKind::Deadline));
    }

    #[test]
    fn nearest_keyword_in_the_same_sentence_wins() {
        let text =
            "Mini-teste no dia 19/11/2026. Exame de época normal a 15 de janeiro de 2027, às 9h00.";
        let hits = find_dates(text, sample_observed(), None);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].kind_hint, Some(ItemKind::Test));
        assert_eq!(hits[1].kind_hint, Some(ItemKind::Exam));
        let text = "A aula de 13/11/2026 está cancelada. Reposição a 20 de novembro de 2026.";
        let hits = find_dates(text, sample_observed(), None);
        assert_eq!(hits.len(), 2);
        assert!(
            hits.iter()
                .all(|h| h.kind_hint == Some(ItemKind::ScheduleChange))
        );
    }

    #[test]
    fn test_rejects_feb_31() {
        let text = "Data: 31/02/2026.";
        let observed = sample_observed();
        let hits = find_dates(text, observed, None);
        assert!(hits.is_empty());
    }

    #[test]
    fn test_textual_dates_pt_and_en() {
        let observed = sample_observed();

        let pt = "O exame da época normal realiza-se em 12 de dezembro de 2026 às 14h.";
        let hits_pt = find_dates(pt, observed, None);
        assert_eq!(hits_pt.len(), 1);
        assert_eq!(
            hits_pt[0].date,
            NaiveDate::from_ymd_opt(2026, 12, 12).unwrap()
        );
        assert_eq!(
            hits_pt[0].time,
            Some(NaiveTime::from_hms_opt(14, 0, 0).unwrap())
        );
        assert_eq!(hits_pt[0].kind_hint, Some(ItemKind::Exam));

        let en = "Midterm test on December 12, 2026 at 2 pm.";
        let hits_en = find_dates(en, observed, None);
        assert_eq!(hits_en.len(), 1);
        assert_eq!(
            hits_en[0].date,
            NaiveDate::from_ymd_opt(2026, 12, 12).unwrap()
        );
        assert_eq!(
            hits_en[0].time,
            Some(NaiveTime::from_hms_opt(14, 0, 0).unwrap())
        );
        assert_eq!(hits_en[0].kind_hint, Some(ItemKind::Test));
    }

    #[test]
    fn test_time_ranges() {
        let observed = sample_observed();
        let text = "Aula prática em 15/05/2026 das 9:00 às 11:30.";
        let hits = find_dates(text, observed, None);
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].time,
            Some(NaiveTime::from_hms_opt(9, 0, 0).unwrap())
        );
        assert_eq!(
            hits[0].end_time,
            Some(NaiveTime::from_hms_opt(11, 30, 0).unwrap())
        );
        assert_eq!(hits[0].kind_hint, Some(ItemKind::Lab));
    }

    #[test]
    fn test_day_ranges() {
        let observed = sample_observed();
        let text = "Período de inscrições de 12 a 16 de janeiro de 2027.";
        let hits = find_dates(text, observed, None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].date, NaiveDate::from_ymd_opt(2027, 1, 12).unwrap());
        assert_eq!(
            hits[0].end_date,
            Some(NaiveDate::from_ymd_opt(2027, 1, 16).unwrap())
        );
        assert_eq!(hits[0].kind_hint, Some(ItemKind::Registration));
    }

    #[test]
    fn test_year_inference_december_to_january() {
        // Observed on 2026-12-15
        let observed = DateTime::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(2026, 12, 15)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap(),
            Utc,
        );
        let text = "1.º Teste em 12 de janeiro.";
        let hits = find_dates(text, observed, None);
        assert_eq!(hits.len(), 1);
        // January after December 2026 should infer year 2027
        assert_eq!(hits[0].date, NaiveDate::from_ymd_opt(2027, 1, 12).unwrap());
        assert_eq!(hits[0].kind_hint, Some(ItemKind::Test));
    }
}
