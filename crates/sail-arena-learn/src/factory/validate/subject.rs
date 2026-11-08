//! Subject validator (`CheckKind::Subject`): deterministic domain validation for each step
//! and subject fixture in a lesson draft.
//!
//! Covers:
//! - **ExactKey**: choice, predict, order, and fill answer keys are consistent, single-choice
//!   steps have exactly one correct option unless `multi` is set, and distractors are distinct.
//! - **FormalLogic**: propositional arguments in steps are re-checked with [`crate::courses::logic`]
//!   and compared against the step's answer key.
//! - **Math / Physics**: [`NumericFixture`] formulas are evaluated over `knowns` using
//!   [`super::units`] and checked against `expected` for both SI dimension and value within
//!   `relative_tolerance`.
//! - **Rubric**: [`RubricFixture`] steps always produce [`Verdict::NeedsReview`] with their
//!   criteria listed (never `Pass` by itself), and require non-empty `not_penalised` for
//!   philosophy courses/objectives.

use super::make_receipt;
use super::units::{check_numeric_fixture, parse_quantity};
use crate::courses::content::{AnswerKey, Step, StepKind};
use crate::courses::grade::normalize;
use crate::courses::logic;
use crate::factory::contracts::{
    CheckKind, DraftValidator, LessonDraft, NumericFixture, RubricFixture, ValidationContext,
    ValidationReceipt, Verdict,
};
use std::collections::{BTreeMap, BTreeSet};

/// Version tag stamped on receipts produced by [`SubjectValidator`].
pub const VALIDATOR_VERSION: &str = "subject/1";

/// Deterministic subject validator across `ExactKey`, `FormalLogic`, `Math`/`Physics`, and
/// `Rubric`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SubjectValidator;

#[async_trait::async_trait]
impl DraftValidator for SubjectValidator {
    fn check(&self) -> CheckKind {
        CheckKind::Subject
    }

    async fn validate(
        &self,
        draft: &LessonDraft,
        ctx: &ValidationContext<'_>,
    ) -> Vec<ValidationReceipt> {
        validate_subject(draft, ctx.now_ms)
    }
}

/// Validates all scored steps and subject fixtures in `draft`, returning per-step
/// [`ValidationReceipt`]s (or a whole-lesson receipt if the draft has no scored steps).
pub fn validate_subject(draft: &LessonDraft, now_ms: i64) -> Vec<ValidationReceipt> {
    let numeric_by_step: BTreeMap<&str, &NumericFixture> = draft
        .output
        .numeric
        .iter()
        .map(|f| (f.step_id.as_str(), f))
        .collect();
    let rubric_by_step: BTreeMap<&str, &RubricFixture> = draft
        .output
        .rubric
        .iter()
        .map(|f| (f.step_id.as_str(), f))
        .collect();

    let is_philosophy_course = is_philosophy_context(
        &draft.course_id,
        &draft.output.lesson.id,
        &draft.output.lesson.objective_ids,
    );

    let mut receipts = Vec::new();
    let mut handled_numeric = BTreeSet::new();
    let mut handled_rubric = BTreeSet::new();

    for step in draft
        .output
        .lesson
        .steps
        .iter()
        .chain(draft.output.review_variants.iter())
    {
        if !step.scored() {
            continue;
        }
        let step_phil = is_philosophy_course
            || is_philosophy_context(&draft.course_id, &step.id, &step.objective_ids);

        let num_fix = numeric_by_step.get(step.id.as_str()).copied();
        if num_fix.is_some() {
            handled_numeric.insert(step.id.as_str());
        }
        let rub_fix = rubric_by_step.get(step.id.as_str()).copied();
        if rub_fix.is_some() {
            handled_rubric.insert(step.id.as_str());
        }

        let verdict = validate_scored_step(step, num_fix, rub_fix, step_phil);
        receipts.push(make_receipt(
            draft,
            CheckKind::Subject,
            Some(step.id.clone()),
            verdict,
            VALIDATOR_VERSION,
            now_ms,
        ));
    }

    // Any orphan NumericFixture whose step_id didn't match a scored step still gets checked.
    for num in &draft.output.numeric {
        if !handled_numeric.contains(num.step_id.as_str()) {
            let verdict = match check_numeric_fixture(num) {
                Ok(_) => Verdict::Fail {
                    reasons: vec![format!(
                        "NumericFixture references unknown or non-scored step `{}`",
                        num.step_id
                    )],
                },
                Err(e) => Verdict::Fail {
                    reasons: vec![format!("NumericFixture `{}`: {e}", num.step_id)],
                },
            };
            receipts.push(make_receipt(
                draft,
                CheckKind::Subject,
                Some(num.step_id.clone()),
                verdict,
                VALIDATOR_VERSION,
                now_ms,
            ));
        }
    }

    // Any orphan RubricFixture whose step_id didn't match a scored step still gets checked.
    for rub in &draft.output.rubric {
        if !handled_rubric.contains(rub.step_id.as_str()) {
            let mut verdict = validate_rubric_fixture(rub, is_philosophy_course);
            if matches!(verdict, Verdict::NeedsReview { .. }) {
                verdict = Verdict::Fail {
                    reasons: vec![format!(
                        "RubricFixture references unknown or non-scored step `{}`",
                        rub.step_id
                    )],
                };
            }
            receipts.push(make_receipt(
                draft,
                CheckKind::Subject,
                Some(rub.step_id.clone()),
                verdict,
                VALIDATOR_VERSION,
                now_ms,
            ));
        }
    }

    if receipts.is_empty() {
        receipts.push(make_receipt(
            draft,
            CheckKind::Subject,
            None,
            Verdict::Pass,
            VALIDATOR_VERSION,
            now_ms,
        ));
    }

    receipts
}

fn validate_scored_step(
    step: &Step,
    numeric: Option<&NumericFixture>,
    rubric: Option<&RubricFixture>,
    is_philosophy: bool,
) -> Verdict {
    let mut failures = Vec::new();
    let mut review_reasons = Vec::new();

    // 1. ExactKey / structural key consistency for non-Rust steps.
    if step.kind != StepKind::RustEdit {
        validate_exact_key(step, &mut failures);
    }

    // 2. FormalLogic check: if the step embeds a formal propositional argument, re-derive
    //    validity with `courses::logic` and compare against the key.
    let logic_checked = validate_formal_logic_step(step, &mut failures);

    // 3. Math / Physics check: if the step has a NumericFixture, evaluate formula and check
    //    both dimension and value against `expected` within `relative_tolerance`.
    if let Some(num_fix) = numeric {
        match check_numeric_fixture(num_fix) {
            Ok(expected_q) => {
                validate_step_key_matches_quantity(
                    step,
                    expected_q,
                    num_fix.relative_tolerance,
                    &mut failures,
                );
            }
            Err(err) => {
                failures.push(format!("step `{}` numeric check failed: {err}", step.id));
            }
        }
    }

    // 4. Rubric check: always `NeedsReview` with criteria listed (never `Pass` by itself),
    //    and `not_penalised` must be non-empty for philosophy.
    if let Some(rub_fix) = rubric {
        match validate_rubric_fixture(rub_fix, is_philosophy) {
            Verdict::Fail { reasons } => failures.extend(reasons),
            Verdict::NeedsReview { reasons } => review_reasons.extend(reasons),
            Verdict::Pass | Verdict::Unavailable { .. } => {}
        }
    } else if is_philosophy && step.kind == StepKind::Fill && !logic_checked && numeric.is_none() {
        // A free-text fill step in philosophy that is not a formal logic derivation is
        // open-ended/ambiguous and requires owner review rather than automatic pass.
        review_reasons.push(format!(
            "philosophy free-text step `{}` has no formal logic derivation or rubric; requires owner review",
            step.id
        ));
    }

    if !failures.is_empty() {
        Verdict::Fail { reasons: failures }
    } else if !review_reasons.is_empty() {
        Verdict::NeedsReview {
            reasons: review_reasons,
        }
    } else {
        Verdict::Pass
    }
}

fn validate_exact_key(step: &Step, failures: &mut Vec<String>) {
    let ctx = format!("step `{}`", step.id);
    let Some(key) = &step.key else {
        failures.push(format!("{ctx}: scored step is missing an AnswerKey"));
        return;
    };

    match (step.kind, key) {
        (StepKind::Choice | StepKind::Predict, AnswerKey::Choice(key_ids)) => {
            if step.options.len() < 2 {
                failures.push(format!("{ctx}: choice step needs at least 2 options"));
            }
            if key_ids.is_empty() {
                failures.push(format!("{ctx}: choice key cannot be empty"));
            }
            if !step.multi && key_ids.len() != 1 {
                failures.push(format!(
                    "{ctx}: single-choice step (`multi: false`) must have exactly 1 correct option, found {}",
                    key_ids.len()
                ));
            }
            let opt_ids: BTreeSet<&str> = step.options.iter().map(|o| o.id.as_str()).collect();
            let mut seen_keys = BTreeSet::new();
            for kid in key_ids {
                if !seen_keys.insert(kid.as_str()) {
                    failures.push(format!("{ctx}: duplicate option id `{kid}` in choice key"));
                }
                if !opt_ids.contains(kid.as_str()) {
                    failures.push(format!(
                        "{ctx}: choice key references non-existent option `{kid}`"
                    ));
                }
            }
            if key_ids.len() >= step.options.len() && step.options.len() >= 2 && !step.multi {
                failures.push(format!("{ctx}: choice step has no distractor options"));
            }

            // Distractors (and all options) must have distinct normalized texts.
            let mut seen_texts = BTreeSet::new();
            for opt in &step.options {
                let norm = normalize_option_text(&opt.text);
                if norm.is_empty() {
                    failures.push(format!("{ctx}: option `{}` has empty text", opt.id));
                } else if !seen_texts.insert(norm) {
                    failures.push(format!(
                        "{ctx}: duplicate option/distractor text `{}`",
                        opt.text
                    ));
                }
            }
        }
        (StepKind::Order, AnswerKey::Order(key_ids)) => {
            if step.items.len() < 2 {
                failures.push(format!("{ctx}: order step needs at least 2 items"));
            }
            let item_ids: BTreeSet<&str> = step.items.iter().map(|i| i.id.as_str()).collect();
            let keyed_ids: BTreeSet<&str> = key_ids.iter().map(String::as_str).collect();
            if keyed_ids != item_ids || key_ids.len() != step.items.len() {
                failures.push(format!(
                    "{ctx}: order key must be an exact permutation of step.items"
                ));
            }
            let mut seen_texts = BTreeSet::new();
            for item in &step.items {
                let norm = normalize_option_text(&item.text);
                if norm.is_empty() {
                    failures.push(format!("{ctx}: order item `{}` has empty text", item.id));
                } else if !seen_texts.insert(norm) {
                    failures.push(format!("{ctx}: duplicate order item text `{}`", item.text));
                }
            }
        }
        (StepKind::Fill, AnswerKey::Text(accepted)) => {
            if accepted.is_empty() {
                failures.push(format!(
                    "{ctx}: fill key must list at least one accepted answer"
                ));
            }
            if !step.template.as_deref().unwrap_or("").contains("____") {
                failures.push(format!(
                    "{ctx}: fill template must contain `____` placeholder"
                ));
            }
            let mut seen_norm = BTreeSet::new();
            for ans in accepted {
                let norm = normalize(ans);
                if norm.is_empty() {
                    failures.push(format!("{ctx}: fill key contains an empty accepted answer"));
                } else if !seen_norm.insert(norm) {
                    failures.push(format!(
                        "{ctx}: fill key contains duplicate normalized answer `{ans}`"
                    ));
                }
            }
        }
        _ => {
            failures.push(format!(
                "{ctx}: answer key kind does not match step kind `{:?}`",
                step.kind
            ));
        }
    }
}

fn normalize_option_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn validate_rubric_fixture(rubric: &RubricFixture, is_philosophy: bool) -> Verdict {
    let mut failures = Vec::new();
    if rubric.criteria.is_empty() || rubric.criteria.iter().any(|c| c.trim().is_empty()) {
        failures.push(format!(
            "rubric for step `{}` must list at least one non-empty criterion",
            rubric.step_id
        ));
    }
    let phil = is_philosophy || rubric.step_id.to_ascii_lowercase().contains("phil");
    if phil
        && (rubric.not_penalised.is_empty()
            || rubric.not_penalised.iter().all(|s| s.trim().is_empty()))
    {
        failures.push(format!(
            "philosophy rubric for step `{}` must specify non-empty `not_penalised` protections",
            rubric.step_id
        ));
    }

    if !failures.is_empty() {
        return Verdict::Fail { reasons: failures };
    }

    let mut reasons: Vec<String> = rubric
        .criteria
        .iter()
        .map(|c| format!("rubric criterion: {c}"))
        .collect();
    if !rubric.not_penalised.is_empty() {
        reasons.push(format!(
            "not penalised: {}",
            rubric.not_penalised.join("; ")
        ));
    }
    Verdict::NeedsReview { reasons }
}

fn validate_step_key_matches_quantity(
    step: &Step,
    expected_q: super::units::Quantity,
    tol: f64,
    failures: &mut Vec<String>,
) {
    let Some(AnswerKey::Text(accepted)) = &step.key else {
        return;
    };
    // If any accepted answer parses as a physical quantity, verify that at least one matches
    // the expected quantity's dimension and value.
    let parsed_any: Vec<_> = accepted
        .iter()
        .filter_map(|a| parse_quantity(a).ok())
        .collect();
    if parsed_any.is_empty() {
        return;
    }
    let matches_one = parsed_any.iter().any(|q| {
        if q.dim != expected_q.dim {
            return false;
        }
        let diff = (q.value - expected_q.value).abs();
        let rel = if expected_q.value.abs() > 1e-12 {
            diff / expected_q.value.abs()
        } else {
            diff
        };
        rel <= tol.max(1e-9) + 1e-12
    });
    if !matches_one {
        failures.push(format!(
            "step `{}` AnswerKey::Text values {:?} do not match NumericFixture expected `{}` ({})",
            step.id, accepted, expected_q.value, expected_q.dim
        ));
    }
}

/// Checks whether `step` contains a formal propositional argument and, if so, verifies its
/// answer key against [`crate::courses::logic::check_argument`].
///
/// Returns `true` if a formal logic argument was found and checked.
fn validate_formal_logic_step(step: &Step, failures: &mut Vec<String>) -> bool {
    let mut candidate_texts = Vec::new();
    if let Some(tmpl) = &step.template {
        candidate_texts.push(tmpl.as_str());
    }
    if let Some(code) = &step.code {
        candidate_texts.push(code.as_str());
    }
    for block in &step.body {
        candidate_texts.push(block.text.as_str());
    }

    // Case 1: Fill step where `____` sits inside the conclusion of a formal argument.
    if step.kind == StepKind::Fill
        && let Some(tmpl) = &step.template
        && tmpl.contains("____")
        && let Some(AnswerKey::Text(accepted)) = &step.key
    {
        let mut checked_any = false;
        for ans in accepted {
            let filled = tmpl.replace("____", ans);
            if let Some((premises, conclusion)) = extract_formal_argument(&filled) {
                checked_any = true;
                let prem_refs: Vec<&str> = premises.iter().map(String::as_str).collect();
                match logic::check_argument(&prem_refs, &conclusion) {
                    Ok(validity) => {
                        if !validity.valid {
                            failures.push(format!(
                                "step `{}`: fill answer `{ans}` produces an invalid deduction for premises {:?} |- `{conclusion}` (counterexample: {:?})",
                                step.id, premises, validity.counterexample
                            ));
                        }
                    }
                    Err(err) => {
                        failures.push(format!(
                            "step `{}`: logic error with fill answer `{ans}`: {err}",
                            step.id
                        ));
                    }
                }
            }
        }
        if checked_any {
            return true;
        }
    }

    // Case 2: Choice / Predict / Fill step with a complete formal argument in its text.
    for text in candidate_texts {
        if let Some((premises, conclusion)) = extract_formal_argument(text) {
            let prem_refs: Vec<&str> = premises.iter().map(String::as_str).collect();
            let validity = match logic::check_argument(&prem_refs, &conclusion) {
                Ok(v) => v,
                Err(err) => {
                    failures.push(format!("step `{}`: formal logic error: {err}", step.id));
                    return true;
                }
            };

            if let Some(key_claims_valid) = step_key_validity_claim(step)
                && key_claims_valid != validity.valid
            {
                failures.push(format!(
                    "step `{}`: answer key claims argument is {}, but courses::logic proved it is {} (premises: {:?}, conclusion: `{conclusion}`, counterexample: {:?})",
                    step.id,
                    if key_claims_valid { "valid" } else { "invalid" },
                    if validity.valid { "valid" } else { "invalid" },
                    premises,
                    validity.counterexample
                ));
            }
            return true;
        }
    }

    false
}

/// Extracts `(premises, conclusion)` from labeled lines (`Premise 1: ...`, `Conclusion: ...`)
/// or turnstile notation (`P -> Q, P |- Q`) when every formula parses via [`logic::parse`].
pub fn extract_formal_argument(text: &str) -> Option<(Vec<String>, String)> {
    // 1. Labeled line format (`Premise ...: <formula>`, `Conclusion: <formula>`).
    let mut premises = Vec::new();
    let mut conclusion: Option<String> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim().trim_start_matches(['-', '*', '•']).trim();
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = strip_premise_prefix(&lower, line) {
            let f = rest.trim().trim_end_matches('.');
            if logic::parse(f).is_ok() {
                premises.push(f.to_string());
            } else {
                // Natural-language premise (e.g. "All moons are made of green cheese"): abort
                // formal extraction for this block so prose steps aren't misread.
                premises.clear();
                conclusion = None;
                break;
            }
        } else if let Some(rest) = strip_conclusion_prefix(&lower, line) {
            let f = rest.trim().trim_end_matches('.');
            if logic::parse(f).is_ok() {
                conclusion = Some(f.to_string());
            } else {
                premises.clear();
                conclusion = None;
                break;
            }
        }
    }

    if !premises.is_empty()
        && let Some(conc) = conclusion
    {
        return Some((premises, conc));
    }

    // 2. Turnstile format (`P -> Q, P |- Q`, `P -> Q, P |= Q`, `P -> Q; ~Q ⊢ ~P`).
    for sep in ["|-", "|=", "⊢", "⊨", "∴"] {
        if let Some((lhs, rhs)) = text.split_once(sep) {
            let cand_prems: Vec<String> = lhs
                .split([',', ';', '\n'])
                .map(|s| s.trim().trim_end_matches('.').to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let cand_conc = rhs.trim().trim_end_matches('.').to_string();
            if !cand_prems.is_empty()
                && !cand_conc.is_empty()
                && cand_prems.iter().all(|p| logic::parse(p).is_ok())
                && logic::parse(&cand_conc).is_ok()
            {
                return Some((cand_prems, cand_conc));
            }
        }
    }

    None
}

fn strip_premise_prefix<'a>(lower: &str, original: &'a str) -> Option<&'a str> {
    if lower.starts_with("premise") {
        let (_, rhs) = original.split_once(':')?;
        return Some(rhs);
    }
    if lower.starts_with('p')
        && let Some((lhs, rhs)) = original.split_once(':')
        && lhs.len() >= 2
        && lhs[1..].chars().all(|c| c.is_ascii_digit())
    {
        return Some(rhs);
    }
    None
}

fn strip_conclusion_prefix<'a>(lower: &str, original: &'a str) -> Option<&'a str> {
    if lower.starts_with("conclusion") || lower.starts_with("therefore") {
        let (_, rhs) = original.split_once(':')?;
        return Some(rhs);
    }
    if (lower.starts_with("c:") || lower.starts_with("∴"))
        && let Some((_, rhs)) = original.split_once(':')
    {
        return Some(rhs);
    }
    None
}

/// Determines whether `step`'s answer key claims the formal argument is `valid` (`Some(true)`)
/// or `invalid` (`Some(false)`).
fn step_key_validity_claim(step: &Step) -> Option<bool> {
    let key = step.key.as_ref()?;
    match key {
        AnswerKey::Choice(ids) => {
            for id in ids {
                if let Some(claim) = classify_validity_text(id) {
                    return Some(claim);
                }
                if let Some(opt) = step.options.iter().find(|o| &o.id == id)
                    && let Some(claim) = classify_validity_text(&opt.text)
                {
                    return Some(claim);
                }
            }
            None
        }
        AnswerKey::Text(accepted) => {
            for text in accepted {
                if let Some(claim) = classify_validity_text(text) {
                    return Some(claim);
                }
            }
            None
        }
        _ => None,
    }
}

fn classify_validity_text(text: &str) -> Option<bool> {
    let lower = text.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .filter(|s| !s.is_empty())
        .collect();

    // Check negative / fallacy markers before positive markers.
    if words
        .iter()
        .any(|&w| matches!(w, "invalid" | "fallacy" | "fallacious" | "ac" | "da"))
        || lower.contains("affirming the consequent")
        || lower.contains("denying the antecedent")
    {
        return Some(false);
    }

    if words
        .iter()
        .any(|&w| matches!(w, "valid" | "valid-sound" | "valid-unsound" | "mp" | "mt"))
        || lower.contains("modus ponens")
        || lower.contains("modus tollens")
        || lower.contains("disjunctive syllogism")
        || lower.contains("hypothetical syllogism")
    {
        return Some(true);
    }

    None
}

fn is_philosophy_context(course_id: &str, item_id: &str, objective_ids: &[String]) -> bool {
    let check_str = |s: &str| {
        let l = s.to_ascii_lowercase();
        l.contains("phil") || l.contains("logic") || l.contains("ethics") || l.contains("epistem")
    };
    check_str(course_id) || check_str(item_id) || objective_ids.iter().any(|o| check_str(o))
}
