//! Date and time parsing for XTB reports.
//!
//! Handles both spreadsheet serial datetime cells and text dates in English and
//! Portuguese export formats (`dd.mm.yyyy HH:MM:SS`, `dd.mm.yyyy`, `yyyy-mm-dd HH:MM:SS`,
//! etc.), converting them into UTC [`EventTime`] according to the report's time zone.
//! Handles daylight saving time transitions: the fall-back hour selects the earliest
//! instant with an [`ImportIssue`](crate::contracts::ImportIssue).
use crate::contracts::{EventTime, ImportIssue, RowRef, Severity, TimePrecision, date_only};
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

use super::table::RawCell;

/// Outcome of parsing a date or datetime cell.
#[derive(Debug, PartialEq)]
pub struct ParsedTime {
    /// The parsed UTC event time.
    pub time: EventTime,
    /// Any DST warning or notice generated during conversion.
    pub issue: Option<ImportIssue>,
}

/// Convert a local [`NaiveDateTime`] to a UTC [`EventTime`] in `tz`, recording DST issues.
pub fn local_datetime_to_utc(naive: NaiveDateTime, tz: Tz, source: Option<RowRef>) -> ParsedTime {
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => ParsedTime {
            time: EventTime {
                at: dt.with_timezone(&Utc),
                precision: TimePrecision::Exact,
            },
            issue: None,
        },
        chrono::LocalResult::Ambiguous(earliest, _latest) => {
            let issue = ImportIssue {
                severity: Severity::Info,
                code: "ambiguous_dst_time".to_string(),
                message: format!(
                    "ambiguous local time {naive} during DST fall-back resolved to earlier instant"
                ),
                source,
            };
            ParsedTime {
                time: EventTime {
                    at: earliest.with_timezone(&Utc),
                    precision: TimePrecision::Exact,
                },
                issue: Some(issue),
            }
        }
        chrono::LocalResult::None => {
            // Spring-forward skipped hour: fall back to UTC representation of the naive time.
            let issue = ImportIssue {
                severity: Severity::Info,
                code: "dst_gap_time".to_string(),
                message: format!(
                    "local time {naive} skipped by DST transition resolved directly to UTC"
                ),
                source,
            };
            ParsedTime {
                time: EventTime {
                    at: Utc.from_utc_datetime(&naive),
                    precision: TimePrecision::Exact,
                },
                issue: Some(issue),
            }
        }
    }
}

/// Parse a date or datetime string into [`ParsedTime`].
pub fn parse_time_text(text: &str, tz: Tz, source: Option<RowRef>) -> Option<ParsedTime> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }

    // Try full datetime formats first.
    let datetime_formats = [
        "%d.%m.%Y %H:%M:%S",
        "%d.%m.%Y %H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%d/%m/%Y %H:%M:%S",
        "%d/%m/%Y %H:%M",
    ];

    for fmt in datetime_formats {
        if let Ok(naive) = NaiveDateTime::parse_from_str(t, fmt) {
            return Some(local_datetime_to_utc(naive, tz, source));
        }
    }

    // Try ISO 8601 / RFC 3339 if already timestamped.
    if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
        return Some(ParsedTime {
            time: EventTime {
                at: dt.with_timezone(&Utc),
                precision: TimePrecision::Exact,
            },
            issue: None,
        });
    }

    // Try date-only formats.
    let date_formats = ["%d.%m.%Y", "%Y-%m-%d", "%d/%m/%Y"];
    for fmt in date_formats {
        if let Ok(date) = NaiveDate::parse_from_str(t, fmt) {
            return Some(ParsedTime {
                time: date_only(date, tz),
                issue: None,
            });
        }
    }

    None
}

/// Parse a [`RawCell`] into an [`EventTime`].
pub fn parse_cell_time(cell: &RawCell, tz: Tz, source: Option<RowRef>) -> Option<ParsedTime> {
    match cell {
        RawCell::DateTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
            has_time,
            ..
        } => {
            let date = NaiveDate::from_ymd_opt(*year as i32, *month as u32, *day as u32)?;
            if *has_time {
                let naive = date.and_hms_opt(*hour as u32, *minute as u32, *second as u32)?;
                Some(local_datetime_to_utc(naive, tz, source))
            } else {
                Some(ParsedTime {
                    time: date_only(date, tz),
                    issue: None,
                })
            }
        }
        RawCell::Text(s) => parse_time_text(s, tz, source),
        RawCell::Float(serial) => excel_serial_time(*serial, tz, source),
        RawCell::Int(serial) => excel_serial_time(*serial as f64, tz, source),
        RawCell::Empty => None,
    }
}

/// Earliest and latest spreadsheet serial accepted as a date: 1954-10-03 and 2119-01-10.
/// A number outside this range in a time column is not a date.
const SERIAL_RANGE: std::ops::RangeInclusive<f64> = 20_000.0..=80_000.0;

/// Read a spreadsheet serial date-time (days since 1899-12-30, the 1900 date system) that
/// arrives as a plain number because the cell has no date format. XTB's exports store
/// times this way. Rounded to the nearest second.
fn excel_serial_time(serial: f64, tz: Tz, source: Option<RowRef>) -> Option<ParsedTime> {
    if !serial.is_finite() || !SERIAL_RANGE.contains(&serial) {
        return None;
    }
    let epoch = NaiveDate::from_ymd_opt(1899, 12, 30)?.and_hms_opt(0, 0, 0)?;
    let seconds = (serial * 86_400.0).round() as i64;
    let naive = epoch.checked_add_signed(chrono::Duration::seconds(seconds))?;
    Some(local_datetime_to_utc(naive, tz, source))
}
