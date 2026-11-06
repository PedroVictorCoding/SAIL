use chrono::{DateTime, Duration, Utc};
use sail_pulse::{
    NewReminder, Pulse, PulseError, PulseSettings, ReminderStatus, default_path,
};

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn reminder(text: &str, due: DateTime<Utc>) -> NewReminder {
    NewReminder::new(text, due, "America/Sao_Paulo", -180).unwrap()
}

fn temp_path() -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("sail-pulse-{}", uuid::Uuid::new_v4().simple()));
    (dir.join("pulse.db"), dir)
}

#[test]
fn reminder_lifecycle_is_durable_and_fires_once() {
    let (path, dir) = temp_path();
    let now = at("2026-11-04T12:00:00Z");
    let id = {
        let pulse = Pulse::open(&path).unwrap();
        assert!(pulse.is_durable());
        let created = pulse
            .create_at(reminder("stretch", now + Duration::hours(1)), now)
            .unwrap();
        assert_eq!(created.status, ReminderStatus::Pending);
        assert_eq!(created.snooze_count, 0);
        created.id
    };

    {
        let pulse = Pulse::open(&path).unwrap();
        let reopened = pulse.get(&id).unwrap();
        assert_eq!(reopened.status, ReminderStatus::Pending);
        assert_eq!(reopened.text, "stretch");
        assert_eq!(reopened.due_at, now + Duration::hours(1));

        let fired = pulse.fire_due_at(now + Duration::hours(2)).unwrap();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].id, id);
        assert_eq!(fired[0].status, ReminderStatus::Due);
        assert!(fired[0].fired_at.is_some());
        assert!(
            pulse
                .fire_due_at(now + Duration::hours(3))
                .unwrap()
                .is_empty(),
            "a durable due reminder is not fired again"
        );
    }

    {
        let pulse = Pulse::open(&path).unwrap();
        let due = pulse.get(&id).unwrap();
        assert_eq!(
            due.status,
            ReminderStatus::Due,
            "a missed reminder survives restart"
        );
        let delivered = pulse.acknowledge(&id).unwrap();
        assert_eq!(delivered.status, ReminderStatus::Delivered);
        assert!(delivered.delivered_at.is_some());
        assert_eq!(
            pulse.acknowledge(&id).unwrap().status,
            ReminderStatus::Delivered
        );
        assert!(pulse.list_active(10).unwrap().is_empty());
        assert_eq!(pulse.list_history(10).unwrap()[0].id, id);

        let fire_at = pulse.get(&id).unwrap().delivered_at.unwrap() + Duration::hours(1);
        assert!(pulse.fire_due_at(fire_at).unwrap().is_empty());

        let until = Utc::now() + Duration::minutes(30);
        let snoozed = pulse.snooze(&id, until).unwrap();
        assert_eq!(snoozed.status, ReminderStatus::Pending);
        assert!(
            (snoozed.due_at - until)
                .num_microseconds()
                .is_some_and(|delta| delta.abs() <= 1),
            "stored time keeps microsecond precision"
        );
        assert_eq!(snoozed.snooze_count, 1);
        assert!(snoozed.fired_at.is_none() && snoozed.delivered_at.is_none());
        let refired = pulse.fire_due_at(until + Duration::minutes(1)).unwrap();
        assert_eq!(refired.len(), 1);
        assert_eq!(refired[0].snooze_count, 1);

        let cancelled = pulse.cancel(&id).unwrap();
        assert_eq!(cancelled.status, ReminderStatus::Cancelled);
        assert!(cancelled.cancelled_at.is_some());
    }

    let pulse = Pulse::open(&path).unwrap();
    assert_eq!(pulse.get(&id).unwrap().status, ReminderStatus::Cancelled);
    assert!(pulse.list_active(10).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn quiet_hours_defer_delivery_until_the_window_ends() {
    let (path, dir) = temp_path();
    let pulse = Pulse::open(&path).unwrap();
    let mut settings = PulseSettings {
        quiet_hours_enabled: true,
        quiet_start_minute: 22 * 60,
        quiet_end_minute: 7 * 60,
        timezone: "America/Sao_Paulo".into(),
        utc_offset_minutes: -180,
        ..PulseSettings::default()
    };
    pulse.update_settings(&settings).unwrap();

    let created_at = at("2026-11-04T22:30:00-03:00");
    let due = at("2026-11-04T23:00:00-03:00");
    let id = pulse
        .create_at(reminder("sleep", due), created_at)
        .unwrap()
        .id;

    assert!(
        pulse
            .fire_due_at(at("2026-11-04T22:45:00-03:00"))
            .unwrap()
            .is_empty(),
        "quiet hours hold the reminder"
    );
    assert_eq!(pulse.get(&id).unwrap().status, ReminderStatus::Pending);

    let fired = pulse
        .fire_due_at(at("2026-11-05T07:05:00-03:00"))
        .unwrap();
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].id, id);

    settings.quiet_hours_enabled = false;
    pulse.update_settings(&settings).unwrap();
    let reopened = Pulse::open(&path).unwrap();
    assert!(!reopened.settings().unwrap().quiet_hours_enabled);
    assert_eq!(reopened.settings().unwrap().timezone, "America/Sao_Paulo");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn invalid_inputs_are_rejected() {
    assert!(NewReminder::new("", Utc::now(), "UTC", 0).is_err());
    assert!(NewReminder::new("x", Utc::now(), "", 0).is_err());
    assert!(NewReminder::new("x", Utc::now(), "not a zone", 0).is_err());
    assert!(NewReminder::new("x", Utc::now(), "UTC", 900).is_err());

    let pulse = Pulse::open_in_memory().unwrap();
    let now = Utc::now();
    assert!(matches!(
        pulse.create_at(reminder("past", now), now),
        Err(PulseError::Invalid(_))
    ));
    let id = pulse
        .create_at(reminder("future", now + Duration::minutes(5)), now)
        .unwrap()
        .id;
    assert!(matches!(
        pulse.snooze(&id, now),
        Err(PulseError::Invalid(_))
    ));
    assert!(matches!(
        pulse.acknowledge(&id),
        Err(PulseError::InvalidState { .. })
    ));
    assert!(matches!(
        pulse.get("missing"),
        Err(PulseError::NotFound(_))
    ));
    assert!(pulse.cancel(&id).is_ok());
    assert!(matches!(
        pulse.snooze(&id, now + Duration::minutes(10)),
        Err(PulseError::InvalidState { .. })
    ));
}

#[test]
fn quiet_window_wraps_midnight_and_defaults_are_safe() {
    let settings = PulseSettings {
        quiet_hours_enabled: true,
        quiet_start_minute: 22 * 60,
        quiet_end_minute: 7 * 60,
        utc_offset_minutes: 0,
        ..PulseSettings::default()
    };
    assert!(settings.is_quiet_at(at("2026-11-04T23:00:00Z")));
    assert!(settings.is_quiet_at(at("2026-11-04T02:00:00Z")));
    assert!(!settings.is_quiet_at(at("2026-11-04T12:00:00Z")));
    assert!(!PulseSettings::default().spoken_reminders);
    assert!(PulseSettings::default().in_app_notifications);
    assert!(default_path().ends_with("pulse/pulse.db"));
}
