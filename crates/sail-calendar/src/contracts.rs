//! Wire and storage contracts for SAIL Calendar. Serde names are part of the API.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// serde: "local" | "ics_feed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Local,
    IcsFeed,
}

/// serde: "fresh" | "stale" | "failed" | "never".
///
/// `stale` means the last success is older than 2 × `refresh_minutes`; `failed` means the
/// last attempt failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Fresh,
    Stale,
    Failed,
    Never,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    pub kind: SourceKind,
    pub name: String,
    /// "#rrggbb"
    pub color: String,
    pub visible: bool,
    pub writable: bool,
    /// "https://calendar.google.com/…"; never the URL.
    pub url_hint: Option<String>,
    pub refresh_minutes: u32,
    pub event_count: u32,
    pub last_sync_at: Option<DateTime<Utc>>,
    /// Plain words, never contains the URL.
    pub last_error: Option<String>,
    pub freshness: Freshness,
    pub truncated: bool,
}

/// serde: "confirmed" | "tentative" | "cancelled".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    Confirmed,
    Tentative,
    Cancelled,
}

/// Internally tagged: `{"type":"timed",…}` or `{"type":"all_day",…}` (end exclusive).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventTime {
    Timed {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        tz: String,
    },
    AllDay {
        start_date: NaiveDate,
        end_date: NaiveDate,
    },
}

/// serde: "none" | "daily" | "weekdays" | "weekly" | "monthly" | "yearly".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatPreset {
    None,
    Daily,
    Weekdays,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub source_id: String,
    pub uid: String,
    /// RFC 5545 value as written, for overrides.
    pub recurrence_id: Option<String>,
    pub title: String,
    pub time: EventTime,
    pub location: Option<String>,
    pub notes: Option<String>,
    pub status: EventStatus,
    /// "FREQ=WEEKLY;BYDAY=MO" (no "RRULE:" prefix).
    pub rrule: Option<String>,
    /// RFC 3339 UTC or YYYY-MM-DD for all-day.
    pub exdates: Vec<String>,
    /// What the UI shows; "none" when the rrule does not match a preset (feed events).
    pub repeat: RepeatPreset,
    pub repeat_until: Option<NaiveDate>,
    pub prepare: bool,
    pub writable: bool,
    pub updated_at: DateTime<Utc>,
}

/// One concrete instance in a window. `key` is the event id plus "@" and the original
/// start (RFC 3339 UTC, or YYYY-MM-DD for all-day), stable across refreshes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Occurrence {
    pub key: String,
    pub event_id: String,
    pub source_id: String,
    pub title: String,
    pub time: EventTime,
    pub location: Option<String>,
    pub status: EventStatus,
    pub recurring: bool,
    pub prepare: bool,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventDraft {
    pub title: String,
    pub time: EventTime,
    pub location: Option<String>,
    pub notes: Option<String>,
    pub repeat: RepeatPreset,
    pub repeat_until: Option<NaiveDate>,
}

/// PATCH body; every field optional. Only `prepare` is accepted on read-only events.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventPatch {
    pub title: Option<String>,
    pub time: Option<EventTime>,
    pub location: Option<Option<String>>,
    pub notes: Option<Option<String>>,
    pub repeat: Option<RepeatPreset>,
    pub repeat_until: Option<Option<NaiveDate>>,
    pub prepare: Option<bool>,
}

/// serde: "all" | "this". `This` on delete adds an EXDATE and requires `occurrence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteScope {
    All,
    This,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewFeed {
    pub name: String,
    pub url: String,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourcePatch {
    pub name: Option<String>,
    pub color: Option<String>,
    pub visible: Option<bool>,
    pub refresh_minutes: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventsWindow {
    pub occurrences: Vec<Occurrence>,
    pub truncated: bool,
    pub sources: Vec<SourceFreshness>,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceFreshness {
    pub id: String,
    pub freshness: Freshness,
    pub last_sync_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportPreview {
    /// "imp_" + 12 hex, valid 15 min, kept in memory.
    pub preview_id: String,
    pub events: Vec<ImportCandidate>,
    pub skipped: Vec<SkipReason>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportCandidate {
    pub uid: String,
    pub title: String,
    pub time: EventTime,
    pub recurring: bool,
    pub duplicate: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkipReason {
    pub reason: String,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportResult {
    pub imported: u32,
    pub duplicates: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusyRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// serde `first_weekday`: "monday" | "sunday"; `default_view`: "month" | "week" | "day"
/// | "agenda".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preferences {
    /// IANA name, validated with chrono-tz; default "Europe/Lisbon".
    pub time_zone: String,
    /// Default "monday".
    pub first_weekday: String,
    /// Default "week".
    pub default_view: String,
    /// 0..=23, default 7.
    pub day_start_hour: u8,
    /// 1..=24, greater than `day_start_hour`, default 23.
    pub day_end_hour: u8,
}

/// A feed event moved or cancelled, starting within the next 14 days (fixed a priori).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedChange {
    pub event_id: String,
    pub source_id: String,
    pub title: String,
    pub kind: FeedChangeKind,
    /// All-day events use local midnight in the owner zone.
    pub old_start: Option<DateTime<Utc>>,
    pub new_start: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedChangeKind {
    Moved,
    Cancelled,
}

/// Broadcast after any store write: `{"revision":42,"source_id":"src_…"|null}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalendarChanged {
    pub revision: u64,
    pub source_id: Option<String>,
}

/// Error codes are serde snake_case strings.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CalendarError {
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("not found")]
    NotFound,
    #[error("this calendar is read-only")]
    ReadOnly,
    #[error("synced less than a minute ago")]
    RateLimited,
    #[error("could not fetch the feed: {0}")]
    FetchFailed(String),
    #[error("that address points inside a private network")]
    BlockedAddress,
    #[error("the feed is larger than 5 MB")]
    TooLarge,
    #[error("that address did not return a calendar")]
    NotCalendar,
    #[error("at most 20 calendars")]
    TooManySources,
    #[error("storage: {0}")]
    Storage(String),
}

impl CalendarError {
    /// Stable machine code for the HTTP error body.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid",
            Self::NotFound => "not_found",
            Self::ReadOnly => "read_only",
            Self::RateLimited => "rate_limited",
            Self::FetchFailed(_) => "fetch_failed",
            Self::BlockedAddress => "blocked_address",
            Self::TooLarge => "too_large",
            Self::NotCalendar => "not_calendar",
            Self::TooManySources => "too_many_sources",
            Self::Storage(_) => "storage",
        }
    }
}
