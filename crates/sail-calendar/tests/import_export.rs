//! K5: ICS import preview/commit with dedup, and export.

mod common;

use std::sync::Arc;

use chrono::Duration;
use common::{FakeFetcher, IMPORT_V1, TempDir, now};
use sail_calendar::contracts::DeleteScope;
use sail_calendar::service::CalendarService;

#[test]
fn import_preview_then_commit_is_idempotent() {
    let tmp = TempDir::new();
    let service =
        CalendarService::open(&tmp.0, Arc::new(FakeFetcher::new(Vec::new()))).expect("service");
    let preview = service.import_preview(IMPORT_V1, now()).expect("preview");
    assert!(preview.preview_id.starts_with("imp_"));
    assert_eq!(preview.events.len(), 2);
    assert!(preview.events.iter().all(|event| !event.duplicate));
    assert!(preview.skipped.is_empty());

    let result = service
        .import_commit(&preview.preview_id, now() + Duration::minutes(1))
        .expect("commit");
    assert_eq!(
        result,
        sail_calendar::contracts::ImportResult {
            imported: 2,
            duplicates: 0
        }
    );
    let window = service
        .window(
            "2026-11-27T00:00:00Z".parse().unwrap(),
            "2026-11-29T00:00:00Z".parse().unwrap(),
            None,
            false,
        )
        .expect("window");
    assert_eq!(window.occurrences.len(), 2);

    let again = service.import_preview(IMPORT_V1, now()).expect("preview");
    assert!(again.events.iter().all(|event| event.duplicate));
    let result = service
        .import_commit(&again.preview_id, now() + Duration::minutes(1))
        .expect("commit");
    assert_eq!(result.imported, 0);
    assert_eq!(result.duplicates, 2);
}

#[test]
fn expired_or_unknown_previews_are_not_found() {
    let tmp = TempDir::new();
    let service =
        CalendarService::open(&tmp.0, Arc::new(FakeFetcher::new(Vec::new()))).expect("service");
    let preview = service.import_preview(IMPORT_V1, now()).expect("preview");
    assert!(matches!(
        service.import_commit(&preview.preview_id, now() + Duration::minutes(16)),
        Err(sail_calendar::contracts::CalendarError::NotFound)
    ));
    assert!(matches!(
        service.import_commit("imp_missing00000", now()),
        Err(sail_calendar::contracts::CalendarError::NotFound)
    ));
}

#[test]
fn export_round_trips_local_events() {
    let tmp = TempDir::new();
    let service =
        CalendarService::open(&tmp.0, Arc::new(FakeFetcher::new(Vec::new()))).expect("service");
    let preview = service.import_preview(IMPORT_V1, now()).expect("preview");
    service
        .import_commit(&preview.preview_id, now())
        .expect("commit");
    let exported = service.export(None, None, None).expect("export");
    assert!(exported.starts_with("BEGIN:VCALENDAR\r\n"));
    assert!(exported.contains("SUMMARY:Dentist"));
    let parsed = sail_calendar::ics::parse::parse(
        &exported,
        "src_local000000",
        chrono_tz::Europe::Lisbon,
        now(),
    );
    assert_eq!(parsed.events.len(), 2);
}

#[test]
fn deleting_one_occurrence_adds_an_exdate() {
    let tmp = TempDir::new();
    let service =
        CalendarService::open(&tmp.0, Arc::new(FakeFetcher::new(Vec::new()))).expect("service");
    let preview = service.import_preview(IMPORT_V1, now()).expect("preview");
    service
        .import_commit(&preview.preview_id, now())
        .expect("commit");
    let window = service
        .window(
            "2026-11-27T00:00:00Z".parse().unwrap(),
            "2026-11-29T00:00:00Z".parse().unwrap(),
            None,
            false,
        )
        .expect("window");
    let occurrence = &window.occurrences[1];
    let event = service.event(&occurrence.event_id).expect("event");
    // Give the imported event a rule so "this" is legal.
    let updated = service
        .update(
            &event.id,
            sail_calendar::contracts::EventPatch {
                repeat: Some(sail_calendar::contracts::RepeatPreset::Weekly),
                ..Default::default()
            },
            now(),
        )
        .expect("update");
    assert!(updated.rrule.is_some());
    let occurrence_key = window.occurrences[1].key.clone();
    service
        .delete(&event.id, DeleteScope::This, Some(&occurrence_key))
        .expect("delete this");
    let after = service.event(&event.id).expect("event");
    assert_eq!(after.exdates.len(), 1);
}
