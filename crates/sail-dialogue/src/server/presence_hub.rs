//! Presence seams (round 26): which voice socket belongs to which device, the hands-free
//! gate, the follow-up window, and delivery of announcements and arrival greetings.
//!
//! The pure decisions live elsewhere: where the owner is (`sail_pulse::presence`), where
//! an `Interrupt` goes (`sail_pulse::attention::route`), whether speech addresses SAIL
//! (`crate::address`) and what a greeting says (`crate::greeting`). This module only
//! connects them to the sockets, the attention store and Telegram.
//!
//! Privacy: speech that does not address SAIL is discarded here, before any transcript is
//! emitted, logged, observed or sent to a model. The client only learns `not_addressed`.

use super::*;
use crate::address::{Addressed, FOLLOW_UP_SECS, FollowUpWindow, addressed};
use crate::briefing::{BriefingEngine, ReturnBriefing};
use crate::greeting::{Lang, compose};
use chrono::{DateTime, Duration as ChronoDuration, Timelike, Utc};
use sail_pulse::Pulse;
use sail_pulse::attention::route::{Route, RouteContext, route};
use sail_pulse::attention::{AttentionItem, Delivery};
use sail_pulse::presence::store::PresenceStore;
use sail_pulse::presence::{PresenceEvent, SurfaceKind};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, OnceLock};

/// An answer this recent may open the follow-up window when its playback ends (fixed a
/// priori: longer than any spoken reply, short enough that a stale `playback_finished`
/// from an old answer cannot open it).
const ANSWER_FRESH_SECS: i64 = 180;
/// Presence loop period. Heartbeats arrive every 30 s; 15 s halves the worst-case lag.
const LOOP_SECS: u64 = 15;
/// Longest announcement body that is also spoken; longer bodies are only shown, so speech
/// stays a headline (fixed a priori).
const MAX_SPOKEN_BODY_CHARS: usize = 120;
/// How long the arrival greeting waits for the return briefing before using held items
/// alone (fixed a priori: a greeting that arrives late is worse than a shorter one).
const BRIEFING_TIMEOUT_SECS: u64 = 4;
/// Held suggestions considered for a greeting.
const GREETING_SUGGESTIONS: usize = 3;

/// What happens to one finished utterance.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Gate {
    /// Run a turn with this text (the wake name already removed).
    Pass(String),
    /// The bare name: acknowledge and wait for the request.
    Wake,
    /// Not addressed to SAIL: drop it without a trace.
    Discard,
}

#[derive(Clone)]
struct Link {
    device_id: String,
    hands_free: bool,
    tx: mpsc::Sender<Outbound>,
    epoch: Arc<AtomicU64>,
    tts: LocalNeuralTts,
    speech: SpeechOutput,
    follow_up: FollowUpWindow,
    answered_at: Option<DateTime<Utc>>,
    busy: bool,
}

/// What a socket hands the hub so announcements can reach and speak on it.
pub(super) struct SocketParts {
    pub(super) tx: mpsc::Sender<Outbound>,
    pub(super) epoch: Arc<AtomicU64>,
    pub(super) tts: LocalNeuralTts,
    pub(super) speech: SpeechOutput,
}

/// Live voice sockets that identified their device, keyed by voice session.
pub(super) struct Hub {
    links: Mutex<HashMap<String, Link>>,
    presence: OnceLock<Arc<PresenceStore>>,
    spoken: Mutex<VecDeque<DateTime<Utc>>>,
    quiet: AtomicBool,
}

pub(super) fn hub() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(Hub::new)
}

/// Unregisters the session's link when the socket handler returns.
pub(super) struct SessionGuard(pub(super) String);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        hub().unregister(&self.0);
    }
}

/// Marks a session busy for the life of a turn, including when the turn is aborted.
pub(super) struct BusyGuard(String);
impl BusyGuard {
    pub(super) fn new(session: &str) -> Self {
        hub().turn_started(session);
        Self(session.to_owned())
    }
}
impl Drop for BusyGuard {
    fn drop(&mut self) {
        hub().turn_ended(&self.0);
    }
}

fn valid_device_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn send_now(tx: &mpsc::Sender<Outbound>, value: Value) {
    let _ = tx.try_send(Outbound {
        epoch: None,
        value,
        wire: None,
    });
}

fn hands_free_state(link: &Link, now: DateTime<Utc>) -> &'static str {
    if !link.hands_free {
        "off"
    } else if link.follow_up.accepts(now) {
        "follow_up"
    } else {
        "listening"
    }
}

impl Hub {
    fn new() -> Self {
        Self {
            links: Mutex::new(HashMap::new()),
            presence: OnceLock::new(),
            spoken: Mutex::new(VecDeque::new()),
            quiet: AtomicBool::new(false),
        }
    }

    fn install_presence(&self, store: Arc<PresenceStore>) {
        let _ = self.presence.set(store);
    }

    /// Handle a client's `{"type":"surface",…}` message. Invalid ids are refused.
    pub(super) fn register(
        &self,
        session: &str,
        value: &Value,
        socket: SocketParts,
        now: DateTime<Utc>,
    ) {
        let SocketParts {
            tx,
            epoch,
            tts,
            speech,
        } = socket;
        let device_id = value["device_id"].as_str().unwrap_or_default();
        let surface = serde_json::from_value::<SurfaceKind>(value["surface"].clone());
        if !valid_device_id(device_id) || surface.is_err() {
            send_now(
                &tx,
                json!({"type":"error","message":"Unknown device; presence features are off for this connection."}),
            );
            return;
        }
        let hands_free = value["hands_free"].as_bool() == Some(true);
        let mut links = lock(&self.links);
        let previous = links.remove(session);
        let link = Link {
            device_id: device_id.to_owned(),
            hands_free,
            tx,
            epoch,
            tts,
            speech,
            follow_up: previous
                .as_ref()
                .map(|p| p.follow_up.clone())
                .unwrap_or_default(),
            answered_at: previous.as_ref().and_then(|p| p.answered_at),
            busy: previous.as_ref().is_some_and(|p| p.busy),
        };
        send_now(
            &link.tx,
            json!({"type":"hands_free","state":hands_free_state(&link, now)}),
        );
        links.insert(session.to_owned(), link);
    }

    pub(super) fn set_speech(&self, session: &str, speech: SpeechOutput) {
        if let Some(link) = lock(&self.links).get_mut(session) {
            link.speech = speech;
        }
    }

    fn unregister(&self, session: &str) {
        lock(&self.links).remove(session);
    }

    pub(super) fn hands_free(&self, session: &str) -> bool {
        lock(&self.links).get(session).is_some_and(|l| l.hands_free)
    }

    /// Decide what a finished utterance becomes. Without hands-free every utterance is a
    /// turn, as before. With it, only the follow-up window or an addressed utterance is.
    pub(super) fn gate(&self, session: &str, transcript: &str, now: DateTime<Utc>) -> Gate {
        let device_id;
        let gate;
        {
            let mut links = lock(&self.links);
            let Some(link) = links.get_mut(session) else {
                return Gate::Pass(transcript.to_owned());
            };
            if !link.hands_free {
                return Gate::Pass(transcript.to_owned());
            }
            if link.follow_up.accepts(now) {
                link.follow_up.close();
                send_now(&link.tx, json!({"type":"hands_free","state":"listening"}));
                gate = Gate::Pass(transcript.to_owned());
            } else {
                gate = match addressed(transcript) {
                    Addressed::Yes { rest } if rest.trim().is_empty() => {
                        // The "Yes?" is an answer: its playback opens the window.
                        link.answered_at = Some(now);
                        Gate::Wake
                    }
                    Addressed::Yes { rest } => Gate::Pass(rest),
                    Addressed::No => Gate::Discard,
                };
            }
            device_id = link.device_id.clone();
        }
        if gate != Gate::Discard
            && let Some(presence) = self.presence.get()
        {
            presence.note_addressed_turn(&device_id, now, self.quiet.load(Ordering::SeqCst));
        }
        gate
    }

    /// Speak a short acknowledgement after the bare wake name.
    pub(super) async fn acknowledge_wake(&self, session: &str) {
        let link = lock(&self.links).get(session).cloned();
        if let Some(link) = link {
            speak(&link, "Yes?").await;
        }
    }

    fn turn_started(&self, session: &str) {
        if let Some(link) = lock(&self.links).get_mut(session) {
            link.busy = true;
        }
    }

    fn turn_ended(&self, session: &str) {
        if let Some(link) = lock(&self.links).get_mut(session) {
            link.busy = false;
        }
    }

    /// A reply was produced; its playback ending may open the follow-up window.
    pub(super) fn answered(&self, session: &str, now: DateTime<Utc>) {
        if let Some(link) = lock(&self.links).get_mut(session) {
            link.answered_at = Some(now);
        }
    }

    /// The client finished playing SAIL's speech. Opening the window only now (not when
    /// generation ends) keeps SAIL's own voice from counting as a follow-up.
    pub(super) fn playback_finished(&self, session: &str, now: DateTime<Utc>) {
        let tx;
        {
            let mut links = lock(&self.links);
            let Some(link) = links.get_mut(session) else {
                return;
            };
            let fresh = link
                .answered_at
                .is_some_and(|at| now - at <= ChronoDuration::seconds(ANSWER_FRESH_SECS));
            link.answered_at = None;
            if !link.hands_free || !fresh {
                return;
            }
            link.follow_up.open(now);
            send_now(&link.tx, json!({"type":"hands_free","state":"follow_up"}));
            tx = link.tx.clone();
        }
        let session = session.to_owned();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(FOLLOW_UP_SECS as u64 + 1)).await;
                let still = lock(&hub().links)
                    .get(&session)
                    .is_some_and(|l| l.hands_free && !l.follow_up.accepts(Utc::now()));
                if still {
                    send_now(&tx, json!({"type":"hands_free","state":"listening"}));
                }
            });
        }
    }

    /// The most recently registered idle link for a device, else a busy one.
    fn link_for(&self, device_id: &str) -> Option<Link> {
        let links = lock(&self.links);
        let mut matching = links.values().filter(|l| l.device_id == device_id);
        let first = matching.next()?.clone();
        if !first.busy {
            return Some(first);
        }
        Some(matching.find(|l| !l.busy).cloned().unwrap_or(first))
    }

    fn spoken_last_hour(&self, now: DateTime<Utc>) -> u32 {
        let mut spoken = lock(&self.spoken);
        while spoken
            .front()
            .is_some_and(|at| now - *at > ChronoDuration::hours(1))
        {
            spoken.pop_front();
        }
        spoken.len() as u32
    }

    fn note_spoken(&self, now: DateTime<Utc>) {
        lock(&self.spoken).push_back(now);
    }

    /// Send an announcement to one device. Returns `None` when the device has no open
    /// voice socket, else whether it was spoken. A device in the middle of a turn gets
    /// the card without speech, so SAIL never talks over itself.
    async fn announce(
        &self,
        device_id: &str,
        item: &AttentionItem,
        speak_it: bool,
        now: DateTime<Utc>,
    ) -> Option<bool> {
        let link = self.link_for(device_id)?;
        let speak_it = speak_it && !link.busy;
        let signal = &item.signal;
        emit_session(
            &link.tx,
            json!({
                "type": "announce",
                "item_id": item.id,
                "title": signal.title,
                "body": signal.body,
                "source": signal.source,
                "kind": signal.kind,
                "link": signal.link,
                "speak": speak_it,
                "reasons": item.assessment.reasons,
            }),
        )
        .await;
        if speak_it {
            self.note_spoken(now);
            speak(&link, &spoken_text(&signal.title, &signal.body)).await;
        }
        Some(speak_it)
    }
}

fn spoken_text(title: &str, body: &str) -> String {
    let title = title.trim().trim_end_matches(['.', '!', '?']);
    let body = body.trim();
    if body.is_empty() || body.chars().count() > MAX_SPOKEN_BODY_CHARS {
        format!("{title}.")
    } else {
        format!("{title}. {body}")
    }
}

/// Speak `text` on one socket, in the same wire format a reply uses. It carries the
/// socket's current epoch, so barge-in drops the rest of it like any reply.
async fn speak(link: &Link, text: &str) {
    let current = link.epoch.load(Ordering::SeqCst);
    if link.speech == SpeechOutput::Client {
        emit(
            &link.tx,
            current,
            json!({"type":"speech_text","channel":"voice:tts","text":text,"pause_ms":pause_after_ms(text),"fragment":1,"state":"speaking","turn_id":current}),
        )
        .await;
        return;
    }
    match link.tts.synthesize_pcm(text).await {
        Ok((pcm, rate)) => {
            let mut orb = OrbEngine::new();
            orb.set_state(OrbState::Speaking);
            orb.set_speaker("assistant");
            for chunk in pcm.chunks((rate / 10).max(1) as usize) {
                let telemetry = orb.compute_telemetry(chunk, rate);
                emit(
                    &link.tx,
                    current,
                    json!({"type":"audio_chunk","channel":"voice:tts","audio_base64":STANDARD.encode(crate::stt_wav_encode(chunk,rate)),"sample_rate":rate,"telemetry":telemetry,"state":"speaking","turn_id":current}),
                )
                .await;
            }
        }
        Err(error) => warn!(%error, "announcement speech failed; the card is still shown"),
    }
}

fn quiet_at(pulse: &Option<Arc<Pulse>>, now: DateTime<Utc>) -> bool {
    pulse
        .as_ref()
        .and_then(|p| p.settings().ok())
        .is_some_and(|s| s.is_quiet_at(now))
}

fn local_hour(pulse: &Option<Arc<Pulse>>, now: DateTime<Utc>) -> u32 {
    let offset = pulse
        .as_ref()
        .and_then(|p| p.settings().ok())
        .map_or(0, |s| s.utc_offset_minutes);
    (now + ChronoDuration::minutes(i64::from(offset))).hour()
}

fn telegram_paired() -> bool {
    crate::gateway_service::global().is_some_and(|g| {
        g.status().connected && g.pairing_view().is_ok_and(|v| !v.paired.is_empty())
    })
}

fn empty_briefing(now: DateTime<Utc>) -> ReturnBriefing {
    ReturnBriefing {
        id: String::new(),
        generated_at: now,
        local_time_str: String::new(),
        greeting: String::new(),
        headline: String::new(),
        away_duration_minutes: None,
        staged_music: None,
        learning_plan: None,
        events_to_watch: Vec::new(),
        news_radar: Vec::new(),
        project_continuity: None,
        system_sentinel: None,
        dismissed: false,
    }
}

/// Everything the presence loop needs.
pub(super) struct Wiring {
    pub(super) presence: Arc<PresenceStore>,
    pub(super) pulse: Option<Arc<Pulse>>,
    pub(super) briefing: Option<Arc<BriefingEngine>>,
}

/// Install the store for the socket seams and, when proactive initiative is on, start the
/// loop that greets arrivals and routes interruptions.
pub(super) fn start(wiring: Wiring) {
    hub().install_presence(wiring.presence.clone());
    let Some(proactive) = crate::proactive::global() else {
        info!("presence routing off: proactive initiative is off");
        return;
    };
    let attention = proactive.initiative.attention().clone();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(run(hub(), wiring, attention));
        }
        Err(_) => warn!("presence routing off: no async runtime"),
    }
}

async fn run(
    hub: &'static Hub,
    wiring: Wiring,
    attention: Arc<sail_pulse::attention::store::AttentionStore>,
) {
    let mut events = wiring.presence.subscribe();
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(LOOP_SECS));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = timer.tick() => {
                let now = Utc::now();
                let quiet = quiet_at(&wiring.pulse, now);
                hub.quiet.store(quiet, Ordering::SeqCst);
                wiring.presence.tick(now, quiet);
                deliver_pending(hub, &wiring, &attention, quiet, now).await;
            }
            event = events.recv() => match event {
                Ok(PresenceEvent::Arrived { device_id, away_for_secs, .. }) => {
                    greet(hub, &wiring, &attention, &device_id, away_for_secs).await;
                }
                Ok(PresenceEvent::Left { .. }) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
        }
    }
}

/// Route every deliverable `Interrupt` to one place, and mark what was delivered.
async fn deliver_pending(
    hub: &Hub,
    wiring: &Wiring,
    attention: &sail_pulse::attention::store::AttentionStore,
    quiet: bool,
    now: DateTime<Utc>,
) {
    let items = match attention.pending_interrupts(now) {
        Ok(items) => items,
        Err(error) => {
            warn!(%error, "pending attention items unavailable");
            return;
        }
    };
    if items.is_empty() {
        return;
    }
    let settings = wiring.presence.settings();
    let paired = telegram_paired();
    for item in items {
        let state = wiring.presence.state(now, quiet);
        let ctx = RouteContext {
            now,
            quiet_hours: quiet,
            spoken_last_hour: hub.spoken_last_hour(now),
            telegram_paired: paired,
        };
        let delivered = match route(&item, &state, &settings, &ctx) {
            Route::Speak { device_id } => hub.announce(&device_id, &item, true, now).await,
            Route::Show { device_id } => hub.announce(&device_id, &item, false, now).await,
            Route::Telegram => crate::gateway_service::global().map(|gateway| {
                gateway.broadcast(
                    &format!("attention:{}", item.id),
                    &spoken_text(&item.signal.title, &item.signal.body),
                );
                false
            }),
            Route::Hold => None,
        };
        if delivered.is_some()
            && let Err(error) = attention.mark_delivered(&item.id, now)
        {
            warn!(%error, item = %item.id, "announced item not marked delivered");
        }
    }
}

/// Greet an arrival after a long enough absence, on the device it arrived at.
async fn greet(
    hub: &Hub,
    wiring: &Wiring,
    attention: &sail_pulse::attention::store::AttentionStore,
    device_id: &str,
    away_for_secs: u64,
) {
    let settings = wiring.presence.settings();
    if !settings.greet_on_arrival || away_for_secs < settings.greet_after_away_secs {
        return;
    }
    let Some(link) = hub.link_for(device_id) else {
        return;
    };
    let now = Utc::now();
    let mut held: Vec<AttentionItem> = attention
        .pending_interrupts(now)
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.assessment.delivery == Delivery::Interrupt)
        .collect();
    held.extend(
        attention
            .suggestions(GREETING_SUGGESTIONS)
            .unwrap_or_default(),
    );
    let briefing = match &wiring.briefing {
        Some(engine) => tokio::time::timeout(
            std::time::Duration::from_secs(BRIEFING_TIMEOUT_SECS),
            engine.get_or_prepare(),
        )
        .await
        .unwrap_or_else(|_| empty_briefing(now)),
        None => empty_briefing(now),
    };
    let greeting = compose(
        &briefing,
        &held,
        away_for_secs,
        now,
        local_hour(&wiring.pulse, now),
        Lang::En,
    );
    let items: Vec<Value> = greeting
        .item_ids
        .iter()
        .filter_map(|id| held.iter().find(|i| &i.id == id))
        .map(|i| json!({"id": i.id, "title": i.signal.title}))
        .collect();
    emit_session(
        &link.tx,
        json!({"type":"presence_greeting","spoken":greeting.spoken,"items":items,"away_for_secs":away_for_secs}),
    )
    .await;
    let quiet = hub.quiet.load(Ordering::SeqCst);
    let can_speak = wiring
        .presence
        .state(now, quiet)
        .surfaces
        .iter()
        .any(|s| s.device_id == device_id && s.can_speak);
    if can_speak && settings.speak_announcements && !quiet && !link.busy {
        speak(&link, &greeting.spoken).await;
    }
    for id in &greeting.item_ids {
        if let Err(error) = attention.mark_delivered(id, now) {
            warn!(%error, item = %id, "greeted item not marked delivered");
        }
    }
}

#[cfg(test)]
#[path = "presence_hub_tests.rs"]
mod tests;
