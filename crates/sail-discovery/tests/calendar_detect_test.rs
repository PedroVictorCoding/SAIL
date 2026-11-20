//! K6: the calendar preparation and feed-change observations.

use chrono::{DateTime, Duration, Utc};
use sail_discovery::detect::{CalendarChangeKind, Observation, detect, detect_in_tz};
use sail_pulse::attention::{Provenance, Source};

fn now() -> DateTime<Utc> {
    "2026-11-20T10:00:00Z".parse().expect("fixed time")
}

fn event(start: DateTime<Utc>, prepare: bool) -> Observation {
    Observation::CalendarEvent {
        id: "loc_1@2026-11-21T09:00:00+00:00".into(),
        title: "Algorithms lecture".into(),
        start,
        all_day: false,
        prepare,
        location: Some("ISCTE".into()),
    }
}

#[test]
fn prepared_events_within_48_hours_become_commitments() {
    let start = now() + Duration::hours(20);
    let signals = detect(&event(start, true), now());
    assert_eq!(signals.len(), 1);
    let signal = &signals[0];
    assert_eq!(signal.source, Source::Calendar);
    assert_eq!(signal.kind, "calendar_prep");
    assert_eq!(
        signal.dedupe_key,
        "cal-prep:loc_1@2026-11-21T09:00:00+00:00"
    );
    assert_eq!(signal.title, "Prepare: Algorithms lecture");
    assert_eq!(signal.deadline, Some(start));
    assert_eq!(signal.provenance, Provenance::UserCommitment);
    assert_eq!(signal.link.as_deref(), Some("/calendar"));
    assert!(signal.urgency > 0.3);

    let soon = detect(&event(now() + Duration::hours(2), true), now());
    assert!(soon[0].urgency > signal.urgency);
}

#[test]
fn plain_or_out_of_horizon_events_are_silent() {
    assert!(detect(&event(now() + Duration::hours(20), false), now()).is_empty());
    assert!(
        detect(
            &event(now() + Duration::hours(48) + Duration::minutes(1), true),
            now()
        )
        .is_empty()
    );
    assert!(detect(&event(now() + Duration::hours(49), true), now()).is_empty());
    assert!(detect(&event(now() - Duration::minutes(1), true), now()).is_empty());
}

#[test]
fn moved_and_cancelled_feeds_use_the_owner_zone_wording() {
    let new_start: DateTime<Utc> = "2026-11-25T14:00:00Z".parse().expect("time");
    let moved = Observation::CalendarChange {
        id: "fe_1".into(),
        title: "Physics lab".into(),
        kind: CalendarChangeKind::Moved,
        new_start: Some(new_start),
        observed_at: now() - Duration::hours(2),
    };
    let signals = detect_in_tz(&moved, now(), chrono_tz::Europe::Lisbon);
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].kind, "calendar_moved");
    assert_eq!(signals[0].title, "Moved: Physics lab → Wed 14:00");
    assert_eq!(
        signals[0].dedupe_key,
        "cal-chg:fe_1:2026-11-25T14:00:00+00:00"
    );
    assert_eq!(signals[0].source, Source::Calendar);

    let cancelled = Observation::CalendarChange {
        id: "fe_2".into(),
        title: "Physics lab".into(),
        kind: CalendarChangeKind::Cancelled,
        new_start: None,
        observed_at: now() - Duration::hours(1),
    };
    let signals = detect_in_tz(&cancelled, now(), chrono_tz::Europe::Lisbon);
    assert_eq!(signals[0].title, "Cancelled: Physics lab");
    assert_eq!(signals[0].dedupe_key, "cal-chg:fe_2:x");
    assert_eq!(signals[0].deadline, None);

    let stale = Observation::CalendarChange {
        id: "fe_3".into(),
        title: "Old".into(),
        kind: CalendarChangeKind::Moved,
        new_start: Some(new_start),
        observed_at: now() - Duration::days(8),
    };
    assert!(detect(&stale, now()).is_empty());
}

#[test]
fn calendar_detection_is_deterministic() {
    let observation = event(now() + Duration::hours(6), true);
    assert_eq!(detect(&observation, now()), detect(&observation, now()));
    assert_eq!(
        detect_in_tz(&observation, now(), chrono_tz::Europe::Lisbon),
        detect(&observation, now())
    );
}
