//! Deterministic attention scoring and preference learning (Antigravity).
//! Spec: the module doc in `attention/mod.rs`.
use chrono::{DateTime, Utc};

use super::{Assessment, AttentionContext, AttentionSettings, Feedback, Preference, Signal};

/// 0 without a deadline; 1.0 at ≤ 2 h; linear to 0 at 7 days; 0 when past.
pub fn deadline_pressure(deadline: Option<DateTime<Utc>>, now: DateTime<Utc>) -> f32 {
    let _ = (deadline, now);
    unimplemented!("Antigravity: attention::score::deadline_pressure")
}

/// 0.2 normal; 0.6 voice/busy; 0.9 focus; 1.0 quiet hours (highest that applies).
pub fn interruption_cost(context: &AttentionContext) -> f32 {
    let _ = context;
    unimplemented!("Antigravity: attention::score::interruption_cost")
}

/// Assess one normalized signal. `recently_delivered` is true when the same dedupe key
/// was delivered or digested within the cooldown; `preference` is the learned tally for
/// the signal's (source, kind), if any.
pub fn assess(
    signal: &Signal,
    context: &AttentionContext,
    settings: &AttentionSettings,
    preference: Option<&Preference>,
    recently_delivered: bool,
) -> Assessment {
    let _ = (signal, context, settings, preference, recently_delivered);
    unimplemented!("Antigravity: attention::score::assess")
}

/// Apply one feedback to a preference (decay first, then add). Returns the updated value.
pub fn learn(preference: Preference, feedback: Feedback, now: DateTime<Utc>) -> Preference {
    let _ = (preference, feedback, now);
    unimplemented!("Antigravity: attention::score::learn")
}
