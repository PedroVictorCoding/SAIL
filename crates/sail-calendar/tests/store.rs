//! K3: store persistence, feed secrets file permissions and source erasure.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use chrono::Duration;
use common::{FEED_V1, FakeFetcher, TempDir, body, now};
use sail_calendar::contracts::{NewFeed, SourceKind};
use sail_calendar::service::CalendarService;

#[tokio::test]
async fn sqlite_sources_events_and_preferences_survive_reopen() {
    let tmp = TempDir::new();
    let fetcher = Arc::new(FakeFetcher::new(vec![body(FEED_V1)]));
    let service = CalendarService::open(&tmp.0, fetcher).expect("service");
    let feed = service
        .add_feed(
            NewFeed {
                name: "Personal".into(),
                url: "https://calendar.example.test/secret".into(),
                color: None,
            },
            now(),
        )
        .await
        .expect("feed");
    assert_eq!(service.sources().expect("sources").len(), 2);
    drop(service);

    let reopened =
        CalendarService::open(&tmp.0, Arc::new(FakeFetcher::new(Vec::new()))).expect("reopen");
    let sources = reopened.sources().expect("sources");
    assert_eq!(sources.len(), 2);
    assert!(sources.iter().any(|s| s.id == feed.id));
    let window = reopened
        .window(
            "2026-11-25T00:00:00Z".parse().unwrap(),
            "2026-11-26T00:00:00Z".parse().unwrap(),
            None,
            false,
        )
        .expect("window");
    assert_eq!(window.occurrences.len(), 1);
}

#[tokio::test]
async fn feeds_file_is_0600_and_delete_erases_events() {
    let tmp = TempDir::new();
    let service = CalendarService::open(&tmp.0, Arc::new(FakeFetcher::new(vec![body(FEED_V1)])))
        .expect("service");
    let feed = service
        .add_feed(
            NewFeed {
                name: "Personal".into(),
                url: "https://calendar.example.test/secret".into(),
                color: None,
            },
            now(),
        )
        .await
        .expect("feed");
    let mode = std::fs::metadata(tmp.0.join("feeds.json"))
        .expect("feeds")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    let contents = std::fs::read_to_string(tmp.0.join("feeds.json")).expect("feeds");
    assert!(contents.contains("calendar.example.test"));
    assert!(!contents.contains("src_local"));

    service.remove_source(&feed.id).expect("remove");
    assert!(matches!(
        service.remove_source(&feed.id),
        Err(sail_calendar::contracts::CalendarError::NotFound)
    ));
    let sources = service.sources().expect("sources");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].kind, SourceKind::Local);
    assert!(
        !std::fs::read_to_string(tmp.0.join("feeds.json"))
            .expect("feeds")
            .contains("calendar.example.test")
    );

    let immediate = service
        .sync_source("src_missing00000", now() + Duration::minutes(2))
        .await
        .expect_err("not found");
    assert_eq!(immediate, sail_calendar::contracts::CalendarError::NotFound);
}
