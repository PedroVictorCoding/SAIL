//! In-memory presence store, with settings persisted as JSON.
//!
//! Heartbeats stay in memory only. The one durable file is
//! `$SAIL_DATA/pulse/presence.json`, which holds [`PresenceSettings`] and nothing else.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use tracing::warn;

use super::estimate::{can_speak, estimate, is_live, surface_priority};
use super::{
    Heartbeat, MAX_DEVICES, Place, PresenceEvent, PresenceSettings, PresenceState, SurfaceStatus,
};

/// Longest idle time accepted from a client, in seconds: one day. A larger value is
/// indistinguishable from "away" for every decision here, and the cap keeps arithmetic
/// small (fixed a priori).
const MAX_IDLE_SECS: u32 = 86_400;

/// Most spoken announcements per hour after clamping on save (owner decision: 3; the
/// range 0..=6 leaves room to turn it up without becoming spammy).
const MAX_SPOKEN_PER_HOUR: u32 = 6;

/// The file name under the Pulse data directory.
const SETTINGS_FILE: &str = "presence.json";

struct Inner {
    surfaces: HashMap<String, SurfaceStatus>,
    place: Place,
    since: Option<DateTime<Utc>>,
    settings: PresenceSettings,
}

impl Inner {
    fn refresh(&mut self, now: DateTime<Utc>, quiet_hours: bool) -> Vec<PresenceEvent> {
        let surfaces: Vec<SurfaceStatus> = self.surfaces.values().cloned().collect();
        let (place, active) = estimate(&surfaces, now, quiet_hours, &self.settings);
        let mut events = Vec::new();
        if place != self.place {
            if place.is_active() {
                if matches!(self.place, Place::Away | Place::Asleep | Place::Unknown) {
                    let away_for_secs = if self.place == Place::Unknown {
                        // No absence start was ever observed.
                        0
                    } else {
                        self.since
                            .map(|since| {
                                now.signed_duration_since(since).num_seconds().max(0) as u64
                            })
                            .unwrap_or(0)
                    };
                    if let Some(device_id) = active {
                        events.push(PresenceEvent::Arrived {
                            place,
                            device_id,
                            away_for_secs,
                        });
                    }
                }
            } else if matches!(place, Place::Away | Place::Asleep) {
                events.push(PresenceEvent::Left { from: self.place });
            }
            self.place = place;
            self.since = Some(now);
        } else if self.since.is_none() {
            self.since = Some(now);
        }
        events
    }

    fn state(&self, now: DateTime<Utc>, quiet_hours: bool) -> PresenceState {
        let all: Vec<SurfaceStatus> = self.surfaces.values().cloned().collect();
        let (place, active_device) = estimate(&all, now, quiet_hours, &self.settings);
        let mut surfaces: Vec<SurfaceStatus> = all
            .into_iter()
            .filter(|surface| is_live(surface, now))
            .map(|mut surface| {
                surface.can_speak = can_speak(&surface, &self.settings);
                surface
            })
            .collect();
        // Most recently active first: latest heartbeat, then smallest idle time, then the
        // fixed Web > Dock > Phone order, then device id for a deterministic tie-break.
        surfaces.sort_by(|a, b| {
            b.last_seen
                .cmp(&a.last_seen)
                .then(a.input_idle_secs.cmp(&b.input_idle_secs))
                .then(surface_priority(a.surface).cmp(&surface_priority(b.surface)))
                .then(a.device_id.cmp(&b.device_id))
        });
        PresenceState {
            place,
            since: self.since.unwrap_or(now),
            active_device,
            surfaces,
        }
    }
}

/// Presence state for the daemon. Cheap to share behind an `Arc`.
pub struct PresenceStore {
    inner: Mutex<Inner>,
    path: PathBuf,
}

impl PresenceStore {
    /// Open the store in `dir`, creating it if needed. A missing or corrupt settings file
    /// yields defaults, never an error or a panic.
    pub fn open(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("create {}: {error}", dir.display()))?;
        let path = dir.join(SETTINGS_FILE);
        let settings = load_settings(&path);
        Ok(Self {
            inner: Mutex::new(Inner {
                surfaces: HashMap::new(),
                place: Place::Unknown,
                since: None,
                settings,
            }),
            path,
        })
    }

    /// Record one heartbeat. The server clock, not the client, stamps it.
    ///
    /// Returns the new state and any place transitions. The only error is an invalid
    /// `device_id` (1..=64 chars of `[A-Za-z0-9_-]`).
    pub fn record(
        &self,
        beat: Heartbeat,
        now: DateTime<Utc>,
        quiet_hours: bool,
    ) -> Result<(PresenceState, Vec<PresenceEvent>), String> {
        if !valid_device_id(&beat.device_id) {
            return Err("device_id must be 1..=64 characters of [A-Za-z0-9_-]".into());
        }
        let mut inner = self.lock();
        let mut surface = SurfaceStatus {
            device_id: beat.device_id.clone(),
            surface: beat.surface,
            visible: beat.visible,
            input_idle_secs: beat.input_idle_secs.min(MAX_IDLE_SECS),
            last_seen: now,
            voice_ready: beat.voice_ready,
            can_speak: false,
        };
        surface.can_speak = can_speak(&surface, &inner.settings);
        inner.surfaces.insert(beat.device_id, surface);
        evict_oldest(&mut inner.surfaces);
        let events = inner.refresh(now, quiet_hours);
        let state = inner.state(now, quiet_hours);
        Ok((state, events))
    }

    /// An addressed turn counts as input from that device and may make it active.
    pub fn note_addressed_turn(
        &self,
        device_id: &str,
        now: DateTime<Utc>,
        quiet_hours: bool,
    ) -> Vec<PresenceEvent> {
        let mut inner = self.lock();
        let settings = inner.settings.clone();
        if let Some(surface) = inner.surfaces.get_mut(device_id) {
            surface.input_idle_secs = 0;
            surface.last_seen = now;
            surface.can_speak = can_speak(surface, &settings);
        }
        inner.refresh(now, quiet_hours)
    }

    /// Drop stale surfaces from the estimate; fires `Left` when the owner becomes
    /// `Away` or `Asleep`.
    pub fn tick(&self, now: DateTime<Utc>, quiet_hours: bool) -> Vec<PresenceEvent> {
        self.lock().refresh(now, quiet_hours)
    }

    /// Current presence without mutating the place history.
    pub fn state(&self, now: DateTime<Utc>, quiet_hours: bool) -> PresenceState {
        self.lock().state(now, quiet_hours)
    }

    /// The persisted settings.
    pub fn settings(&self) -> PresenceSettings {
        self.lock().settings.clone()
    }

    /// Save settings, clamping `max_spoken_per_hour` to 0..=6, and return what was stored.
    pub fn save_settings(&self, s: PresenceSettings) -> Result<PresenceSettings, String> {
        let mut s = s;
        s.max_spoken_per_hour = s.max_spoken_per_hour.min(MAX_SPOKEN_PER_HOUR);
        let mut inner = self.lock();
        persist_settings(&self.path, &s)?;
        inner.settings = s.clone();
        Ok(s)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock still exposes the last consistent state; recovering it is
        // preferable to panicking in a daemon.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn valid_device_id(device_id: &str) -> bool {
    (1..=64).contains(&device_id.len())
        && device_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn evict_oldest(surfaces: &mut HashMap<String, SurfaceStatus>) {
    while surfaces.len() > MAX_DEVICES {
        let oldest = surfaces
            .values()
            .min_by(|a, b| {
                a.last_seen
                    .cmp(&b.last_seen)
                    .then(a.device_id.cmp(&b.device_id))
            })
            .map(|surface| surface.device_id.clone());
        match oldest {
            Some(device_id) => {
                surfaces.remove(&device_id);
            }
            None => break,
        }
    }
}

fn load_settings(path: &Path) -> PresenceSettings {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<PresenceSettings>(&bytes) {
            Ok(settings) => settings,
            Err(error) => {
                warn!(path = %path.display(), %error, "presence settings are corrupt; using defaults");
                PresenceSettings::default()
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => PresenceSettings::default(),
        Err(error) => {
            warn!(path = %path.display(), %error, "presence settings unreadable; using defaults");
            PresenceSettings::default()
        }
    }
}

fn persist_settings(path: &Path, settings: &PresenceSettings) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(settings)
        .map_err(|error| format!("serialize presence settings: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes)
        .map_err(|error| format!("write {}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .map_err(|error| format!("replace {}: {error}", path.display()))
}
