//! Deterministic attention scoring and preference learning (Antigravity).
//! Spec: the module doc in `attention/mod.rs`.
use chrono::{DateTime, Utc};

use super::{
    Assessment, AttentionContext, AttentionSettings, Autonomy, Delivery, DropReason, Feedback,
    Preference, Provenance, Signal,
};

const TWO_HOURS_SECS: f32 = 2.0 * 3600.0;
const SEVEN_DAYS_SECS: f32 = 7.0 * 86_400.0;
const DECAY_HALF_LIFE_DAYS: f32 = 30.0;

/// 0 without a deadline; 1.0 at ≤ 2 h; linear to 0 at 7 days; 0 when past.
pub fn deadline_pressure(deadline: Option<DateTime<Utc>>, now: DateTime<Utc>) -> f32 {
    let Some(deadline) = deadline else {
        return 0.0;
    };
    if deadline < now {
        return 0.0;
    }
    let secs = (deadline - now).num_seconds().max(0) as f32;
    if secs <= TWO_HOURS_SECS {
        return 1.0;
    }
    if secs >= SEVEN_DAYS_SECS {
        return 0.0;
    }
    let span = SEVEN_DAYS_SECS - TWO_HOURS_SECS;
    (1.0 - (secs - TWO_HOURS_SECS) / span).clamp(0.0, 1.0)
}

/// 0.2 normal; 0.6 voice/busy; 0.9 focus; 1.0 quiet hours (highest that applies).
pub fn interruption_cost(context: &AttentionContext) -> f32 {
    if context.quiet_hours {
        1.0
    } else if context.focus_until.is_some_and(|until| until > context.now) {
        0.9
    } else if context.voice_active || context.busy {
        0.6
    } else {
        0.2
    }
}

fn decayed_tallies(preference: Option<&Preference>, now: DateTime<Utc>) -> (f32, f32) {
    let Some(pref) = preference else {
        return (0.0, 0.0);
    };
    let days = (now - pref.updated_at).num_seconds().max(0) as f32 / 86_400.0;
    let decay = 0.5_f32.powf(days / DECAY_HALF_LIFE_DAYS);
    (
        pref.accepted.max(0.0) * decay,
        pref.rejected.max(0.0) * decay,
    )
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
    let pressure = deadline_pressure(signal.deadline, context.now);
    let urgency_prime = signal.urgency.max(pressure);
    let value = 0.35 * urgency_prime
        + 0.25 * signal.relevance
        + 0.20 * signal.novelty
        + 0.20 * signal.confidence;
    let (accepted, rejected) = decayed_tallies(preference, context.now);
    let learned = (accepted + 1.0) / (accepted + rejected + 2.0);
    let score = (value * (0.6 + 0.8 * learned)).clamp(0.0, 1.0);
    let cost = interruption_cost(context);

    let mut reasons = vec![format!(
        "score {score:.2} (urgency' {urgency_prime:.2}, value {value:.2}, learned {learned:.2}), cost {cost:.2}"
    )];

    // Rule 1: cooldown duplicate or muted (source, kind).
    if recently_delivered {
        reasons.push("dropped: same dedupe key within cooldown".into());
        return Assessment {
            score,
            interruption_cost: cost,
            delivery: Delivery::Drop(DropReason::Duplicate),
            reasons,
        };
    }
    if preference.is_some_and(|p| p.muted) {
        reasons.push("dropped: (source, kind) is muted".into());
        return Assessment {
            score,
            interruption_cost: cost,
            delivery: Delivery::Drop(DropReason::Muted),
            reasons,
        };
    }

    // Past deadline: stale unless it is an overdue user commitment.
    if signal.deadline.is_some_and(|d| d < context.now) {
        if signal.provenance == Provenance::UserCommitment {
            reasons.push("overdue user commitment kept for digest".into());
            return Assessment {
                score,
                interruption_cost: cost,
                delivery: Delivery::Digest,
                reasons,
            };
        }
        reasons.push("dropped: deadline is in the past (stale)".into());
        return Assessment {
            score,
            interruption_cost: cost,
            delivery: Delivery::Drop(DropReason::Stale),
            reasons,
        };
    }

    // Autonomy::Off drops everything except UserCommitment (at Digest).
    if settings.autonomy == Autonomy::Off {
        if signal.provenance == Provenance::UserCommitment {
            reasons.push("autonomy is off; user commitment held for digest".into());
            return Assessment {
                score,
                interruption_cost: cost,
                delivery: Delivery::Digest,
                reasons,
            };
        }
        reasons.push("dropped: autonomy is off".into());
        return Assessment {
            score,
            interruption_cost: cost,
            delivery: Delivery::Drop(DropReason::AutonomyOff),
            reasons,
        };
    }

    // Rule 2: low novelty without a deadline.
    if signal.deadline.is_none() && signal.novelty < settings.min_novelty {
        reasons.push(format!(
            "dropped: novelty {:.2} below min_novelty {:.2}",
            signal.novelty, settings.min_novelty
        ));
        return Assessment {
            score,
            interruption_cost: cost,
            delivery: Delivery::Drop(DropReason::NotNovel),
            reasons,
        };
    }

    let proactive_shift = if settings.autonomy == Autonomy::Proactive {
        0.1
    } else {
        0.0
    };
    let interrupt_threshold = (settings.interrupt_threshold - proactive_shift).max(0.0);
    let suggest_threshold = (settings.suggest_threshold - proactive_shift).max(0.0);

    // Rule 3: Interrupt.
    let trusted_override = urgency_prime >= 0.95 && signal.provenance == Provenance::UserCommitment;
    if settings.autonomy >= Autonomy::Balanced {
        if context.interrupts_last_hour < settings.max_interrupts_per_hour {
            if trusted_override {
                if score - 0.2 >= interrupt_threshold {
                    reasons.push("interrupt: trusted override for imminent user commitment".into());
                    return Assessment {
                        score,
                        interruption_cost: cost,
                        delivery: Delivery::Interrupt,
                        reasons,
                    };
                }
            } else if context.quiet_hours {
                reasons.push("quiet hours block non-override interrupt".into());
            } else if score - cost >= interrupt_threshold {
                reasons.push(format!(
                    "interrupt: net score {:.2} >= threshold {:.2}",
                    score - cost,
                    interrupt_threshold
                ));
                return Assessment {
                    score,
                    interruption_cost: cost,
                    delivery: Delivery::Interrupt,
                    reasons,
                };
            }
        } else {
            reasons.push("hourly interrupt budget reached".into());
        }
    } else {
        reasons.push("quiet autonomy caps delivery at digest".into());
    }

    // Rule 4: Suggest.
    if settings.autonomy >= Autonomy::Balanced {
        if context.suggestions_today >= settings.max_suggestions_per_day {
            reasons.push("daily suggestion budget reached".into());
        } else if score >= suggest_threshold {
            reasons.push(format!(
                "suggest: score {score:.2} >= threshold {suggest_threshold:.2}"
            ));
            return Assessment {
                score,
                interruption_cost: cost,
                delivery: Delivery::Suggest,
                reasons,
            };
        }
    }

    // Rule 5: Digest.
    if score >= settings.digest_threshold {
        reasons.push(format!(
            "digest: score {score:.2} >= threshold {:.2}",
            settings.digest_threshold
        ));
        return Assessment {
            score,
            interruption_cost: cost,
            delivery: Delivery::Digest,
            reasons,
        };
    }

    // Rule 6: Silent.
    reasons.push("silent: below digest threshold".into());
    Assessment {
        score,
        interruption_cost: cost,
        delivery: Delivery::Silent,
        reasons,
    }
}

/// Apply one feedback to a preference (decay first, then add). Returns the updated value.
pub fn learn(mut preference: Preference, feedback: Feedback, now: DateTime<Utc>) -> Preference {
    let (accepted, rejected) = decayed_tallies(Some(&preference), now);
    preference.accepted = accepted;
    preference.rejected = rejected;
    match feedback {
        Feedback::Useful | Feedback::Investigate => preference.accepted += 1.0,
        Feedback::Dismiss => preference.rejected += 1.0,
        Feedback::NotNow | Feedback::Later(_) => preference.rejected += 0.25,
        Feedback::Mute => preference.muted = true,
    }
    preference.updated_at = now;
    preference
}
