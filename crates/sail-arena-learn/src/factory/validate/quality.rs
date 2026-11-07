//! Quality validator (`CheckKind::Quality`): produces a deterministic [`QualityReport`]
//! covering objective coverage, step mix (at least one scored step per objective and at
//! least one transfer step), duration within 3–8 minutes, and progressive hint ladders.

use super::make_receipt;
use crate::courses::content::{MAX_HINTS, Step, StepKind};
use crate::factory::contracts::{
    CheckKind, DraftValidator, LessonDraft, ValidationContext, ValidationReceipt, Verdict,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Version tag stamped on receipts produced by [`QualityValidator`].
pub const VALIDATOR_VERSION: &str = "quality/1";

/// Minimum active minutes permitted for an ordinary lesson.
pub const MIN_LESSON_MINUTES: u32 = 3;

/// Maximum active minutes permitted for an ordinary lesson.
pub const MAX_LESSON_MINUTES: u32 = 8;

/// Deterministic pedagogical quality report for a [`LessonDraft`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityReport {
    /// Objectives declared on the lesson.
    pub objectives_declared: Vec<String>,
    /// Objectives tested by at least one scored step in the lesson.
    pub objectives_scored: Vec<String>,
    /// Declared objectives that have no scored step in the lesson.
    pub uncovered_objectives: Vec<String>,
    /// Number of `Explain` steps in the lesson.
    pub explain_step_count: usize,
    /// Number of scored (interactive) steps in the lesson.
    pub scored_step_count: usize,
    /// Number of transfer steps identified in the lesson.
    pub transfer_step_count: usize,
    /// Active duration of the lesson in minutes.
    pub minutes: u32,
    /// Whether `minutes` lies within `MIN_LESSON_MINUTES..=MAX_LESSON_MINUTES`.
    pub duration_in_bounds: bool,
    /// IDs of scored steps missing a valid hint ladder.
    pub steps_missing_hints: Vec<String>,
    /// Human-readable reasons why the draft failed quality validation (empty when passing).
    pub failures: Vec<String>,
}

/// Validates pedagogical quality rules deterministically without calling a reviewer model.
#[derive(Debug, Default, Clone, Copy)]
pub struct QualityValidator;

#[async_trait::async_trait]
impl DraftValidator for QualityValidator {
    fn check(&self) -> CheckKind {
        CheckKind::Quality
    }

    async fn validate(
        &self,
        draft: &LessonDraft,
        ctx: &ValidationContext<'_>,
    ) -> Vec<ValidationReceipt> {
        let report = evaluate_quality(draft);
        let verdict = if report.failures.is_empty() {
            Verdict::Pass
        } else {
            Verdict::Fail {
                reasons: report.failures,
            }
        };
        vec![make_receipt(
            draft,
            CheckKind::Quality,
            None,
            verdict,
            VALIDATOR_VERSION,
            ctx.now_ms,
        )]
    }
}

/// Computes the deterministic [`QualityReport`] for `draft`.
pub fn evaluate_quality(draft: &LessonDraft) -> QualityReport {
    let lesson = &draft.output.lesson;
    let mut failures = Vec::new();

    // 1. Duration check (3–8 minutes for a Lesson).
    let duration_in_bounds = (MIN_LESSON_MINUTES..=MAX_LESSON_MINUTES).contains(&lesson.minutes);
    if !duration_in_bounds {
        failures.push(format!(
            "lesson duration is {} min, expected {MIN_LESSON_MINUTES}–{MAX_LESSON_MINUTES} min",
            lesson.minutes
        ));
    }

    // 2. Step counts & transfer step detection.
    let mut explain_step_count = 0usize;
    let mut scored_step_count = 0usize;
    let mut transfer_step_count = 0usize;
    let mut seen_families = BTreeSet::new();
    let mut scored_objectives = BTreeSet::new();
    let mut steps_missing_hints = Vec::new();

    for step in &lesson.steps {
        if step.kind == StepKind::Explain {
            explain_step_count += 1;
            continue;
        }
        scored_step_count += 1;
        for obj_id in &step.objective_ids {
            scored_objectives.insert(obj_id.clone());
        }

        let explicit_transfer = is_explicit_transfer_step(step);
        let family_transfer = if let Some(fam) = &step.family {
            let is_new_after_first =
                !seen_families.is_empty() && !seen_families.contains(fam.as_str());
            seen_families.insert(fam.as_str());
            is_new_after_first
        } else {
            false
        };
        if explicit_transfer || family_transfer {
            transfer_step_count += 1;
        }

        if !has_valid_hint_ladder(step) {
            steps_missing_hints.push(step.id.clone());
        }
    }

    if scored_step_count == 0 {
        failures.push("lesson must contain at least one scored step".to_string());
    }
    if transfer_step_count == 0 {
        failures.push("lesson step mix must include at least one transfer step".to_string());
    }

    // 3. Objectives covered: every declared objective must have at least one scored step.
    let objectives_declared = lesson.objective_ids.clone();
    if objectives_declared.is_empty() {
        failures.push("lesson declares no objectives".to_string());
    }

    let mut uncovered_objectives = Vec::new();
    for obj_id in &objectives_declared {
        if !scored_objectives.contains(obj_id) {
            uncovered_objectives.push(obj_id.clone());
            failures.push(format!(
                "objective `{obj_id}` has no scored step in lesson `{}`",
                lesson.id
            ));
        }
    }

    // 4. Hint ladder presence on all scored steps.
    for step_id in &steps_missing_hints {
        failures.push(format!(
            "scored step `{step_id}` is missing a valid progressive hint ladder (1..={MAX_HINTS} distinct non-empty hints)"
        ));
    }

    QualityReport {
        objectives_declared,
        objectives_scored: scored_objectives.into_iter().collect(),
        uncovered_objectives,
        explain_step_count,
        scored_step_count,
        transfer_step_count,
        minutes: lesson.minutes,
        duration_in_bounds,
        steps_missing_hints,
        failures,
    }
}

fn is_explicit_transfer_step(step: &Step) -> bool {
    let check = |s: &str| {
        let lower = s.to_ascii_lowercase();
        lower.contains("transfer")
    };
    check(&step.id) || check(&step.title) || step.family.as_deref().is_some_and(check)
}

fn has_valid_hint_ladder(step: &Step) -> bool {
    if step.hints.is_empty() || step.hints.len() > MAX_HINTS {
        return false;
    }
    let mut seen = BTreeSet::new();
    for hint in &step.hints {
        let trimmed = hint.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_ascii_lowercase()) {
            return false;
        }
    }
    true
}
