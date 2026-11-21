//! K2: recurrence expansion, overrides, EXDATE, bounds and presets.

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Europe::Lisbon;
use sail_calendar::contracts::{Event, EventStatus, EventTime, RepeatPreset};
use sail_calendar::ics::parse::parse;
use sail_calendar::recur::{expand, preset_rrule};

const RECURRING: &str = include_str!("fixtures/recurring.ics");

fn now() -> DateTime<Utc> {
    "2026-11-20T10:00:00Z".parse().expect("fixed time")
}

fn event(id: &str, time: EventTime, rule: Option<&str>) -> Event {
    Event {
        id: id.into(),
        source_id: "src_aaaaaaaaaaaa".into(),
        uid: format!("{id}@example.test"),
        recurrence_id: None,
        title: id.into(),
        time,
        location: None,
        notes: None,
        status: EventStatus::Confirmed,
        rrule: rule.map(str::to_string),
        exdates: Vec::new(),
        repeat: RepeatPreset::None,
        repeat_until: None,
        prepare: false,
        writable: false,
        updated_at: now(),
    }
}

#[test]
fn weekly_series_exdate_override_and_cancelled_override() {
    let parsed = parse(RECURRING, "src_aaaaaaaaaaaa", Lisbon, now());
    let start = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2026, 12, 31, 0, 0, 0).unwrap();
    let (visible, truncated) = expand(&parsed.events, start, end, Lisbon, false);
    assert!(!truncated);
    let titles: Vec<&str> = visible.iter().map(|o| o.title.as_str()).collect();
    assert_eq!(
        titles,
        vec![
            "Weekly lab",
            "Weekly lab",
            "Weekly lab (moved)",
            "Weekly lab"
        ]
    );
    assert!(visible[2].time_overlaps_moved());
    assert_eq!(visible[0].event_id, parsed.events[0].id);
    assert!(visible[0].recurring);
    assert_eq!(
        visible[2].key,
        format!("{}@2026-11-23T09:00:00+00:00", parsed.events[0].id)
    );
    let starts: Vec<DateTime<Utc>> = visible
        .iter()
        .map(|o| match o.time {
            EventTime::Timed { start, .. } => start,
            _ => panic!("expected timed"),
        })
        .collect();
    assert!(!starts.contains(&Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()));
    assert!(!starts.contains(&Utc.with_ymd_and_hms(2026, 11, 30, 9, 0, 0).unwrap()));

    let (with_cancelled, _) = expand(&parsed.events, start, end, Lisbon, true);
    assert_eq!(with_cancelled.len(), 5);
    assert!(
        with_cancelled
            .iter()
            .any(|o| o.status == EventStatus::Cancelled)
    );
}

trait MovedAssert {
    fn time_overlaps_moved(&self) -> bool;
}

impl MovedAssert for sail_calendar::contracts::Occurrence {
    fn time_overlaps_moved(&self) -> bool {
        matches!(
            &self.time,
            EventTime::Timed { start, .. }
                if *start == Utc.with_ymd_and_hms(2026, 11, 24, 14, 0, 0).unwrap()
        )
    }
}

#[test]
fn weekly_local_hour_survives_both_dst_boundaries() {
    let mut weekly = event(
        "loc_dst",
        EventTime::Timed {
            start: Utc.with_ymd_and_hms(2026, 10, 18, 8, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 10, 18, 9, 0, 0).unwrap(),
            tz: "Europe/Lisbon".into(),
        },
        Some("FREQ=WEEKLY;COUNT=25"),
    );
    weekly.writable = true;
    let start = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2027, 4, 1, 0, 0, 0).unwrap();
    let (occurrences, truncated) = expand(&[weekly], start, end, Lisbon, false);
    assert!(!truncated);
    assert!(occurrences.len() >= 20);
    for occurrence in occurrences {
        let EventTime::Timed { start, .. } = occurrence.time else {
            panic!("expected timed")
        };
        let local = start.with_timezone(&Lisbon);
        assert_eq!(local.hour(), 9);
        assert_eq!(local.minute(), 0);
    }
    let dates: Vec<NaiveDate> = ["2026-10-25", "2027-03-28"]
        .iter()
        .map(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap())
        .collect();
    assert!(
        dates
            .iter()
            .all(|d| d.weekday().num_days_from_monday() == 6)
    );
}

#[test]
fn all_day_series_keys_are_dates_and_end_is_exclusive() {
    let start_date = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
    let mut all_day = event(
        "loc_allday",
        EventTime::AllDay {
            start_date,
            end_date: start_date + chrono::Duration::days(1),
        },
        Some("FREQ=WEEKLY;COUNT=3"),
    );
    all_day.writable = true;
    let (occurrences, _) = expand(
        &[all_day],
        Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 11, 30, 0, 0, 0).unwrap(),
        Lisbon,
        false,
    );
    assert_eq!(occurrences.len(), 3);
    assert!(occurrences[0].key.ends_with("@2026-11-02"));
    match occurrences[0].time {
        EventTime::AllDay {
            start_date,
            end_date,
        } => {
            assert_eq!(start_date, NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
            assert_eq!(end_date, NaiveDate::from_ymd_opt(2026, 11, 3).unwrap());
        }
        _ => panic!("expected all-day"),
    }
}

#[test]
fn secondly_rule_is_clipped_to_the_window_and_cap() {
    let event = event(
        "loc_seconds",
        EventTime::Timed {
            start: Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 1).unwrap(),
            tz: "UTC".into(),
        },
        Some("FREQ=SECONDLY"),
    );
    let (one_hour, truncated) = expand(
        std::slice::from_ref(&event),
        Utc.with_ymd_and_hms(2026, 11, 20, 9, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 11, 20, 10, 0, 0).unwrap(),
        Lisbon,
        false,
    );
    assert_eq!(one_hour.len(), 3_600);
    assert!(!truncated);

    let (two_hours, truncated) = expand(
        &[event],
        Utc.with_ymd_and_hms(2026, 11, 20, 9, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 11, 20, 11, 0, 0).unwrap(),
        Lisbon,
        false,
    );
    assert_eq!(two_hours.len(), 4_999);
    assert!(truncated);
}

#[test]
fn non_recurring_all_day_overlap_is_inclusive_of_its_first_day() {
    let event = event(
        "loc_span",
        EventTime::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 11, 23).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 11, 26).unwrap(),
        },
        None,
    );
    let (occurrences, _) = expand(
        &[event],
        Utc.with_ymd_and_hms(2026, 11, 25, 12, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 11, 26, 0, 0, 0).unwrap(),
        Lisbon,
        false,
    );
    assert_eq!(occurrences.len(), 1);
}

#[test]
fn presets_render_the_contract_rules() {
    let timed = EventTime::Timed {
        start: Utc.with_ymd_and_hms(2026, 11, 24, 15, 0, 0).unwrap(),
        end: Utc.with_ymd_and_hms(2026, 11, 24, 16, 0, 0).unwrap(),
        tz: "Europe/Lisbon".into(),
    };
    assert_eq!(preset_rrule(RepeatPreset::None, None, &timed), None);
    assert_eq!(
        preset_rrule(RepeatPreset::Daily, None, &timed).as_deref(),
        Some("FREQ=DAILY")
    );
    assert_eq!(
        preset_rrule(RepeatPreset::Weekdays, None, &timed).as_deref(),
        Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR")
    );
    assert_eq!(
        preset_rrule(RepeatPreset::Weekly, None, &timed).as_deref(),
        Some("FREQ=WEEKLY;BYDAY=TU")
    );
    assert_eq!(
        preset_rrule(RepeatPreset::Monthly, None, &timed).as_deref(),
        Some("FREQ=MONTHLY")
    );
    assert_eq!(
        preset_rrule(RepeatPreset::Yearly, None, &timed).as_deref(),
        Some("FREQ=YEARLY")
    );
    let until = NaiveDate::from_ymd_opt(2027, 1, 31).unwrap();
    assert_eq!(
        preset_rrule(RepeatPreset::Weekly, Some(until), &timed).as_deref(),
        Some("FREQ=WEEKLY;BYDAY=TU;UNTIL=20270131")
    );
}

#[test]
fn expansion_is_deterministic() {
    let parsed = parse(RECURRING, "src_aaaaaaaaaaaa", Lisbon, now());
    let start = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2026, 12, 31, 0, 0, 0).unwrap();
    let first = expand(&parsed.events, start, end, Lisbon, true);
    let second = expand(&parsed.events, start, end, Lisbon, true);
    assert_eq!(first, second);
}
