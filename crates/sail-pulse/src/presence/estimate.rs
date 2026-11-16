//! Pure presence estimation: which place the owner is in, from surface metadata.
//!
//! No clock, randomness, I/O or global state; `now` is a parameter, so the same input
//! always produces the same output.

use chrono::{DateTime, Utc};

use super::{Place, PresenceSettings, STALE_AFTER_SECS, SurfaceKind, SurfaceStatus};

/// A surface is live while it is at most [`STALE_AFTER_SECS`] old. A clock that went
/// backwards (last_seen in the future) is treated as live rather than rejected.
pub(crate) fn is_live(surface: &SurfaceStatus, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(surface.last_seen).num_seconds() <= STALE_AFTER_SECS as i64
}

/// Order used when two active surfaces have the same idle time: Web, then Dock, then
/// Phone (fixed a priori: the desk is the least surprising place to speak from).
pub(crate) fn surface_priority(surface: SurfaceKind) -> u8 {
    match surface {
        SurfaceKind::Web => 0,
        SurfaceKind::Dock => 1,
        SurfaceKind::Phone => 2,
    }
}

/// A surface can speak when it is visible, its voice socket is open and the owner allowed
/// speech on that kind of surface. Liveness is checked by the caller.
pub(crate) fn can_speak(surface: &SurfaceStatus, settings: &PresenceSettings) -> bool {
    surface.visible && surface.voice_ready && settings.speak_on.contains(&surface.surface)
}

fn place_of(surface: SurfaceKind) -> Place {
    match surface {
        SurfaceKind::Web => Place::Desk,
        SurfaceKind::Dock => Place::Dock,
        SurfaceKind::Phone => Place::Phone,
    }
}

fn is_active(surface: &SurfaceStatus, now: DateTime<Utc>, settings: &PresenceSettings) -> bool {
    is_live(surface, now)
        && surface.visible
        && u64::from(surface.input_idle_secs) < settings.away_after_secs
}

/// Whether `candidate` beats `current`: smaller idle time, then Web > Dock > Phone, then
/// the most recent `last_seen`, then the device id (so the result never depends on the
/// caller's slice order).
fn is_better(candidate: &SurfaceStatus, current: &SurfaceStatus) -> bool {
    if candidate.input_idle_secs != current.input_idle_secs {
        return candidate.input_idle_secs < current.input_idle_secs;
    }
    if candidate.surface != current.surface {
        return surface_priority(candidate.surface) < surface_priority(current.surface);
    }
    if candidate.last_seen != current.last_seen {
        return candidate.last_seen > current.last_seen;
    }
    candidate.device_id < current.device_id
}

/// Estimate the owner's place and, when it is backed by a surface, the active device.
///
/// A surface is active when it is live, visible and its input idle time is below
/// `settings.away_after_secs`. The active device is the active surface with the smallest
/// idle time; ties go Web > Dock > Phone, then the latest `last_seen`. With no active
/// surface the place is `Asleep` during quiet hours and `Away` otherwise; `Unknown` is
/// returned when no heartbeat was ever recorded (an empty surface list).
pub fn estimate(
    surfaces: &[SurfaceStatus],
    now: DateTime<Utc>,
    quiet_hours: bool,
    settings: &PresenceSettings,
) -> (Place, Option<String>) {
    if surfaces.is_empty() {
        return (Place::Unknown, None);
    }
    let mut best: Option<&SurfaceStatus> = None;
    for surface in surfaces {
        if !is_active(surface, now, settings) {
            continue;
        }
        if best.is_none_or(|current| is_better(surface, current)) {
            best = Some(surface);
        }
    }
    match best {
        Some(surface) => (place_of(surface.surface), Some(surface.device_id.clone())),
        None if quiet_hours => (Place::Asleep, None),
        None => (Place::Away, None),
    }
}
