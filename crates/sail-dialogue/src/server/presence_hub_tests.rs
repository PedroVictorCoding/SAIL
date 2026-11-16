//! Round 26 acceptance for the presence seams: the hands-free gate, the follow-up
//! window, routing an `Interrupt` to one device, and the arrival greeting.

use super::*;
use crate::tts::TtsConfig;
use sail_pulse::attention::store::AttentionStore;
use sail_pulse::attention::{AttentionContext, Provenance, Signal, Source};
use sail_pulse::presence::Heartbeat;

fn t(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000 + secs, 0).expect("valid time")
}

fn socket() -> (SocketParts, mpsc::Receiver<Outbound>) {
    let (tx, rx) = mpsc::channel(64);
    (
        SocketParts {
            tx,
            epoch: Arc::new(AtomicU64::new(0)),
            tts: LocalNeuralTts::new(TtsConfig::default()),
            // Client speech: announcements arrive as `speech_text`, no host TTS in tests.
            speech: SpeechOutput::Client,
        },
        rx,
    )
}

fn drain(rx: &mut mpsc::Receiver<Outbound>) -> Vec<Value> {
    let mut out = Vec::new();
    while let Ok(message) = rx.try_recv() {
        out.push(message.value);
    }
    out
}

fn surface(device: &str, hands_free: bool) -> Value {
    json!({"type":"surface","device_id":device,"surface":"dock","hands_free":hands_free})
}

#[test]
fn without_hands_free_every_utterance_is_a_turn() {
    let hub = Hub::new();
    assert_eq!(
        hub.gate("no-link", "we should sail this weekend", t(0)),
        Gate::Pass("we should sail this weekend".into())
    );
    let (parts, mut rx) = socket();
    hub.register("s1", &surface("dock-a", false), parts, t(0));
    assert_eq!(
        drain(&mut rx),
        vec![json!({"type":"hands_free","state":"off"})]
    );
    assert_eq!(
        hub.gate("s1", "what time is it", t(1)),
        Gate::Pass("what time is it".into())
    );
}

#[test]
fn hands_free_passes_only_addressed_speech_and_leaks_nothing() {
    let hub = Hub::new();
    let (parts, mut rx) = socket();
    hub.register("s1", &surface("dock-a", true), parts, t(0));
    assert_eq!(
        drain(&mut rx),
        vec![json!({"type":"hands_free","state":"listening"})]
    );
    assert_eq!(
        hub.gate("s1", "We should sail this weekend", t(1)),
        Gate::Discard
    );
    assert_eq!(
        hub.gate("s1", "SAIL, what's next today?", t(2)),
        Gate::Pass("what's next today?".into())
    );
    assert_eq!(hub.gate("s1", "Sail.", t(3)), Gate::Wake);
    // The gate itself never writes the transcript anywhere.
    let leaked = drain(&mut rx)
        .iter()
        .any(|m| m.to_string().contains("weekend"));
    assert!(!leaked);
}

#[test]
fn follow_up_opens_only_after_playback_of_a_fresh_answer_and_is_used_once() {
    let hub = Hub::new();
    let (parts, mut rx) = socket();
    hub.register("s1", &surface("dock-a", true), parts, t(0));
    drain(&mut rx);

    hub.answered("s1", t(10));
    // Still speaking: SAIL's own voice must not count as a follow-up.
    assert_eq!(hub.gate("s1", "and tomorrow?", t(11)), Gate::Discard);

    hub.playback_finished("s1", t(15));
    assert_eq!(
        drain(&mut rx),
        vec![json!({"type":"hands_free","state":"follow_up"})]
    );
    assert_eq!(
        hub.gate("s1", "and tomorrow?", t(15 + FOLLOW_UP_SECS)),
        Gate::Pass("and tomorrow?".into())
    );
    assert_eq!(
        drain(&mut rx),
        vec![json!({"type":"hands_free","state":"listening"})]
    );
    // One follow-up per answer.
    assert_eq!(hub.gate("s1", "and the day after?", t(20)), Gate::Discard);

    // A window expires on its own.
    hub.answered("s1", t(30));
    hub.playback_finished("s1", t(31));
    assert_eq!(
        hub.gate("s1", "and next week?", t(31 + FOLLOW_UP_SECS + 1)),
        Gate::Discard
    );

    // A stale answer, or playback with no answer, opens nothing.
    hub.answered("s1", t(100));
    hub.playback_finished("s1", t(100 + ANSWER_FRESH_SECS + 1));
    hub.playback_finished("s1", t(400));
    assert_eq!(hub.gate("s1", "and then?", t(401)), Gate::Discard);
}

#[test]
fn invalid_surfaces_are_refused() {
    let hub = Hub::new();
    for value in [
        json!({"type":"surface","device_id":"bad id!","surface":"dock","hands_free":true}),
        json!({"type":"surface","device_id":"dock-a","surface":"toaster","hands_free":true}),
        json!({"type":"surface","surface":"dock","hands_free":true}),
    ] {
        let (parts, mut rx) = socket();
        hub.register("s1", &value, parts, t(0));
        assert_eq!(drain(&mut rx)[0]["type"], "error");
        assert!(!hub.hands_free("s1"));
    }
}

fn wiring(dir: &std::path::Path) -> (Wiring, AttentionStore) {
    let presence = Arc::new(PresenceStore::open(dir).expect("presence store"));
    let attention = AttentionStore::open(dir.join("attention.db")).expect("attention store");
    (
        Wiring {
            presence,
            pulse: None,
            briefing: None,
        },
        attention,
    )
}

fn context(now: DateTime<Utc>) -> AttentionContext {
    AttentionContext {
        now,
        quiet_hours: false,
        focus_until: None,
        voice_active: false,
        busy: false,
        interrupts_last_hour: 0,
        suggestions_today: 0,
    }
}

fn stuck_task(now: DateTime<Utc>) -> Signal {
    Signal {
        source: Source::Projects,
        kind: "task_needs_you".into(),
        dedupe_key: "task:t1:task_needs_you".into(),
        title: "Course Factory: Networks needs you".into(),
        body: "Two sources could not be read.".into(),
        urgency: 0.85,
        relevance: 0.8,
        novelty: 1.0,
        confidence: 1.0,
        deadline: None,
        provenance: Provenance::Observation,
        link: None,
        observed_at: now,
    }
}

fn beat(device: &str) -> Heartbeat {
    Heartbeat {
        device_id: device.into(),
        surface: SurfaceKind::Dock,
        visible: true,
        input_idle_secs: 0,
        charging: Some(true),
        voice_ready: true,
    }
}

#[tokio::test]
async fn an_interrupt_is_spoken_on_the_active_device_only_and_marked_delivered() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (wiring, attention) = wiring(dir.path());
    let hub = Hub::new();
    let now = Utc::now();
    let item = attention
        .ingest(stuck_task(now), context(now))
        .expect("ingest");
    assert_eq!(item.assessment.delivery, Delivery::Interrupt);

    wiring
        .presence
        .record(beat("dock-a"), now, false)
        .expect("heartbeat");
    let (dock, mut dock_rx) = socket();
    hub.register("s-dock", &surface("dock-a", true), dock, now);
    let (other, mut other_rx) = socket();
    hub.register("s-other", &surface("dock-b", true), other, now);
    drain(&mut dock_rx);
    drain(&mut other_rx);

    deliver_pending(&hub, &wiring, &attention, false, now).await;

    let sent = drain(&mut dock_rx);
    assert_eq!(sent[0]["type"], "announce");
    assert_eq!(sent[0]["item_id"], item.id.as_str());
    assert_eq!(sent[0]["speak"], true);
    assert_eq!(sent[0]["source"], "projects");
    assert!(sent[0]["reasons"].as_array().is_some_and(|r| !r.is_empty()));
    assert_eq!(sent[1]["type"], "speech_text");
    assert_eq!(
        sent[1]["text"],
        "Course Factory: Networks needs you. Two sources could not be read."
    );
    assert!(drain(&mut other_rx).is_empty(), "one surface only");
    assert!(
        attention
            .pending_interrupts(now)
            .expect("pending")
            .is_empty()
    );
    assert_eq!(hub.spoken_last_hour(now), 1);
}

#[tokio::test]
async fn a_busy_device_gets_the_card_without_speech_and_no_socket_means_no_delivery() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (wiring, attention) = wiring(dir.path());
    let hub = Hub::new();
    let now = Utc::now();
    attention
        .ingest(stuck_task(now), context(now))
        .expect("ingest");
    wiring
        .presence
        .record(beat("dock-a"), now, false)
        .expect("heartbeat");

    // Heartbeats but no voice socket yet: nothing is marked delivered.
    deliver_pending(&hub, &wiring, &attention, false, now).await;
    assert_eq!(attention.pending_interrupts(now).expect("pending").len(), 1);

    let (dock, mut rx) = socket();
    hub.register("s-dock", &surface("dock-a", true), dock, now);
    drain(&mut rx);
    hub.turn_started("s-dock");
    deliver_pending(&hub, &wiring, &attention, false, now).await;
    let sent = drain(&mut rx);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["speak"], false);
    assert!(
        attention
            .pending_interrupts(now)
            .expect("pending")
            .is_empty()
    );
}

#[tokio::test]
async fn an_arrival_after_a_long_absence_is_greeted_with_titles_not_ids() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (wiring, attention) = wiring(dir.path());
    let hub = Hub::new();
    let now = Utc::now();
    let item = attention
        .ingest(stuck_task(now), context(now))
        .expect("ingest");
    wiring
        .presence
        .record(beat("dock-a"), now, false)
        .expect("heartbeat");
    let (dock, mut rx) = socket();
    hub.register("s-dock", &surface("dock-a", true), dock, now);
    drain(&mut rx);

    // Too short an absence: no greeting.
    greet(&hub, &wiring, &attention, "dock-a", 600).await;
    assert!(drain(&mut rx).is_empty());

    greet(&hub, &wiring, &attention, "dock-a", 5_400).await;
    let sent = drain(&mut rx);
    assert_eq!(sent[0]["type"], "presence_greeting");
    let spoken = sent[0]["spoken"].as_str().expect("spoken").to_owned();
    assert!(
        spoken.contains("Course Factory: Networks needs you"),
        "{spoken}"
    );
    assert_eq!(
        sent[0]["items"],
        json!([{"id": item.id, "title": "Course Factory: Networks needs you"}])
    );
    assert!(!sent[0].to_string().contains("item_ids"));
    assert_eq!(sent[1]["type"], "speech_text");
    assert_eq!(sent[1]["text"], spoken.as_str());
    assert!(
        attention
            .pending_interrupts(now)
            .expect("pending")
            .is_empty()
    );
}

#[test]
fn spoken_text_keeps_speech_to_a_headline() {
    assert_eq!(spoken_text("Done!", ""), "Done.");
    assert_eq!(
        spoken_text("Build failed", "2 tests."),
        "Build failed. 2 tests."
    );
    assert_eq!(
        spoken_text("Build failed", &"x".repeat(200)),
        "Build failed."
    );
}
