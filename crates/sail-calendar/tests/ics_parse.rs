//! K1: ICS parsing, time zones, folding and export.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::Europe::Lisbon;
use sail_calendar::contracts::{EventStatus, EventTime};
use sail_calendar::ics::parse::parse;
use sail_calendar::ics::write::write;

const GOOGLE: &str = include_str!("fixtures/google.ics");
const OUTLOOK: &str = include_str!("fixtures/outlook_windows.ics");
const MOODLE: &str = include_str!("fixtures/moodle.ics");
const HOSTILE: &str = include_str!("fixtures/hostile.ics");
const RECURRING: &str = include_str!("fixtures/recurring.ics");

fn now() -> DateTime<Utc> {
    "2026-11-20T10:00:00Z".parse().expect("fixed time")
}

#[test]
fn google_export_parses_timed_all_day_and_tentative_events() {
    let parsed = parse(GOOGLE, "src_aaaaaaaaaaaa", Lisbon, now());
    assert_eq!(parsed.events.len(), 3);
    assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    assert!(!parsed.truncated);

    let lecture = parsed
        .events
        .iter()
        .find(|event| event.uid == "algorithms@example.test")
        .expect("lecture");
    assert_eq!(lecture.title, "Algorithms lecture");
    assert_eq!(lecture.location.as_deref(), Some("ISCTE - Building II"));
    assert_eq!(
        lecture.notes.as_deref(),
        Some("Chapter 4\nBring the exercise sheet.")
    );
    match &lecture.time {
        EventTime::Timed { start, end, tz } => {
            assert_eq!(*start, Utc.with_ymd_and_hms(2026, 11, 24, 9, 0, 0).unwrap());
            assert_eq!(*end, Utc.with_ymd_and_hms(2026, 11, 24, 10, 30, 0).unwrap());
            assert_eq!(tz, "Europe/Lisbon");
        }
        other => panic!("expected timed, got {other:?}"),
    }
    assert!(lecture.id.starts_with("fe_"));
    assert!(!lecture.writable);

    let dentist = parsed
        .events
        .iter()
        .find(|event| event.uid == "dentist@example.test")
        .expect("dentist");
    match &dentist.time {
        EventTime::AllDay {
            start_date,
            end_date,
        } => {
            assert_eq!(*start_date, NaiveDate::from_ymd_opt(2026, 11, 25).unwrap());
            assert_eq!(*end_date, NaiveDate::from_ymd_opt(2026, 11, 26).unwrap());
        }
        other => panic!("expected all-day, got {other:?}"),
    }

    let lab = parsed
        .events
        .iter()
        .find(|event| event.uid == "lab@example.test")
        .expect("lab");
    assert_eq!(lab.status, EventStatus::Tentative);
}

#[test]
fn outlook_windows_zone_names_and_folding_are_resolved() {
    let parsed = parse(OUTLOOK, "src_bbbbbbbbbbbb", Lisbon, now());
    assert_eq!(parsed.events.len(), 2);
    let london = parsed
        .events
        .iter()
        .find(|event| event.uid == "london-review@example.test")
        .expect("london");
    assert!(london.title.contains("folded across two physical lines"));
    match &london.time {
        EventTime::Timed { start, end, .. } => {
            assert_eq!(*start, Utc.with_ymd_and_hms(2026, 12, 1, 14, 0, 0).unwrap());
            assert_eq!(*end, Utc.with_ymd_and_hms(2026, 12, 1, 15, 0, 0).unwrap());
        }
        other => panic!("expected timed, got {other:?}"),
    }
    let berlin = parsed
        .events
        .iter()
        .find(|event| event.uid == "berlin-call@example.test")
        .expect("berlin");
    match &berlin.time {
        EventTime::Timed { start, end, .. } => {
            assert_eq!(*start, Utc.with_ymd_and_hms(2026, 12, 2, 8, 0, 0).unwrap());
            assert_eq!(*end, Utc.with_ymd_and_hms(2026, 12, 2, 9, 0, 0).unwrap());
        }
        other => panic!("expected timed, got {other:?}"),
    }
}

#[test]
fn moodle_events_default_to_one_hour_and_keep_status() {
    let parsed = parse(MOODLE, "src_cccccccccccc", Lisbon, now());
    assert_eq!(parsed.events.len(), 2);
    let assignment = parsed
        .events
        .iter()
        .find(|event| event.uid == "assignment-335@example.test")
        .expect("assignment");
    match &assignment.time {
        EventTime::Timed { start, end, .. } => {
            assert_eq!(
                *start,
                Utc.with_ymd_and_hms(2026, 11, 30, 23, 59, 0).unwrap()
            );
            assert_eq!(*end, *start + chrono::Duration::hours(1));
        }
        other => panic!("expected timed, got {other:?}"),
    }
    let quiz = parsed
        .events
        .iter()
        .find(|event| event.uid == "quiz-4@example.test")
        .expect("quiz");
    assert_eq!(quiz.status, EventStatus::Tentative);
}

#[test]
fn unknown_zone_uses_the_owner_zone_with_a_warning() {
    let parsed = parse(HOSTILE, "src_dddddddddddd", Lisbon, now());
    assert_eq!(parsed.events.len(), 1);
    assert!(
        parsed
            .warnings
            .iter()
            .any(|warning| warning.contains("unknown TZID"))
    );
    assert!(
        parsed
            .warnings
            .iter()
            .any(|warning| warning.contains("without UID"))
    );
    assert!(
        parsed
            .warnings
            .iter()
            .any(|warning| warning.contains("DTEND is before DTSTART"))
    );
    assert!(
        parsed
            .warnings
            .iter()
            .any(|warning| warning.contains("ended inside"))
    );
    match &parsed.events[0].time {
        EventTime::Timed { start, .. } => {
            assert_eq!(*start, Utc.with_ymd_and_hms(2026, 12, 6, 9, 0, 0).unwrap());
        }
        other => panic!("expected timed, got {other:?}"),
    }
}

#[test]
fn a_dst_boundary_keeps_the_same_local_hour() {
    let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:before-dst@example.test\nDTSTART;TZID=Europe/Lisbon:20261024T090000\nDTEND;TZID=Europe/Lisbon:20261024T100000\nSUMMARY:Before DST\nEND:VEVENT\nBEGIN:VEVENT\nUID:after-dst@example.test\nDTSTART;TZID=Europe/Lisbon:20261025T090000\nDTEND;TZID=Europe/Lisbon:20261025T100000\nSUMMARY:After DST\nEND:VEVENT\nEND:VCALENDAR\n";
    let parsed = parse(ics, "src_eeeeeeeeeeee", Lisbon, now());
    let starts: Vec<DateTime<Utc>> = parsed
        .events
        .iter()
        .map(|event| match &event.time {
            EventTime::Timed { start, .. } => *start,
            _ => panic!("expected timed"),
        })
        .collect();
    assert!(starts.contains(&Utc.with_ymd_and_hms(2026, 10, 24, 8, 0, 0).unwrap()));
    assert!(starts.contains(&Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap()));
}

#[test]
fn twenty_thousand_events_is_the_cap() {
    let mut text = String::from("BEGIN:VCALENDAR\n");
    for number in 0..20_001 {
        text.push_str(&format!(
            "BEGIN:VEVENT\nUID:bulk-{number}@example.test\nDTSTART:20261201T090000Z\nSUMMARY:Bulk {number}\nEND:VEVENT\n"
        ));
    }
    text.push_str("END:VCALENDAR\n");
    let parsed = parse(&text, "src_ffffffffffff", Lisbon, now());
    assert_eq!(parsed.events.len(), 20_000);
    assert!(parsed.truncated);
}

#[test]
fn export_is_crlf_folded_at_75_octets_and_round_trips() {
    let parsed = parse(GOOGLE, "src_aaaaaaaaaaaa", Lisbon, now());
    let mut events = parsed.events;
    events[0].notes = Some("é".repeat(400));
    let exported = write(&events, "SAIL-Test");
    assert!(exported.contains("\r\n"));
    assert!(!exported.replace("\r\n", "").contains('\n'));
    for line in exported.split("\r\n") {
        assert!(line.len() <= 75, "line too long: {line:?}");
    }
    let reparsed = parse(&exported, "src_aaaaaaaaaaaa", Lisbon, now());
    assert_eq!(reparsed.events.len(), events.len());
    assert_eq!(reparsed.events[0].uid, events[0].uid);
    assert_eq!(reparsed.events[0].title, events[0].title);
    assert_eq!(reparsed.events[0].notes, events[0].notes);
}

#[test]
fn parsing_and_writing_are_deterministic() {
    let first = parse(RECURRING, "src_gggggggggggg", Lisbon, now());
    let second = parse(RECURRING, "src_gggggggggggg", Lisbon, now());
    assert_eq!(first, second);
    assert_eq!(write(&first.events, "P"), write(&second.events, "P"));
}
