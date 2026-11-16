//! Continuity task outcomes as attention signals.
//!
//! Discovery does not depend on Continuity: the caller converts durable tasks to
//! [`TaskOutcome`] and this module turns the newly finished ones into Pulse signals.
//! Pure: no clock, randomness, I/O or global state.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use sail_pulse::attention::{MAX_BODY_CHARS, MAX_TITLE_CHARS, Provenance, Signal, Source};

use crate::detect::excerpt;

/// How a task ended. serde: `"succeeded" | "failed" | "needs_intervention"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEnd {
    Succeeded,
    Failed,
    NeedsIntervention,
}

/// One finished Continuity task, in Discovery's own vocabulary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskOutcome {
    pub id: String,
    pub title: String,
    pub owner: String,
    pub end: TaskEnd,
    pub ended_at: DateTime<Utc>,
    pub summary: String,
}

impl TaskEnd {
    /// Stable machine kind used in the signal and its dedupe key.
    pub fn kind(self) -> &'static str {
        match self {
            TaskEnd::Succeeded => "task_succeeded",
            TaskEnd::Failed => "task_failed",
            TaskEnd::NeedsIntervention => "task_needs_you",
        }
    }

    /// Where this outcome should land, before Pulse's context and preferences. The
    /// numbers are fixed a priori so a failure interrupts and a success does not:
    ///
    /// With default attention settings (threshold 0.6, interruption cost 0.2, learned
    /// 0.5 so the score is `value`, and no deadline):
    /// - failed:       0.35*0.70 + 0.25*0.8 + 0.20 + 0.20 = 0.845; 0.845-0.2 = 0.645 →
    ///   Interrupt.
    /// - needs-you:    0.35*0.85 + 0.60 = 0.8975; 0.8975-0.2 = 0.6975 → Interrupt.
    /// - succeeded:    0.35*0.50 + 0.60 = 0.775; 0.775-0.2 = 0.575 → below 0.6, but above
    ///   the 0.45 suggest threshold, so Suggest and held for the greeting.
    pub fn urgency(self) -> f32 {
        match self {
            TaskEnd::Succeeded => 0.5,
            TaskEnd::Failed => 0.7,
            TaskEnd::NeedsIntervention => 0.85,
        }
    }

    fn title(self, task_title: &str) -> String {
        // Keep room for "Forge: " and the verb inside MAX_TITLE_CHARS.
        let title = excerpt(task_title, MAX_TITLE_CHARS - 40);
        match self {
            TaskEnd::Succeeded => format!("Forge: {title} succeeded"),
            TaskEnd::Failed => format!("Forge: {title} failed"),
            TaskEnd::NeedsIntervention => format!("Forge: {title} needs you"),
        }
    }
}

/// Outcomes that ended after `since` (exclusive) and not after `now` become signals.
pub fn task_signals(
    outcomes: &[TaskOutcome],
    since: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Vec<Signal> {
    outcomes
        .iter()
        .filter(|outcome| outcome.ended_at > since && outcome.ended_at <= now)
        .map(|outcome| {
            let kind = outcome.end.kind();
            Signal {
                source: Source::Projects,
                kind: kind.to_owned(),
                dedupe_key: format!("task:{}:{kind}", outcome.id),
                title: outcome.end.title(&outcome.title),
                body: excerpt(&outcome.summary, MAX_BODY_CHARS),
                urgency: outcome.end.urgency(),
                relevance: 0.8,
                novelty: 1.0,
                confidence: 1.0,
                deadline: None,
                provenance: Provenance::Observation,
                link: None,
                observed_at: outcome.ended_at,
            }
        })
        .collect()
}
