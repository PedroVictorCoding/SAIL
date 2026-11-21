//! Round 28 acceptance: the owner's day through the public `CalendarService`, with a
//! fake fetcher standing in for Google. Each scenario is one the owner hits in real use.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sail_calendar::contracts::{
    CalendarError, DeleteScope, EventDraft, EventPatch, EventTime, FeedChangeKind, NewFeed,
    RepeatPreset,
};
use sail_calendar::fetch::{FeedFetcher, FetchOutcome};
use sail_calendar::service::CalendarService;
use url::Url;

/// The secret part of a Google "Secret address in iCal format".
const SECRET: &str = "s3cr3t-token-abc123";

struct Feed(Mutex<VecDeque<Result<FetchOutcome, CalendarError>>>);

#[async_trait]
impl FeedFetcher for Feed {
    async fn fetch(
        &self,
        _url: &Url,
        _etag: Option<&str>,
        _last_modified: Option<&str>,
    ) -> Result<FetchOutcome, CalendarError> {
        self.0
            .lock()
            .expect("queue")
            .pop_front()
            .unwrap_or_else(|| Err(CalendarError::FetchFailed("nothing queued".into())))
    }
}

fn ok(text: String) -> Result<FetchOutcome, CalendarError> {
    Ok(FetchOutcome::Body {
        text,
        etag: None,
        last_modified: None,
    })
}

struct Tmp(PathBuf);
impl Tmp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "sail-calendar-e2e-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        Self(dir)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn t(s: &str) -> DateTime<Utc> {
    s.parse().expect("time")
}

/// A Google-style export. Google stamps every VEVENT's DTSTAMP with the export time,
/// so `stamp` changes on every fetch even when nothing else does.
fn google(stamp: &str, extra: &str, lab_start: &str, with_seminar: bool) -> String {
    let seminar = if with_seminar {
        format!(
            "BEGIN:VEVENT\r\nUID:seminar@example.test\r\nDTSTAMP:{stamp}\r\n\
             DTSTART:20261127T160000Z\r\nDTEND:20261127T170000Z\r\nSUMMARY:Seminar\r\nEND:VEVENT\r\n"
        )
    } else {
        String::new()
    };
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\n\
         BEGIN:VEVENT\r\nUID:algo@example.test\r\nDTSTAMP:{stamp}\r\n\
         DTSTART;TZID=Europe/Lisbon:20261019T090000\r\nDTEND;TZID=Europe/Lisbon:20261019T103000\r\n\
         RRULE:FREQ=WEEKLY;BYDAY=MO\r\nSUMMARY:Algorithms lecture\r\nLOCATION:ISCTE\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nUID:lab@example.test\r\nDTSTAMP:{stamp}\r\n\
         DTSTART:{lab_start}\r\nDTEND:20261127T170000Z\r\nSUMMARY:Physics lab\r\nEND:VEVENT\r\n\
         {seminar}{extra}END:VCALENDAR\r\n"
    )
}

/// Google's override when one Monday lecture moves: same UID, a RECURRENCE-ID.
const MOVED_LECTURE: &str = "BEGIN:VEVENT\r\nUID:algo@example.test\r\nDTSTAMP:20261121T100000Z\r\n\
    RECURRENCE-ID;TZID=Europe/Lisbon:20261130T090000\r\n\
    DTSTART;TZID=Europe/Lisbon:20261201T100000\r\nDTEND;TZID=Europe/Lisbon:20261201T113000\r\n\
    SUMMARY:Algorithms lecture\r\nEND:VEVENT\r\n";

fn titles_between(
    service: &CalendarService,
    start: &str,
    end: &str,
) -> Vec<(String, DateTime<Utc>)> {
    service
        .window(t(start), t(end), None, false)
        .expect("window")
        .occurrences
        .into_iter()
        .map(|occ| {
            let start = match occ.time {
                EventTime::Timed { start, .. } => start,
                EventTime::AllDay { start_date, .. } => {
                    start_date.and_hms_opt(0, 0, 0).expect("midnight").and_utc()
                }
            };
            (occ.title, start)
        })
        .collect()
}

#[tokio::test]
async fn a_google_feed_through_a_real_week() {
    let tmp = Tmp::new("google");
    let now = t("2026-11-20T10:00:00Z");
    let queue = VecDeque::from(vec![
        ok(google("20261120T100000Z", "", "20261126T150000Z", true)),
        // 31 min later: only DTSTAMP differs.
        ok(google("20261120T103100Z", "", "20261126T150000Z", true)),
        // An hour later: lab moved a day, one lecture moved, seminar deleted.
        ok(google(
            "20261120T113100Z",
            MOVED_LECTURE,
            "20261127T150000Z",
            false,
        )),
        Err(CalendarError::FetchFailed(
            "the feed returned HTTP 404".into(),
        )),
    ]);
    let service = CalendarService::open(&tmp.0, Arc::new(Feed(Mutex::new(queue)))).expect("open");

    // 1. Connect in one paste.
    let url = format!("webcal://calendar.google.com/calendar/ical/me/private-{SECRET}/basic.ics");
    let source = service
        .add_feed(
            NewFeed {
                name: "Personal".into(),
                url,
                color: None,
            },
            now,
        )
        .await
        .expect("add feed");
    assert_eq!(
        source.url_hint.as_deref(),
        Some("https://calendar.google.com/…")
    );
    let listed = serde_json::to_string(&service.sources().expect("sources")).expect("json");
    assert!(
        !listed.contains(SECRET),
        "the secret never leaves the store"
    );

    // 2. DST: the weekly 09:00 Lisbon lecture is 08:00Z in summer time, 09:00Z in winter.
    let october = titles_between(&service, "2026-10-19T00:00:00Z", "2026-10-20T00:00:00Z");
    assert_eq!(
        october,
        vec![("Algorithms lecture".into(), t("2026-10-19T08:00:00Z"))]
    );
    let week = titles_between(&service, "2026-11-23T00:00:00Z", "2026-11-30T00:00:00Z");
    assert_eq!(
        week,
        vec![
            ("Algorithms lecture".into(), t("2026-11-23T09:00:00Z")),
            ("Physics lab".into(), t("2026-11-26T15:00:00Z")),
            ("Seminar".into(), t("2026-11-27T16:00:00Z")),
        ]
    );

    // 3. Read-only, except for the prepare flag.
    let lab = service
        .window(
            t("2026-11-26T00:00:00Z"),
            t("2026-11-27T00:00:00Z"),
            None,
            false,
        )
        .expect("window")
        .occurrences
        .remove(0);
    let renamed = service.update(
        &lab.event_id,
        EventPatch {
            title: Some("Hijacked".into()),
            ..EventPatch::default()
        },
        now,
    );
    assert_eq!(renamed, Err(CalendarError::ReadOnly));
    assert_eq!(
        service.delete(&lab.event_id, DeleteScope::All, None),
        Err(CalendarError::ReadOnly)
    );
    service
        .update(
            &lab.event_id,
            EventPatch {
                prepare: Some(true),
                ..EventPatch::default()
            },
            now,
        )
        .expect("prepare");

    // 4. A refresh where only Google's DTSTAMP moved changes nothing the clients see.
    let before = service
        .window(
            t("2026-11-23T00:00:00Z"),
            t("2026-11-30T00:00:00Z"),
            None,
            false,
        )
        .expect("window")
        .revision;
    service
        .sync_source(&source.id, now + Duration::minutes(31))
        .await
        .expect("sync 2");
    let after = service
        .window(
            t("2026-11-23T00:00:00Z"),
            t("2026-11-30T00:00:00Z"),
            None,
            false,
        )
        .expect("window")
        .revision;
    assert_eq!(
        before, after,
        "a DTSTAMP-only refresh must not bump the revision"
    );
    assert!(
        service.event(&lab.event_id).expect("lab").prepare,
        "prepare survives a refresh"
    );

    // 5. Moves and cancellations become changes the owner hears about.
    let third = now + Duration::minutes(91);
    service
        .sync_source(&source.id, third)
        .await
        .expect("sync 3");
    let mut changes = service.recent_changes(third - Duration::seconds(1));
    changes.sort_by(|a, b| a.title.cmp(&b.title));
    let summary: Vec<(String, FeedChangeKind, Option<DateTime<Utc>>)> = changes
        .iter()
        .map(|c| (c.title.clone(), c.kind, c.new_start))
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "Algorithms lecture".into(),
                FeedChangeKind::Moved,
                Some(t("2026-12-01T10:00:00Z"))
            ),
            (
                "Physics lab".into(),
                FeedChangeKind::Moved,
                Some(t("2026-11-27T15:00:00Z"))
            ),
            ("Seminar".into(), FeedChangeKind::Cancelled, None),
        ]
    );
    let next_week = titles_between(&service, "2026-11-30T00:00:00Z", "2026-12-02T00:00:00Z");
    assert_eq!(
        next_week,
        vec![("Algorithms lecture".into(), t("2026-12-01T10:00:00Z"))],
        "the override replaces Monday's instance"
    );
    assert!(
        service.event(&lab.event_id).expect("lab").prepare,
        "prepare survives a move"
    );

    // 6. A failing feed says so, in plain words, without the URL.
    let failed = service
        .sync_source(&source.id, third + Duration::minutes(31))
        .await;
    assert!(failed.is_err());
    let listed = service.sources().expect("sources");
    let feed = listed.iter().find(|s| s.id == source.id).expect("feed");
    let error = feed.last_error.clone().expect("last error");
    assert!(!error.contains(SECRET) && !error.contains("calendar.google.com/calendar"));
}

#[tokio::test]
async fn local_events_repeat_import_and_dedup() {
    let tmp = Tmp::new("local");
    let now = t("2026-11-20T10:00:00Z");
    let service =
        CalendarService::open(&tmp.0, Arc::new(Feed(Mutex::new(VecDeque::new())))).expect("open");
    let gym = service
        .create(
            EventDraft {
                title: "Gym".into(),
                time: EventTime::Timed {
                    start: t("2026-11-23T18:00:00Z"),
                    end: t("2026-11-23T19:00:00Z"),
                    tz: "Europe/Lisbon".into(),
                },
                location: None,
                notes: None,
                repeat: RepeatPreset::Weekly,
                repeat_until: None,
            },
            now,
        )
        .expect("create");
    let three_weeks = |s: &CalendarService| {
        titles_between(s, "2026-11-23T00:00:00Z", "2026-12-14T00:00:00Z").len()
    };
    assert_eq!(three_weeks(&service), 3);
    service
        .delete(
            &gym.id,
            DeleteScope::This,
            Some(&format!("{}@2026-11-30T18:00:00+00:00", gym.id)),
        )
        .or_else(|_| {
            service.delete(
                &gym.id,
                DeleteScope::This,
                Some(&format!("{}@2026-11-30T18:00:00Z", gym.id)),
            )
        })
        .expect("delete one");
    assert_eq!(
        three_weeks(&service),
        2,
        "skipping one week leaves the rest"
    );

    let ics = google("20261120T100000Z", "", "20261126T150000Z", false);
    let preview = service.import_preview(&ics, now).expect("preview");
    assert_eq!(preview.events.len(), 2);
    assert!(preview.events.iter().all(|e| !e.duplicate));
    let result = service
        .import_commit(&preview.preview_id, now)
        .expect("commit");
    assert_eq!((result.imported, result.duplicates), (2, 0));
    let again = service.import_preview(&ics, now).expect("preview again");
    assert!(
        again.events.iter().all(|e| e.duplicate),
        "a second import is all duplicates"
    );
    let imported = titles_between(&service, "2026-11-26T00:00:00Z", "2026-11-27T00:00:00Z");
    assert_eq!(
        imported,
        vec![("Physics lab".into(), t("2026-11-26T15:00:00Z"))]
    );
}

#[tokio::test]
async fn a_large_feed_syncs_quickly() {
    let tmp = Tmp::new("large");
    let now = t("2026-11-20T10:00:00Z");
    let mut text = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n");
    for i in 0..5_000u32 {
        let day = 1 + (i % 28);
        text.push_str(&format!(
            "BEGIN:VEVENT\r\nUID:e{i}@example.test\r\nDTSTAMP:20261120T100000Z\r\n\
             DTSTART:202612{day:02}T{h:02}0000Z\r\nDTEND:202612{day:02}T{h:02}3000Z\r\n\
             SUMMARY:Event {i}\r\nEND:VEVENT\r\n",
            h = 8 + (i % 10)
        ));
    }
    text.push_str("END:VCALENDAR\r\n");
    let second = text.replace("DTSTAMP:20261120T100000Z", "DTSTAMP:20261120T103100Z");
    let queue = VecDeque::from(vec![ok(text), ok(second)]);
    let service = CalendarService::open(&tmp.0, Arc::new(Feed(Mutex::new(queue)))).expect("open");
    let started = Instant::now();
    let source = service
        .add_feed(
            NewFeed {
                name: "Big".into(),
                url: "https://calendar.example.test/big.ics".into(),
                color: None,
            },
            now,
        )
        .await
        .expect("add");
    service
        .sync_source(&source.id, now + Duration::minutes(31))
        .await
        .expect("sync");
    let elapsed = started.elapsed();
    // Fixed a priori: two syncs of 5 000 events must not take more than a few seconds
    // on the host HDD (one transaction per sync, not one fsync per event).
    assert!(elapsed.as_secs_f64() < 5.0, "two syncs took {elapsed:?}");
    assert_eq!(source.event_count, 5_000);
}
