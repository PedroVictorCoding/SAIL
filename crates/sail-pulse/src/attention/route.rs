//! Where an interruption should be delivered.
//!
//! Routing decides *where* an [`AttentionItem`] goes, never *whether*: Pulse's
//! [`score::assess`](super::score::assess) owns that decision, and routing never raises a
//! delivery. It is pure: no clock, randomness, I/O or global state.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{AttentionItem, Delivery, score};
use crate::presence::{Place, PresenceSettings, PresenceState, SurfaceStatus};

/// One destination. Internally tagged.
///
/// serde: `{"route":"speak","device_id":"…"}`, `{"route":"show","device_id":"…"}`,
/// `{"route":"telegram"}` or `{"route":"hold"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "route", rename_all = "snake_case")]
pub enum Route {
    Speak { device_id: String },
    Show { device_id: String },
    Telegram,
    Hold,
}

/// Inputs routing needs that are not the item or the presence estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteContext {
    /// The routing clock; held or snoozed items may be routed long after ingest.
    pub now: DateTime<Utc>,
    pub quiet_hours: bool,
    pub spoken_last_hour: u32,
    pub telegram_paired: bool,
}

/// Telegram is only used for items at least this urgent (fixed a priori).
pub const TELEGRAM_MIN_URGENCY: f32 = 0.8;

/// Choose the single destination for `item`. Pure.
///
/// First match wins:
/// 1. anything Pulse did not assess as `Interrupt` is held for the next greeting;
/// 2. quiet hours / `Asleep` never speak: Telegram if away or asleep and the owner
///    allowed it, else show on the active device, else hold;
/// 3. the active device can speak, speech is enabled and the hourly cap is not spent;
/// 4. otherwise show on the active device;
/// 5. when away, Telegram if the owner allowed it, a bot is paired and the item (or its
///    deadline) is at least [`TELEGRAM_MIN_URGENCY`];
/// 6. otherwise hold.
///
/// The deadline pressure used by rule 5 is evaluated against [`RouteContext::now`].
pub fn route(
    item: &AttentionItem,
    presence: &PresenceState,
    settings: &PresenceSettings,
    ctx: &RouteContext,
) -> Route {
    if item.assessment.delivery != Delivery::Interrupt {
        return Route::Hold;
    }

    let active = active_surface(presence);

    if matches!(presence.place, Place::Asleep) || ctx.quiet_hours {
        if matches!(presence.place, Place::Away | Place::Asleep)
            && settings.telegram_when_away
            && ctx.telegram_paired
        {
            return Route::Telegram;
        }
        return match active {
            Some(surface) => Route::Show {
                device_id: surface.device_id.clone(),
            },
            None => Route::Hold,
        };
    }

    if let Some(surface) = active
        && surface.can_speak
        && settings.speak_announcements
        && ctx.spoken_last_hour < settings.max_spoken_per_hour
    {
        return Route::Speak {
            device_id: surface.device_id.clone(),
        };
    }

    if let Some(surface) = active {
        return Route::Show {
            device_id: surface.device_id.clone(),
        };
    }

    let reach = item
        .signal
        .urgency
        .max(score::deadline_pressure(item.signal.deadline, ctx.now));
    if presence.place == Place::Away
        && settings.telegram_when_away
        && ctx.telegram_paired
        && reach >= TELEGRAM_MIN_URGENCY
    {
        return Route::Telegram;
    }

    Route::Hold
}

fn active_surface(presence: &PresenceState) -> Option<&SurfaceStatus> {
    let device_id = presence.active_device.as_deref()?;
    presence
        .surfaces
        .iter()
        .find(|surface| surface.device_id == device_id)
}
