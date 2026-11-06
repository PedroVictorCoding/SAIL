//! Course evidence (#386): an append-only log of what the learner actually did, and pure
//! projections over it. Nothing here reads a clock: callers pass `now_ms`.
//!
//! Rules, kept deliberately simple and transparent (no knowledge tracing):
//! - Only graded attempts count. `unavailable` results are ignored. Viewing a lesson is exposure.
//! - A pass with any hint or a revealed solution is *assisted*. It never certifies an objective.
//! - The first independent pass makes an objective *provisional* and schedules a check after
//!   [`INTERVAL_DAYS`]\[0\].
//! - An independent pass at least [`MIN_DELAY_MS`] after the previous one, on a different
//!   problem family, is a *delayed* success. It makes the objective *retained* and moves the
//!   schedule one interval further.
//! - A wrong answer after a pass resets the schedule to the first interval and marks it *weak*.
//! - Any state whose check is due reads as *due* until it's attempted again.
use super::content::CoursePackage;
use super::grade::{ValidatorKind, Verdict};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// Heuristic spacing (planning assumptions, not proven optimal for every learner).
pub const INTERVAL_DAYS: [i64; 5] = [1, 3, 7, 14, 30];
/// The least time between passes for the later one to count as delayed retrieval.
pub const MIN_DELAY_MS: i64 = 20 * 60 * 60 * 1000;
/// The most review items offered in one session.
pub const REVIEW_CAP: usize = 5;
/// The policy version stored in responses, so a future change in rules is visible.
pub const POLICY_VERSION: &str = "courses-evidence-1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Context {
    Lesson,
    Diagnostic,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assistance {
    Independent,
    Hinted,
    SolutionSeen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Untested,
    Exposure,
    Assisted,
    Provisional,
    Due,
    Retained,
    Weak,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub evidence_id: String,
    pub attempt_id: String,
    pub context: Context,
    /// Lesson id, diagnostic session id, or review step id.
    pub context_id: String,
    pub course_id: String,
    pub course_version: String,
    pub step_id: String,
    pub family: Option<String>,
    pub objective_ids: Vec<String>,
    pub verdict: Verdict,
    pub validator: ValidatorKind,
    pub assistance: Assistance,
    pub confidence: Option<Confidence>,
    /// The full response as sent, so a retried `attempt_id` gets the same answer.
    pub response: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CourseEvent {
    Attempt(AttemptRecord),
    Exposure {
        course_id: String,
        lesson_id: String,
        step_id: String,
        objective_ids: Vec<String>,
    },
    Hint {
        context: Context,
        context_id: String,
        course_id: String,
        step_id: String,
        level: u8,
    },
    Reveal {
        context: Context,
        context_id: String,
        course_id: String,
        step_id: String,
    },
    LessonCertified {
        course_id: String,
        lesson_id: String,
        course_version: String,
    },
    DiagnosticStarted {
        id: String,
        course_id: String,
    },
    DiagnosticFinished {
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stored {
    pub seq: u64,
    pub at_ms: i64,
    pub event: CourseEvent,
}

/// One objective's projected evidence. A learner projection, not a fact about the world.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObjectiveEvidence {
    pub objective_id: String,
    pub title: String,
    pub state: EvidenceState,
    pub independent_passes: u32,
    pub assisted_passes: u32,
    pub failures: u32,
    pub last_independent_success: Option<String>,
    pub last_delayed_success: Option<String>,
    pub retention_due_at: Option<String>,
    pub why: String,
    pub not_assessed: bool,
    pub revision: u64,
    #[serde(skip)]
    pub due_ms: Option<i64>,
    #[serde(skip)]
    pub families_seen: BTreeSet<String>,
    #[serde(skip)]
    pub last_family_at: BTreeMap<String, i64>,
}

pub fn rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// Hints and reveals since the previous attempt on the same step in the same context, so a
/// retry after a hint is still assisted. Checked before an attempt is stored.
pub fn assistance_for(
    events: &[Stored],
    context: Context,
    context_id: &str,
    step_id: &str,
) -> Assistance {
    let mut level = Assistance::Independent;
    for s in events {
        match &s.event {
            CourseEvent::Hint {
                context: c,
                context_id: id,
                step_id: st,
                ..
            } if *c == context && id == context_id && st == step_id => {
                level = level.max(Assistance::Hinted);
            }
            CourseEvent::Reveal {
                context: c,
                context_id: id,
                step_id: st,
                ..
            } if *c == context && id == context_id && st == step_id => {
                level = Assistance::SolutionSeen;
            }
            CourseEvent::Attempt(a)
                if a.context == context
                    && a.context_id == context_id
                    && a.step_id == step_id
                    && a.verdict != Verdict::Unavailable
                    // A lesson step stays assisted through retries until it's passed. A
                    // review or diagnostic attempt ends its episode either way.
                    && (context != Context::Lesson || a.verdict == Verdict::Correct) =>
            {
                level = Assistance::Independent;
            }
            _ => {}
        }
    }
    level
}

/// Hints already shown for a step in a context (since the last ended episode).
pub fn hints_used(events: &[Stored], context: Context, context_id: &str, step_id: &str) -> u8 {
    let mut used = 0u8;
    for s in events {
        match &s.event {
            CourseEvent::Hint {
                context: c,
                context_id: id,
                step_id: st,
                level,
                ..
            } if *c == context && id == context_id && st == step_id => used = used.max(*level),
            CourseEvent::Attempt(a)
                if a.context == context
                    && a.context_id == context_id
                    && a.step_id == step_id
                    && a.verdict != Verdict::Unavailable
                    && (context != Context::Lesson || a.verdict == Verdict::Correct) =>
            {
                used = 0
            }
            _ => {}
        }
    }
    used
}

/// Projects every objective of `course` at `now_ms`.
pub fn project(course: &CoursePackage, events: &[Stored], now_ms: i64) -> Vec<ObjectiveEvidence> {
    course
        .objectives
        .iter()
        .map(|o| project_one(course, &o.id, &o.title, events, now_ms))
        .collect()
}

fn project_one(
    course: &CoursePackage,
    objective: &str,
    title: &str,
    events: &[Stored],
    now_ms: i64,
) -> ObjectiveEvidence {
    let mut exposed = false;
    let mut independent = 0u32;
    let mut assisted = 0u32;
    let mut failures = 0u32;
    let mut stage: usize = 0;
    let mut last_indep: Option<(i64, Option<String>)> = None;
    let mut last_delayed: Option<i64> = None;
    let mut retained = false;
    let mut weak = false;
    let mut due: Option<i64> = None;
    let mut families_seen = BTreeSet::new();
    let mut last_family_at = BTreeMap::new();
    let mut revision = 0u64;
    for s in events {
        let at = s.at_ms;
        match &s.event {
            CourseEvent::Exposure {
                course_id,
                objective_ids,
                ..
            } if course_id == &course.id && objective_ids.iter().any(|o| o == objective) => {
                exposed = true;
                revision = s.seq;
            }
            CourseEvent::Attempt(a)
                if a.course_id == course.id && a.objective_ids.iter().any(|o| o == objective) =>
            {
                exposed = true;
                revision = s.seq;
                if let Some(f) = &a.family {
                    families_seen.insert(f.clone());
                    last_family_at.insert(f.clone(), at);
                }
                match (a.verdict, a.assistance) {
                    (Verdict::Unavailable, _) => continue,
                    (Verdict::Correct, Assistance::Independent) => {
                        independent += 1;
                        weak = false;
                        match &last_indep {
                            None => {
                                stage = 0;
                                due = Some(at + INTERVAL_DAYS[0] * DAY_MS);
                            }
                            Some((prev, prev_family))
                                if at - prev >= MIN_DELAY_MS && prev_family != &a.family =>
                            {
                                stage = (stage + 1).min(INTERVAL_DAYS.len() - 1);
                                retained = true;
                                last_delayed = Some(at);
                                due = Some(at + INTERVAL_DAYS[stage] * DAY_MS);
                            }
                            Some(_) => {}
                        }
                        last_indep = Some((at, a.family.clone()));
                    }
                    (Verdict::Correct, _) => {
                        assisted += 1;
                        weak = false;
                        if last_indep.is_none() {
                            due = Some(due.map_or(at + DAY_MS, |d| d.min(at + DAY_MS)));
                        }
                    }
                    (Verdict::Incorrect, _) => {
                        failures += 1;
                        weak = true;
                        if last_indep.is_some() || a.context == Context::Review {
                            stage = 0;
                            retained = false;
                            due = Some(at + DAY_MS);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let attempted = independent + assisted + failures > 0;
    let is_due = due.is_some_and(|d| d <= now_ms);
    let state = if !attempted {
        if exposed {
            EvidenceState::Exposure
        } else {
            EvidenceState::Untested
        }
    } else if weak {
        EvidenceState::Weak
    } else if last_indep.is_none() {
        EvidenceState::Assisted
    } else if is_due {
        EvidenceState::Due
    } else if retained {
        EvidenceState::Retained
    } else {
        EvidenceState::Provisional
    };
    let next = due.map(rfc3339).unwrap_or_default();
    let why = match state {
        EvidenceState::Untested => {
            "Not assessed yet. SAIL doesn't assume you know it or don't.".to_string()
        }
        EvidenceState::Exposure => {
            "You've seen this material, but no answer has been checked yet.".to_string()
        }
        EvidenceState::Assisted => format!(
            "Passed only with a hint or the solution. A fresh independent problem is due {next}."
        ),
        EvidenceState::Weak => {
            format!("Your most recent answer was wrong. A new problem is scheduled for {next}.")
        }
        EvidenceState::Due => {
            "A delayed check is due now: a new problem, without help, shows whether it stuck."
                .to_string()
        }
        EvidenceState::Retained => {
            format!("Passed again on a new problem after a delay. The next check is {next}.")
        }
        EvidenceState::Provisional => format!(
            "Passed independently, but not yet after a delay. The first retention check is {next}."
        ),
    };
    ObjectiveEvidence {
        objective_id: objective.to_string(),
        title: title.to_string(),
        state,
        independent_passes: independent,
        assisted_passes: assisted,
        failures,
        last_independent_success: last_indep.map(|(t, _)| rfc3339(t)),
        last_delayed_success: last_delayed.map(rfc3339),
        retention_due_at: due.map(rfc3339),
        why,
        not_assessed: !attempted,
        revision,
        due_ms: due,
        families_seen,
        last_family_at,
    }
}

/// One review offered now: an objective that is due, plus an unseen variant for it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewPick {
    pub step_id: String,
    pub objective_id: String,
    pub objective_title: String,
    pub due_ms: i64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DueQueue {
    pub items: Vec<ReviewPick>,
    pub total_due: usize,
    /// Review attempts made in the last [`MIN_DELAY_MS`].
    pub reviewed_recently: usize,
}

impl DueQueue {
    /// The certified-advance gate. It closes while due reviews exist and the learner hasn't
    /// yet done a bounded session's worth today. The cap means a backlog can't lock
    /// advancement for good.
    pub fn blocks(&self) -> bool {
        self.total_due > 0 && self.reviewed_recently < self.total_due.min(REVIEW_CAP)
    }

    pub fn sessions(&self) -> usize {
        self.total_due.div_ceil(REVIEW_CAP)
    }
}

/// Builds the bounded review queue. Weak objectives come first, then the most overdue. Each
/// gets a review variant whose family the learner hasn't seen, or failing that, the one
/// seen longest ago.
pub fn due_queue(course: &CoursePackage, events: &[Stored], now_ms: i64) -> DueQueue {
    let evidence = project(course, events, now_ms);
    let mut due: Vec<&ObjectiveEvidence> = evidence
        .iter()
        .filter(|e| e.due_ms.is_some_and(|d| d <= now_ms))
        .filter(|e| {
            matches!(
                e.state,
                EvidenceState::Due | EvidenceState::Weak | EvidenceState::Assisted
            )
        })
        .collect();
    due.sort_by_key(|e| (e.state != EvidenceState::Weak, e.due_ms.unwrap_or(i64::MAX)));
    let reviewed_recently = events
        .iter()
        .filter(|s| now_ms - s.at_ms < MIN_DELAY_MS)
        .filter(|s| {
            matches!(&s.event, CourseEvent::Attempt(a)
                if a.context == Context::Review && a.course_id == course.id && a.verdict != Verdict::Unavailable)
        })
        .count();
    let mut used = BTreeSet::new();
    let mut items = Vec::new();
    for e in &due {
        if items.len() == REVIEW_CAP {
            break;
        }
        let mut candidates: Vec<_> = course
            .reviews
            .iter()
            .filter(|r| r.objective_ids.contains(&e.objective_id) && !used.contains(&r.id))
            .collect();
        candidates.sort_by_key(|r| {
            let family = r.family.clone().unwrap_or_default();
            (
                e.families_seen.contains(&family),
                e.last_family_at.get(&family).copied().unwrap_or(i64::MIN),
            )
        });
        let Some(step) = candidates.first() else {
            continue;
        };
        used.insert(step.id.clone());
        let reason = match e.state {
            EvidenceState::Weak => "Your last answer on this was wrong, so here is a new problem.",
            EvidenceState::Assisted => {
                "You passed this with help. Solve a new one on your own to count it."
            }
            _ => "It's time for a delayed check on a problem you haven't seen.",
        };
        items.push(ReviewPick {
            step_id: step.id.clone(),
            objective_id: e.objective_id.clone(),
            objective_title: e.title.clone(),
            due_ms: e.due_ms.unwrap_or(now_ms),
            reason: reason.to_string(),
        });
    }
    DueQueue {
        items,
        total_due: due.len(),
        reviewed_recently,
    }
}
