//! Presence: where the owner is, and which surface can speak to them.
//!
//! This is the first slice of the Sense capability (#12). It lives in Pulse until Sense
//! needs its own crate: Pulse already owns notification timing, and routing an
//! interruption needs to know both *whether* it matters (attention) and *where* the owner
//! is (presence). Nothing here senses on its own; clients send metadata-only heartbeats
//! and the server stamps them with its own clock.
//!
//! Privacy invariant: only tab visibility, seconds since the last input event, app
//! foreground state, charging and whether a voice socket is open are accepted. Heartbeats
//! live in memory; only [`PresenceSettings`] is persisted.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Kind of client surface. serde: `"web" | "dock" | "phone"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    Web,
    Dock,
    Phone,
}

/// What a client reports. Server clock stamps it; clients never send time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Heartbeat {
    /// 1..=64 chars of `[A-Za-z0-9_-]`; anything else is rejected.
    pub device_id: String,
    pub surface: SurfaceKind,
    /// Tab visible / screen on and app foreground.
    pub visible: bool,
    /// Seconds since the last pointer, key, touch or addressed turn; clamped to 86 400.
    pub input_idle_secs: u32,
    #[serde(default)]
    pub charging: Option<bool>,
    /// A voice socket from this device is open with audio out.
    #[serde(default)]
    pub voice_ready: bool,
}

/// Estimated place of the owner. serde: `"desk" | "dock" | "phone" | "away" |
/// "asleep" | "unknown"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Place {
    Desk,
    Dock,
    Phone,
    Away,
    Asleep,
    Unknown,
}

impl Place {
    /// True for the three places backed by an active surface.
    pub fn is_active(self) -> bool {
        matches!(self, Place::Desk | Place::Dock | Place::Phone)
    }
}

/// One live client surface as the server currently understands it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SurfaceStatus {
    pub device_id: String,
    pub surface: SurfaceKind,
    pub visible: bool,
    pub input_idle_secs: u32,
    pub last_seen: DateTime<Utc>,
    pub voice_ready: bool,
    pub can_speak: bool,
}

/// The server's estimate of where the owner is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresenceState {
    pub place: Place,
    pub since: DateTime<Utc>,
    pub active_device: Option<String>,
    /// Live surfaces only, most recently active first.
    pub surfaces: Vec<SurfaceStatus>,
}

/// A change worth acting on. Internally tagged.
///
/// serde: `{"event":"arrived","place":"desk","device_id":"…","away_for_secs":3600}` or
/// `{"event":"left","from":"desk"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum PresenceEvent {
    Arrived {
        place: Place,
        device_id: String,
        away_for_secs: u64,
    },
    Left {
        from: Place,
    },
}

/// Owner-facing presence controls. Only these values are persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PresenceSettings {
    /// Seconds without an active surface before the owner counts as away.
    /// Fixed a priori (10 minutes): long enough for a break, short enough to greet.
    pub away_after_secs: u64,
    pub greet_on_arrival: bool,
    /// Minimum absence before an arrival greeting is spoken. Fixed a priori from the
    /// owner decision "45 min".
    pub greet_after_away_secs: u64,
    pub speak_announcements: bool,
    /// Surfaces announcements may be spoken on.
    pub speak_on: Vec<SurfaceKind>,
    /// Spoken announcements allowed per hour. Owner decision: 3; clamped to 0..=6 on save.
    pub max_spoken_per_hour: u32,
    /// Opt-in, default off: send an interruption to Telegram while the owner is away.
    pub telegram_when_away: bool,
}

impl Default for PresenceSettings {
    fn default() -> Self {
        Self {
            away_after_secs: 600,
            greet_on_arrival: true,
            greet_after_away_secs: 2_700,
            speak_announcements: true,
            speak_on: vec![SurfaceKind::Web, SurfaceKind::Dock],
            max_spoken_per_hour: 3,
            telegram_when_away: false,
        }
    }
}

/// A surface is live while it is at most this many seconds stale: three missed 30 s
/// heartbeats (fixed a priori).
pub const STALE_AFTER_SECS: u64 = 90;

/// Most devices tracked at once; the oldest is evicted (fixed a priori).
pub const MAX_DEVICES: usize = 16;
