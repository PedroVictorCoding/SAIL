//! Routing an interruption to exactly one surface.
use chrono::{DateTime, Duration, Utc};
use sail_pulse::attention::route::{Route, RouteContext, route};
use sail_pulse::attention::{
    Assessment, AttentionItem, Delivery, ItemState, Provenance, Signal, Source,
};
use sail_pulse::presence::{Place, PresenceSettings, PresenceState, SurfaceKind, SurfaceStatus};

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn signal(deadline: Option<DateTime<Utc>>, urgency: f32) -> Signal {
    Signal {
        source: Source::Projects,
        kind: "task_failed".into(),
        dedupe_key: "task:42:task_failed".into(),
        title: "Forge: refactor failed".into(),
        body: "2 tests failed.".into(),
        urgency,
        relevance: 0.8,
        novelty: 1.0,
        confidence: 1.0,
        deadline,
        provenance: Provenance::Observation,
        link: None,
        observed_at: at("2026-11-16T18:00:00Z"),
    }
}

fn item(delivery: Delivery, deadline: Option<DateTime<Utc>>, urgency: f32) -> AttentionItem {
    let created_at = at("2026-11-16T18:00:00Z");
    AttentionItem {
        id: "a1b2".into(),
        signal: signal(deadline, urgency),
        assessment: Assessment {
            score: 0.9,
            interruption_cost: 0.2,
            delivery,
            reasons: vec!["urgency 0.70".into()],
        },
        state: ItemState::Pending,
        snoozed_until: None,
        created_at,
        updated_at: created_at,
        last_feedback: None,
    }
}

fn presence(place: Place, active: Option<&str>, can_speak: bool) -> PresenceState {
    let now = at("2026-11-16T18:00:00Z");
    let surfaces = active
        .map(|device_id| SurfaceStatus {
            device_id: device_id.into(),
            surface: SurfaceKind::Web,
            visible: true,
            input_idle_secs: 5,
            last_seen: now,
            voice_ready: true,
            can_speak,
        })
        .into_iter()
        .collect();
    PresenceState {
        place,
        since: now,
        active_device: active.map(str::to_owned),
        surfaces,
    }
}

fn ctx(quiet_hours: bool, spoken_last_hour: u32, telegram_paired: bool) -> RouteContext {
    RouteContext {
        quiet_hours,
        spoken_last_hour,
        telegram_paired,
    }
}

#[test]
fn routing_is_pure() {
    let settings = PresenceSettings::default();
    let item = item(Delivery::Interrupt, None, 0.8);
    let state = presence(Place::Desk, Some("web-1"), true);
    let context = ctx(false, 0, false);
    let first = route(&item, &state, &settings, &context);
    let second = route(&item, &state, &settings, &context);
    assert_eq!(first, second);
    assert_eq!(
        first,
        Route::Speak {
            device_id: "web-1".into()
        }
    );
}

#[test]
fn only_interrupts_are_routed() {
    let settings = PresenceSettings::default();
    let state = presence(Place::Desk, Some("web-1"), true);
    for delivery in [
        Delivery::Suggest,
        Delivery::Digest,
        Delivery::Silent,
        Delivery::Drop(sail_pulse::attention::DropReason::Duplicate),
    ] {
        assert_eq!(
            route(
                &item(delivery, None, 1.0),
                &state,
                &settings,
                &ctx(false, 0, true)
            ),
            Route::Hold
        );
    }
}

#[test]
fn speaking_requires_an_allowed_device_speech_and_budget() {
    let settings = PresenceSettings::default();
    let state = presence(Place::Desk, Some("web-1"), true);
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 0.7),
            &state,
            &settings,
            &ctx(false, 2, false)
        ),
        Route::Speak {
            device_id: "web-1".into()
        }
    );
    // Hourly cap reached.
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 0.7),
            &state,
            &settings,
            &ctx(false, 3, false)
        ),
        Route::Show {
            device_id: "web-1".into()
        }
    );
    // Speech off for the owner.
    let mute = PresenceSettings {
        speak_announcements: false,
        ..PresenceSettings::default()
    };
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 0.7),
            &state,
            &mute,
            &ctx(false, 0, false)
        ),
        Route::Show {
            device_id: "web-1".into()
        }
    );
    // The active device cannot speak.
    let no_voice = presence(Place::Desk, Some("web-1"), false);
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 0.7),
            &no_voice,
            &settings,
            &ctx(false, 0, false)
        ),
        Route::Show {
            device_id: "web-1".into()
        }
    );
    // Zero means never speak.
    let zero = PresenceSettings {
        max_spoken_per_hour: 0,
        ..PresenceSettings::default()
    };
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 0.7),
            &state,
            &zero,
            &ctx(false, 0, false)
        ),
        Route::Show {
            device_id: "web-1".into()
        }
    );
}

#[test]
fn quiet_hours_never_speak() {
    let settings = PresenceSettings::default();
    let state = presence(Place::Desk, Some("web-1"), true);
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 1.0),
            &state,
            &settings,
            &ctx(true, 0, true)
        ),
        Route::Show {
            device_id: "web-1".into()
        }
    );
    // Asleep is quiet even if the flag was not passed.
    let asleep = presence(Place::Asleep, None, false);
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 1.0),
            &asleep,
            &settings,
            &ctx(false, 0, true)
        ),
        Route::Hold
    );
}

#[test]
fn quiet_hours_telegram_only_when_away_and_allowed() {
    let settings = PresenceSettings::default();
    let away = presence(Place::Away, None, false);
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 0.1),
            &away,
            &settings,
            &ctx(true, 0, true)
        ),
        Route::Hold,
        "Telegram is opt-in"
    );
    let allowed = PresenceSettings {
        telegram_when_away: true,
        ..PresenceSettings::default()
    };
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 0.1),
            &away,
            &allowed,
            &ctx(true, 0, true)
        ),
        Route::Telegram,
        "quiet-hour override keeps its Telegram escape"
    );
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 0.1),
            &away,
            &allowed,
            &ctx(true, 0, false)
        ),
        Route::Hold,
        "no paired bot"
    );
}

#[test]
fn telegram_away_needs_pairing_permission_and_urgency() {
    let settings = PresenceSettings {
        telegram_when_away: true,
        ..PresenceSettings::default()
    };
    let away = presence(Place::Away, None, false);

    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 0.9),
            &away,
            &settings,
            &ctx(false, 0, true)
        ),
        Route::Telegram
    );
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 0.7),
            &away,
            &settings,
            &ctx(false, 0, true)
        ),
        Route::Hold,
        "below the urgency floor and no deadline"
    );
    // A near deadline raises the reach above the floor.
    let deadline = at("2026-11-16T18:30:00Z");
    assert_eq!(
        route(
            &item(Delivery::Interrupt, Some(deadline), 0.7),
            &away,
            &settings,
            &ctx(false, 0, true)
        ),
        Route::Telegram
    );
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 0.9),
            &away,
            &settings,
            &ctx(false, 0, false)
        ),
        Route::Hold,
        "no paired bot"
    );
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 0.9),
            &away,
            &PresenceSettings::default(),
            &ctx(false, 0, true)
        ),
        Route::Hold,
        "Telegram when away is off by default"
    );
    // An active device wins over Telegram when it can only show.
    let desk = presence(Place::Desk, Some("web-1"), false);
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 0.9),
            &desk,
            &settings,
            &ctx(false, 0, true)
        ),
        Route::Show {
            device_id: "web-1".into()
        }
    );
}

#[test]
fn stale_active_device_id_is_ignored() {
    let settings = PresenceSettings::default();
    let state = PresenceState {
        place: Place::Desk,
        since: at("2026-11-16T18:00:00Z"),
        active_device: Some("gone".into()),
        surfaces: Vec::new(),
    };
    assert_eq!(
        route(
            &item(Delivery::Interrupt, None, 0.9),
            &state,
            &settings,
            &ctx(false, 0, true)
        ),
        Route::Hold
    );
}

#[test]
fn deadline_reach_uses_the_assessment_clock() {
    let settings = PresenceSettings {
        telegram_when_away: true,
        ..PresenceSettings::default()
    };
    let away = presence(Place::Away, None, false);
    // 6 days out is well under the floor even though the deadline exists.
    let far = at("2026-11-16T18:00:00Z") + Duration::days(6);
    assert_eq!(
        route(
            &item(Delivery::Interrupt, Some(far), 0.7),
            &away,
            &settings,
            &ctx(false, 0, true)
        ),
        Route::Hold
    );
}
