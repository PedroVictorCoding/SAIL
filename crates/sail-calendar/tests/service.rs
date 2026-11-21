//! K5: local CRUD, windows, feeds, sync, preferences and agenda.

mod common;

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use common::{FEED_V1, FEED_V2_MOVED, FEED_V3_CANCELLED, FakeFetcher, TempDir, at, body, now};
use sail_calendar::contracts::{
    CalendarError, DeleteScope, EventDraft, EventPatch, EventStatus, EventTime, NewFeed,
    RepeatPreset, SourceKind,
};
use sail_calendar::fetch::FetchOutcome;
use sail_calendar::service::CalendarService;

fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 0.001);
}

fn timed(start: chrono::DateTime<Utc>, hours: i64) -> EventTime {
    EventTime::Timed {
        start,
        end: start + Duration::hours(hours),
        tz: "Europe/Lisbon".into(),
    }
}

#[test]
fn local_crud_window_freebusy_and_preferences() {
    let tmp = TempDir::new();
    let fetcher = Arc::new(FakeFetcher::new(Vec::new()));
    let service = CalendarService::open(&tmp.0, fetcher).expect("service");
    assert_eq!(service.sources().expect("sources").len(), 1);
    assert_eq!(
        service.sources().expect("sources")[0].kind,
        SourceKind::Local
    );

    let event = service
        .create(
            EventDraft {
                title: "Dentist".into(),
                time: timed(at(24, 15, 0), 1),
                location: Some("Clinic".into()),
                notes: Some("Bring the card".into()),
                repeat: RepeatPreset::None,
                repeat_until: None,
            },
            now(),
        )
        .expect("create");
    assert!(event.id.starts_with("loc_"));
    assert!(event.writable);

    let window = service
        .window(at(24, 0, 0), at(25, 0, 0), None, false)
        .expect("window");
    assert_eq!(window.occurrences.len(), 1);
    assert_eq!(window.occurrences[0].title, "Dentist");
    assert!(window.revision > 0);

    let updated = service
        .update(
            &event.id,
            EventPatch {
                title: Some("Dentist (moved)".into()),
                prepare: Some(true),
                ..Default::default()
            },
            now(),
        )
        .expect("update");
    assert_eq!(updated.title, "Dentist (moved)");
    assert!(service.event(&event.id).expect("event").prepare);

    let busy = service
        .freebusy(at(24, 0, 0), at(25, 0, 0))
        .expect("freebusy");
    assert_eq!(busy.len(), 1);
    assert_eq!(busy[0].start, at(24, 15, 0));
    assert_eq!(busy[0].end, at(24, 16, 0));

    let mut prefs = service.preferences().expect("prefs");
    prefs.time_zone = "Europe/Lisbon".into();
    prefs.day_start_hour = 8;
    prefs.day_end_hour = 22;
    assert_eq!(service.set_preferences(prefs.clone()).expect("set"), prefs);
    let mut bad = prefs;
    bad.time_zone = "Nowhere/Zone".into();
    assert!(matches!(
        service.set_preferences(bad),
        Err(CalendarError::Invalid(_))
    ));

    service
        .delete(&event.id, DeleteScope::All, None)
        .expect("delete");
    assert!(matches!(
        service.event(&event.id),
        Err(CalendarError::NotFound)
    ));
    close(0.0, 0.0);
}

#[tokio::test]
async fn feed_add_sync_not_modified_and_read_only_annotation() {
    let tmp = TempDir::new();
    let fetcher = Arc::new(FakeFetcher::new(vec![
        body(FEED_V1),
        body(FEED_V2_MOVED),
        Ok(FetchOutcome::NotModified),
    ]));
    let service = CalendarService::open(&tmp.0, fetcher.clone()).expect("service");
    let source = service
        .add_feed(
            NewFeed {
                name: "Personal".into(),
                url: "webcal://calendar.example.test/secret/address".into(),
                color: Some("#2f9e72".into()),
            },
            now(),
        )
        .await
        .expect("add feed");
    assert_eq!(source.kind, SourceKind::IcsFeed);
    assert_eq!(source.event_count, 1);
    assert_eq!(
        source.url_hint.as_deref(),
        Some("https://calendar.example.test/…")
    );
    assert!(
        !serde_json::to_string(&source)
            .expect("json")
            .contains("secret")
    );
    assert!(tmp.0.join("feeds.json").is_file());

    let missing = service
        .add_feed(
            NewFeed {
                name: "Bad".into(),
                url: "http://plain.test/x".into(),
                color: None,
            },
            now(),
        )
        .await
        .expect_err("http refused");
    assert!(matches!(missing, CalendarError::Invalid(_)));

    let before = service
        .sources()
        .expect("sources")
        .into_iter()
        .find(|s| s.id == source.id)
        .expect("feed");
    assert_eq!(before.event_count, 1);
    let window = service
        .window(at(25, 0, 0), at(26, 0, 0), None, false)
        .expect("window");
    assert_eq!(window.occurrences.len(), 1);
    let event_id = window.occurrences[0].event_id.clone();

    assert!(matches!(
        service.update(
            &event_id,
            EventPatch {
                title: Some("Changed".into()),
                ..Default::default()
            },
            now() + Duration::minutes(2)
        ),
        Err(CalendarError::ReadOnly)
    ));
    service
        .update(
            &event_id,
            EventPatch {
                prepare: Some(true),
                ..Default::default()
            },
            now() + Duration::minutes(2),
        )
        .expect("prepare is allowed");
    assert!(service.event(&event_id).expect("event").prepare);

    let immediate = service
        .sync_source(&source.id, now() + Duration::seconds(30))
        .await
        .expect_err("rate limited");
    assert_eq!(immediate, CalendarError::RateLimited);

    service
        .sync_source(&source.id, now() + Duration::minutes(2))
        .await
        .expect("moved sync");
    let after = service
        .window(at(26, 0, 0), at(27, 0, 0), None, false)
        .expect("window");
    assert_eq!(after.occurrences.len(), 1);
    assert_eq!(after.occurrences[0].title, "Physics lab");
    let changes = service.recent_changes(now());
    assert!(
        changes
            .iter()
            .any(|change| change.kind == sail_calendar::contracts::FeedChangeKind::Moved)
    );
    service
        .sync_source(&source.id, now() + Duration::minutes(4))
        .await
        .expect("not modified");

    let cancelled_service = CalendarService::open(
        &tmp.0,
        Arc::new(FakeFetcher::new(vec![body(FEED_V3_CANCELLED)])),
    )
    .expect("service");
    cancelled_service
        .sync_source(&source.id, now() + Duration::minutes(6))
        .await
        .expect("cancel sync");
    let changes = cancelled_service.recent_changes(now());
    assert!(
        changes
            .iter()
            .any(|change| change.kind == sail_calendar::contracts::FeedChangeKind::Cancelled)
    );

    let local = service.sources().expect("sources")[0].id.clone();
    assert!(matches!(
        service.remove_source(&local),
        Err(CalendarError::Invalid(_))
    ));
    service.remove_source(&source.id).expect("remove");
    assert_eq!(service.sources().expect("sources").len(), 1);
    assert!(fetcher.calls().len() >= 3);
}

#[test]
fn agenda_lines_are_deterministic_and_have_location() {
    let occurrence = sail_calendar::contracts::Occurrence {
        key: "local@1".into(),
        event_id: "loc_1".into(),
        source_id: "src_local000000".into(),
        title: "Algorithms lecture".into(),
        time: EventTime::Timed {
            start: at(24, 9, 0),
            end: at(24, 10, 30),
            tz: "Europe/Lisbon".into(),
        },
        location: Some("ISCTE".into()),
        status: EventStatus::Confirmed,
        recurring: false,
        prepare: false,
        writable: true,
    };
    let lines = sail_calendar::agenda::agenda_lines(&[occurrence], chrono_tz::Europe::Lisbon, 8);
    assert_eq!(lines, vec!["09:00–10:30 Algorithms lecture (ISCTE)"]);
    let utc = Utc.with_ymd_and_hms(2026, 11, 24, 9, 0, 0).unwrap();
    let _ = utc;
}
