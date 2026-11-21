//! Shared test helpers: a fake fetcher and fixed event builders.
#![allow(dead_code)] // one shared module serves several test binaries

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sail_calendar::contracts::{CalendarError, Event, EventStatus, EventTime, RepeatPreset};
use sail_calendar::fetch::{FeedFetcher, FetchOutcome};
use url::Url;

pub struct FakeFetcher {
    responses: Mutex<VecDeque<Result<FetchOutcome, CalendarError>>>,
    calls: Mutex<Vec<(String, Option<String>)>>,
}

impl FakeFetcher {
    pub fn new(responses: Vec<Result<FetchOutcome, CalendarError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<(String, Option<String>)> {
        self.calls.lock().expect("calls").clone()
    }
}

#[async_trait]
impl FeedFetcher for FakeFetcher {
    async fn fetch(
        &self,
        url: &Url,
        etag: Option<&str>,
        _last_modified: Option<&str>,
    ) -> Result<FetchOutcome, CalendarError> {
        self.calls
            .lock()
            .expect("calls")
            .push((url.to_string(), etag.map(str::to_string)));
        self.responses
            .lock()
            .expect("responses")
            .pop_front()
            .unwrap_or_else(|| Err(CalendarError::FetchFailed("no response queued".into())))
    }
}

pub fn body(text: &str) -> Result<FetchOutcome, CalendarError> {
    Ok(FetchOutcome::Body {
        text: text.to_string(),
        etag: Some("\"v1\"".into()),
        last_modified: None,
    })
}

pub fn now() -> DateTime<Utc> {
    "2026-11-20T10:00:00Z".parse().expect("fixed time")
}

pub fn at(day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
    format!("2026-11-{day:02}T{hour:02}:{minute:02}:00Z")
        .parse()
        .expect("fixed time")
}

pub fn timed_event(id: &str, title: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> Event {
    Event {
        id: id.into(),
        source_id: "src_local000000".into(),
        uid: format!("{id}@local"),
        recurrence_id: None,
        title: title.into(),
        time: EventTime::Timed {
            start,
            end,
            tz: "Europe/Lisbon".into(),
        },
        location: None,
        notes: None,
        status: EventStatus::Confirmed,
        rrule: None,
        exdates: Vec::new(),
        repeat: RepeatPreset::None,
        repeat_until: None,
        prepare: false,
        writable: true,
        updated_at: now(),
    }
}

pub const FEED_V1: &str = "BEGIN:VCALENDAR\nVERSION:2.0\nBEGIN:VEVENT\nUID:lab@example.test\nDTSTAMP:20261101T120000Z\nDTSTART:20261125T090000Z\nDTEND:20261125T100000Z\nSUMMARY:Physics lab\nEND:VEVENT\nEND:VCALENDAR\n";

pub const FEED_V2_MOVED: &str = "BEGIN:VCALENDAR\nVERSION:2.0\nBEGIN:VEVENT\nUID:lab@example.test\nDTSTAMP:20261101T120000Z\nDTSTART:20261126T140000Z\nDTEND:20261126T150000Z\nSUMMARY:Physics lab\nEND:VEVENT\nEND:VCALENDAR\n";

pub const FEED_V3_CANCELLED: &str = "BEGIN:VCALENDAR\nVERSION:2.0\nBEGIN:VEVENT\nUID:lab@example.test\nDTSTAMP:20261101T120000Z\nDTSTART:20261126T140000Z\nDTEND:20261126T150000Z\nSUMMARY:Physics lab\nSTATUS:CANCELLED\nEND:VEVENT\nEND:VCALENDAR\n";

pub const IMPORT_V1: &str = "BEGIN:VCALENDAR\nVERSION:2.0\nBEGIN:VEVENT\nUID:dentist-import@example.test\nDTSTAMP:20261101T120000Z\nDTSTART:20261127T150000Z\nDTEND:20261127T160000Z\nSUMMARY:Dentist\nEND:VEVENT\nBEGIN:VEVENT\nUID:lab-import@example.test\nDTSTAMP:20261101T120000Z\nDTSTART:20261128T090000Z\nDTEND:20261128T110000Z\nSUMMARY:Lab report draft\nEND:VEVENT\nEND:VCALENDAR\n";

pub struct TempDir(pub std::path::PathBuf);

impl TempDir {
    pub fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("sail-calendar-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
