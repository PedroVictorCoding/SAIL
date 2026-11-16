//! Where an interruption should be delivered.
//!
//! Routing decides *where* an [`AttentionItem`] goes, never *whether*: Pulse's
//! [`score::assess`](super::score::assess) owns that decision, and routing never raises a
//! delivery. K2 implements the rules; this module holds the fixed contract.

use serde::{Deserialize, Serialize};

use super::AttentionItem;
use crate::presence::{PresenceSettings, PresenceState};

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
    pub quiet_hours: bool,
    pub spoken_last_hour: u32,
    pub telegram_paired: bool,
}

/// Telegram is only used for items at least this urgent (fixed a priori).
pub const TELEGRAM_MIN_URGENCY: f32 = 0.8;

/// Choose the single destination for `item`. Pure; K2 implements the rules.
pub fn route(
    _item: &AttentionItem,
    _presence: &PresenceState,
    _settings: &PresenceSettings,
    _ctx: &RouteContext,
) -> Route {
    Route::Hold
}
