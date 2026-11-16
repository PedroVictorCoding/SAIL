//! Presence estimation and the in-memory store.
use chrono::{DateTime, Duration, Utc};
use sail_pulse::presence::estimate::estimate;
use sail_pulse::presence::store::PresenceStore;
use sail_pulse::presence::{
    Heartbeat, Place, PresenceEvent, PresenceSettings, SurfaceKind, SurfaceStatus,
};

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn temp_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "sail-presence-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

fn beat(device_id: &str, surface: SurfaceKind, visible: bool, idle: u32) -> Heartbeat {
    Heartbeat {
        device_id: device_id.into(),
        surface,
        visible,
        input_idle_secs: idle,
        charging: None,
        voice_ready: false,
    }
}

fn surface(
    device_id: &str,
    surface: SurfaceKind,
    visible: bool,
    idle: u32,
    last_seen: DateTime<Utc>,
) -> SurfaceStatus {
    SurfaceStatus {
        device_id: device_id.into(),
        surface,
        visible,
        input_idle_secs: idle,
        last_seen,
        voice_ready: true,
        can_speak: true,
    }
}

#[test]
fn estimate_is_pure_and_picks_the_least_idle_active_surface() {
    let now = at("2026-11-16T18:00:00Z");
    let settings = PresenceSettings::default();
    let surfaces = [
        surface("web-1", SurfaceKind::Web, true, 30, now),
        surface("dock-1", SurfaceKind::Dock, true, 10, now),
        surface("phone-1", SurfaceKind::Phone, true, 5, now),
    ];
    let first = estimate(&surfaces, now, false, &settings);
    let second = estimate(&surfaces, now, false, &settings);
    assert_eq!(first, second);
    assert_eq!(first, (Place::Phone, Some("phone-1".into())));

    // Not visible or idle past the away threshold is not active.
    let hidden = [
        surface("web-1", SurfaceKind::Web, false, 0, now),
        surface("dock-1", SurfaceKind::Dock, true, 600, now),
    ];
    assert_eq!(
        estimate(&hidden, now, false, &settings),
        (Place::Away, None)
    );
}

#[test]
fn estimate_ties_go_web_then_dock_then_phone_then_latest_and_is_order_independent() {
    let now = at("2026-11-16T18:00:00Z");
    let settings = PresenceSettings::default();
    let web = surface("web-1", SurfaceKind::Web, true, 12, now);
    let dock = surface(
        "dock-1",
        SurfaceKind::Dock,
        true,
        12,
        now + Duration::seconds(1),
    );
    let forward = estimate(&[web.clone(), dock.clone()], now, false, &settings);
    let backward = estimate(&[dock, web], now, false, &settings);
    assert_eq!(forward, backward);
    assert_eq!(forward, (Place::Desk, Some("web-1".into())));

    // Same surface kind, same idle: the latest heartbeat wins.
    let a = surface("dock-a", SurfaceKind::Dock, true, 12, now);
    let b = surface(
        "dock-b",
        SurfaceKind::Dock,
        true,
        12,
        now + Duration::seconds(1),
    );
    assert_eq!(
        estimate(&[a, b], now + Duration::seconds(1), false, &settings),
        (Place::Dock, Some("dock-b".into()))
    );
}

#[test]
fn estimate_handles_quiet_unknown_and_stale_surfaces() {
    let now = at("2026-11-16T18:00:00Z");
    let settings = PresenceSettings::default();
    assert_eq!(estimate(&[], now, false, &settings), (Place::Unknown, None));
    assert_eq!(estimate(&[], now, true, &settings), (Place::Unknown, None));

    let stale = [surface(
        "web-1",
        SurfaceKind::Web,
        true,
        0,
        now - Duration::seconds(91),
    )];
    assert_eq!(estimate(&stale, now, false, &settings), (Place::Away, None));
    assert_eq!(
        estimate(&stale, now, true, &settings),
        (Place::Asleep, None)
    );

    let live = [surface(
        "web-1",
        SurfaceKind::Web,
        true,
        0,
        now - Duration::seconds(90),
    )];
    assert_eq!(
        estimate(&live, now, false, &settings),
        (Place::Desk, Some("web-1".into()))
    );
}

#[test]
fn store_fires_arrived_left_and_skips_place_changes_between_surfaces() {
    let dir = temp_dir();
    let store = PresenceStore::open(&dir).unwrap();
    let t0 = at("2026-11-16T18:00:00Z");

    let (state, events) = store
        .record(beat("web-1", SurfaceKind::Web, true, 5), t0, false)
        .unwrap();
    assert_eq!(state.place, Place::Desk);
    assert_eq!(state.active_device.as_deref(), Some("web-1"));
    assert_eq!(state.since, t0);
    assert_eq!(
        events,
        vec![PresenceEvent::Arrived {
            place: Place::Desk,
            device_id: "web-1".into(),
            away_for_secs: 0,
        }]
    );

    let (state, events) = store
        .record(
            beat("dock-1", SurfaceKind::Dock, true, 0),
            t0 + Duration::seconds(5),
            false,
        )
        .unwrap();
    assert_eq!(state.place, Place::Dock);
    assert!(events.is_empty(), "Desk -> Dock is not an event");
    assert_eq!(state.since, t0 + Duration::seconds(5));

    // Nothing heard for longer than STALE_AFTER_SECS: the surface goes stale.
    let left_at = t0 + Duration::seconds(5 + 91);
    let events = store.tick(left_at, false);
    assert_eq!(events, vec![PresenceEvent::Left { from: Place::Dock }]);
    assert!(store.state(left_at, false).surfaces.is_empty());
    assert!(store.tick(left_at + Duration::seconds(1), false).is_empty());

    // Coming back an hour after the estimate noticed the absence.
    let back = left_at + Duration::seconds(3_600);
    let (state, events) = store
        .record(beat("dock-1", SurfaceKind::Dock, true, 0), back, false)
        .unwrap();
    assert_eq!(state.place, Place::Dock);
    assert_eq!(
        events,
        vec![PresenceEvent::Arrived {
            place: Place::Dock,
            device_id: "dock-1".into(),
            away_for_secs: 3_600,
        }]
    );

    // Quiet hours with no active surface is Asleep and fires Left.
    let quiet = back + Duration::seconds(3_601);
    let events = store.tick(quiet, true);
    assert_eq!(events, vec![PresenceEvent::Left { from: Place::Dock }]);
    assert_eq!(store.state(quiet, true).place, Place::Asleep);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn store_rejects_invalid_device_ids_and_clamps_idle_time() {
    let dir = temp_dir();
    let store = PresenceStore::open(&dir).unwrap();
    let now = at("2026-11-16T18:00:00Z");
    for id in ["", "with space", "with/slash", "olá", "é"] {
        assert!(
            store
                .record(beat(id, SurfaceKind::Web, true, 0), now, false)
                .is_err(),
            "{id:?} must be rejected"
        );
    }
    assert!(
        store
            .record(beat(&"a".repeat(65), SurfaceKind::Web, true, 0), now, false)
            .is_err()
    );
    let long = "a".repeat(64);
    let (state, _) = store
        .record(beat(&long, SurfaceKind::Web, true, 100_000), now, false)
        .unwrap();
    assert_eq!(state.surfaces[0].input_idle_secs, 86_400);
    // 86 400 s of idle is past the away threshold, so the desk is not active.
    assert_eq!(state.place, Place::Away);

    let (state, _) = store
        .record(beat("web_1-x", SurfaceKind::Web, true, 0), now, false)
        .unwrap();
    assert_eq!(state.active_device.as_deref(), Some("web_1-x"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn store_keeps_only_the_latest_sixteen_devices() {
    let dir = temp_dir();
    let store = PresenceStore::open(&dir).unwrap();
    let t0 = at("2026-11-16T18:00:00Z");
    for index in 0..17u32 {
        let id = format!("dev-{index:02}");
        store
            .record(
                beat(&id, SurfaceKind::Web, true, 0),
                t0 + Duration::seconds(i64::from(index)),
                false,
            )
            .unwrap();
    }
    let state = store.state(t0 + Duration::seconds(17), false);
    assert_eq!(state.surfaces.len(), 16);
    assert!(!state.surfaces.iter().any(|s| s.device_id == "dev-00"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn note_addressed_turn_resets_idle_and_can_make_the_device_active() {
    let dir = temp_dir();
    let store = PresenceStore::open(&dir).unwrap();
    let t0 = at("2026-11-16T18:00:00Z");
    store
        .record(beat("dock-1", SurfaceKind::Dock, true, 700), t0, false)
        .unwrap();
    assert_eq!(store.state(t0, false).place, Place::Away);

    let events = store.note_addressed_turn("dock-1", t0 + Duration::seconds(1), false);
    assert_eq!(
        events,
        vec![PresenceEvent::Arrived {
            place: Place::Dock,
            device_id: "dock-1".into(),
            away_for_secs: 0,
        }]
    );
    let state = store.state(t0 + Duration::seconds(1), false);
    assert_eq!(state.surfaces[0].input_idle_secs, 0);
    assert_eq!(state.place, Place::Dock);

    // An unknown device is a no-op, not a panic.
    assert!(
        store
            .note_addressed_turn("nope", t0 + Duration::seconds(2), false)
            .is_empty()
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn state_recomputes_can_speak_from_current_settings_and_hides_stale_surfaces() {
    let dir = temp_dir();
    let store = PresenceStore::open(&dir).unwrap();
    let t0 = at("2026-11-16T18:00:00Z");
    let mut voice = beat("web-1", SurfaceKind::Web, true, 0);
    voice.voice_ready = true;
    store.record(voice, t0, false).unwrap();
    let state = store.state(t0, false);
    assert!(state.surfaces[0].can_speak);

    let settings = PresenceSettings {
        speak_on: vec![SurfaceKind::Dock],
        ..PresenceSettings::default()
    };
    store.save_settings(settings).unwrap();
    let state = store.state(t0, false);
    assert!(
        !state.surfaces[0].can_speak,
        "Web is no longer a speak surface"
    );

    let stale = store.state(t0 + Duration::seconds(91), false);
    assert!(stale.surfaces.is_empty());
    assert_eq!(stale.place, Place::Away);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn settings_persist_clamp_and_survive_corruption() {
    let dir = temp_dir();
    let t0 = at("2026-11-16T18:00:00Z");
    {
        let store = PresenceStore::open(&dir).unwrap();
        assert_eq!(store.settings(), PresenceSettings::default());
        let saved = store
            .save_settings(PresenceSettings {
                away_after_secs: 0,
                greet_after_away_secs: 1,
                max_spoken_per_hour: 9,
                telegram_when_away: true,
                ..PresenceSettings::default()
            })
            .unwrap();
        assert_eq!(saved.away_after_secs, 60);
        assert_eq!(saved.greet_after_away_secs, 300);
        assert_eq!(saved.max_spoken_per_hour, 6);
        let saved = store
            .save_settings(PresenceSettings {
                away_after_secs: u64::MAX,
                greet_after_away_secs: u64::MAX,
                max_spoken_per_hour: 9,
                telegram_when_away: true,
                ..PresenceSettings::default()
            })
            .unwrap();
        assert_eq!(saved.away_after_secs, 7_200);
        assert_eq!(saved.greet_after_away_secs, 86_400);
        assert_eq!(saved.max_spoken_per_hour, 6);
        store
            .record(beat("web-1", SurfaceKind::Web, true, 0), t0, false)
            .unwrap();
    }
    {
        let reopened = PresenceStore::open(&dir).unwrap();
        let settings = reopened.settings();
        assert_eq!(settings.max_spoken_per_hour, 6);
        assert!(settings.telegram_when_away);
        // Heartbeats are memory-only: a restart has no surfaces.
        assert_eq!(reopened.state(t0, false).place, Place::Unknown);
    }

    std::fs::write(dir.join("presence.json"), b"{ not json").unwrap();
    let recovered = PresenceStore::open(&dir).unwrap();
    assert_eq!(recovered.settings(), PresenceSettings::default());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn absence_survives_quiet_hours_across_a_whole_night() {
    let dir = temp_dir();
    let store = PresenceStore::open(&dir).unwrap();
    let evening = at("2026-11-16T19:00:00Z");
    store
        .record(beat("web-1", SurfaceKind::Web, true, 0), evening, false)
        .unwrap();

    let left = at("2026-11-16T20:00:00Z");
    let (state, events) = store
        .record(beat("web-1", SurfaceKind::Web, false, 0), left, false)
        .unwrap();
    assert_eq!(state.place, Place::Away);
    assert_eq!(events, vec![PresenceEvent::Left { from: Place::Desk }]);

    // Quiet hours start and end: the place changes, but the absence clock does not.
    assert!(store.tick(at("2026-11-16T23:00:00Z"), true).is_empty());
    assert_eq!(
        store.state(at("2026-11-16T23:00:00Z"), true).place,
        Place::Asleep
    );
    assert!(store.tick(at("2026-11-17T07:00:00Z"), false).is_empty());
    assert_eq!(
        store.state(at("2026-11-17T07:00:00Z"), false).place,
        Place::Away
    );

    let morning = at("2026-11-17T07:20:00Z");
    let (state, events) = store
        .record(beat("web-1", SurfaceKind::Web, true, 0), morning, false)
        .unwrap();
    assert_eq!(state.place, Place::Desk);
    assert_eq!(
        events,
        vec![PresenceEvent::Arrived {
            place: Place::Desk,
            device_id: "web-1".into(),
            away_for_secs: 11 * 3_600 + 20 * 60,
        }]
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn no_left_before_any_heartbeat_or_between_away_and_asleep() {
    let dir = temp_dir();
    let store = PresenceStore::open(&dir).unwrap();
    let now = at("2026-11-16T18:00:00Z");

    // Nothing has ever been seen.
    assert!(store.tick(now, false).is_empty());
    assert_eq!(store.state(now, false).place, Place::Unknown);

    // Unknown -> Away is not a departure.
    let (state, events) = store
        .record(beat("web-1", SurfaceKind::Web, false, 0), now, false)
        .unwrap();
    assert_eq!(state.place, Place::Away);
    assert!(events.is_empty());

    // Away -> Asleep and Asleep -> Away are quiet-hour changes, not departures.
    assert!(store.tick(now, true).is_empty());
    assert_eq!(store.state(now, true).place, Place::Asleep);
    assert!(store.tick(now, false).is_empty());
    assert_eq!(store.state(now, false).place, Place::Away);

    // With no observed departure there is nothing to measure, so the arrival is 0 s.
    let (_, events) = store
        .record(
            beat("web-1", SurfaceKind::Web, true, 0),
            now + Duration::seconds(1),
            false,
        )
        .unwrap();
    assert_eq!(
        events,
        vec![PresenceEvent::Arrived {
            place: Place::Desk,
            device_id: "web-1".into(),
            away_for_secs: 0,
        }]
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn store_publishes_events_from_record_note_and_tick() {
    let dir = temp_dir();
    let store = PresenceStore::open(&dir).unwrap();
    let mut events = store.subscribe();
    let t0 = at("2026-11-16T18:00:00Z");

    store
        .record(beat("web-1", SurfaceKind::Web, true, 0), t0, false)
        .unwrap();
    assert_eq!(
        events.try_recv().unwrap(),
        PresenceEvent::Arrived {
            place: Place::Desk,
            device_id: "web-1".into(),
            away_for_secs: 0,
        }
    );

    store
        .record(
            beat("web-1", SurfaceKind::Web, true, 700),
            t0 + Duration::seconds(1),
            false,
        )
        .unwrap();
    assert_eq!(
        events.try_recv().unwrap(),
        PresenceEvent::Left { from: Place::Desk }
    );

    store.note_addressed_turn("web-1", t0 + Duration::seconds(2), false);
    assert_eq!(
        events.try_recv().unwrap(),
        PresenceEvent::Arrived {
            place: Place::Desk,
            device_id: "web-1".into(),
            away_for_secs: 1,
        }
    );

    store.tick(t0 + Duration::seconds(2 + 91), false);
    assert_eq!(
        events.try_recv().unwrap(),
        PresenceEvent::Left { from: Place::Desk }
    );

    std::fs::remove_dir_all(&dir).ok();
}
