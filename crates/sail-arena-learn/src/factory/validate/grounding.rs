//! Grounding validator (`CheckKind::Grounding`): verifies that every factual claim in a
//! lesson draft resolves to a clean fragment from `draft.built_from` or a deterministically
//! checked derivation, and that every `Explain` step is covered by at least one claim.

use super::make_receipt;
use super::units::{Quantity, check_numeric_fixture, evaluate_formula, parse_quantity};
use crate::courses::content::{Step, StepKind};
use crate::courses::logic;
use crate::factory::contracts::{
    CheckKind, Claim, DraftValidator, FragmentLookup, IngestionStatus, LessonDraft, QualityStatus,
    ValidationContext, ValidationReceipt, Verdict,
};
use std::collections::{BTreeMap, BTreeSet};

/// Version tag stamped on receipts produced by [`GroundingValidator`].
pub const VALIDATOR_VERSION: &str = "grounding/1";

/// Maximum number of claims inspected per draft.
pub const MAX_CLAIMS_CHECKED: usize = 128;

/// Maximum number of fragment citations inspected per claim.
pub const MAX_FRAGMENTS_PER_CLAIM: usize = 32;

/// Validates source grounding of claims and explain steps against [`FragmentLookup`].
#[derive(Debug, Default, Clone, Copy)]
pub struct GroundingValidator;

#[async_trait::async_trait]
impl DraftValidator for GroundingValidator {
    fn check(&self) -> CheckKind {
        CheckKind::Grounding
    }

    async fn validate(
        &self,
        draft: &LessonDraft,
        ctx: &ValidationContext<'_>,
    ) -> Vec<ValidationReceipt> {
        let verdict = validate_grounding(draft, ctx.fragments);
        vec![make_receipt(
            draft,
            CheckKind::Grounding,
            None,
            verdict,
            VALIDATOR_VERSION,
            ctx.now_ms,
        )]
    }
}

/// Evaluates grounding rules on `draft` using `fragments`, returning a [`Verdict`].
pub fn validate_grounding(draft: &LessonDraft, fragments: &dyn FragmentLookup) -> Verdict {
    let mut failures = Vec::new();
    let mut reviews = Vec::new();

    if draft.output.claims.len() > MAX_CLAIMS_CHECKED {
        failures.push(format!(
            "draft has {} claims, exceeding cap of {MAX_CLAIMS_CHECKED}",
            draft.output.claims.len()
        ));
    }

    let mut supported_claims: Vec<&Claim> = Vec::new();

    for (idx, claim) in draft
        .output
        .claims
        .iter()
        .take(MAX_CLAIMS_CHECKED)
        .enumerate()
    {
        let label = format!("claim[{idx}]");
        if claim.text.trim().is_empty() {
            failures.push(format!("{label}: claim text cannot be empty"));
            continue;
        }
        if claim.fragment_ids.len() > MAX_FRAGMENTS_PER_CLAIM {
            failures.push(format!(
                "{label}: cites {} fragments, exceeding cap of {MAX_FRAGMENTS_PER_CLAIM}",
                claim.fragment_ids.len()
            ));
            continue;
        }

        let has_fragments = !claim.fragment_ids.is_empty();
        let derivation = claim
            .derivation
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        if !has_fragments && derivation.is_none() {
            reviews.push(format!(
                "{label} (`{}`): claim has no fragment citations and no derivation",
                truncate_label(&claim.text, 48)
            ));
            continue;
        }

        let mut claim_ok = true;

        for frag_id in &claim.fragment_ids {
            if let Err(err) = verify_cited_fragment(frag_id, draft, fragments) {
                failures.push(format!("{label}: {err}"));
                claim_ok = false;
            }
        }

        if let Some(deriv) = derivation {
            match verify_claim_derivation(deriv, draft) {
                DerivationCheck::Verified => {}
                DerivationCheck::Invalid(reason) => {
                    failures.push(format!("{label}: invalid derivation `{deriv}`: {reason}"));
                    claim_ok = false;
                }
                DerivationCheck::Unverifiable(reason) => {
                    if !has_fragments {
                        reviews.push(format!(
                            "{label}: derivation `{deriv}` requires review: {reason}"
                        ));
                        claim_ok = false;
                    }
                }
            }
        }

        if claim_ok {
            supported_claims.push(claim);
        }
    }

    // Check that every Explain step has at least one claim covering its body.
    for step in &draft.output.lesson.steps {
        if step.kind == StepKind::Explain && !explain_step_covered(step, &supported_claims) {
            reviews.push(format!(
                "explain step `{}` (`{}`) has no supported claim covering its body",
                step.id, step.title
            ));
        }
    }

    if draft.output.claims.is_empty() && reviews.is_empty() && failures.is_empty() {
        reviews.push("draft contains no grounding claims".to_string());
    }

    if !failures.is_empty() {
        Verdict::Fail { reasons: failures }
    } else if !reviews.is_empty() {
        Verdict::NeedsReview { reasons: reviews }
    } else {
        Verdict::Pass
    }
}

fn verify_cited_fragment(
    frag_id: &str,
    draft: &LessonDraft,
    fragments: &dyn FragmentLookup,
) -> Result<(), String> {
    let Some(fragment) = fragments.fragment(frag_id) else {
        return Err(format!("cited fragment `{frag_id}` does not exist"));
    };

    let Some(built_anchor) = draft.built_from.iter().find(|b| {
        b.source_id == fragment.source_id && b.source_revision == fragment.source_revision
    }) else {
        return Err(format!(
            "cited fragment `{frag_id}` comes from `{}@{}` which is not in draft.built_from",
            fragment.source_id, fragment.source_revision
        ));
    };

    if built_anchor.content_hash.trim().is_empty() || fragment.content_hash.trim().is_empty() {
        return Err(format!(
            "cited fragment `{frag_id}` or built_from anchor has an empty content_hash"
        ));
    }

    if let Some(source) = fragments.source(&fragment.source_id) {
        if matches!(
            source.status,
            IngestionStatus::Removed | IngestionStatus::Failed { .. }
        ) {
            return Err(format!(
                "source `{}` for fragment `{frag_id}` is in status `{:?}`",
                source.source_id, source.status
            ));
        }
        if source.revision != built_anchor.source_revision {
            return Err(format!(
                "stale source revision for `{}`: draft built from rev {}, current rev is {}",
                source.source_id, built_anchor.source_revision, source.revision
            ));
        }
        if source.content_hash != built_anchor.content_hash {
            return Err(format!(
                "stale content_hash for source `{}`: draft built from `{}`, current source hash is `{}`",
                source.source_id, built_anchor.content_hash, source.content_hash
            ));
        }
    } else if fragment.content_hash != built_anchor.content_hash {
        return Err(format!(
            "stale content_hash for fragment `{frag_id}`: draft built from `{}`, fragment hash is `{}`",
            built_anchor.content_hash, fragment.content_hash
        ));
    }

    if let QualityStatus::NeedsValidation { reason } = &fragment.quality {
        return Err(format!(
            "cited fragment `{frag_id}` has quality NeedsValidation ({reason})"
        ));
    }

    Ok(())
}

/// Outcome of deterministically checking a [`Claim::derivation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DerivationCheck {
    /// Deterministic subject validator confirmed the derivation.
    Verified,
    /// Deterministic validator proved the derivation is false or dimensionally inconsistent.
    Invalid(String),
    /// Derivation could not be parsed by a deterministic validator; owner review required.
    Unverifiable(String),
}

/// Re-checks a claim's `derivation` string using the formal logic checker and unit-aware
/// expression evaluator.
pub fn verify_claim_derivation(derivation: &str, draft: &LessonDraft) -> DerivationCheck {
    let trimmed = derivation.trim();
    if trimmed.is_empty() {
        return DerivationCheck::Unverifiable("empty derivation".to_string());
    }

    // 1. Check if the derivation matches a NumericFixture in the draft.
    for num in &draft.output.numeric {
        if num.formula.trim() == trimmed || num.step_id == trimmed {
            return match check_numeric_fixture(num) {
                Ok(_) => DerivationCheck::Verified,
                Err(e) => DerivationCheck::Invalid(e.to_string()),
            };
        }
    }

    // 2. Check if the derivation is a propositional logic entailment (`P -> Q, P |- Q` or `|=`).
    for sep in ["|-", "|=", "⊢", "⊨", "therefore", "∴"] {
        if let Some((lhs, rhs)) = trimmed.split_once(sep) {
            let premises: Vec<&str> = lhs
                .split([',', ';'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            let conclusion = rhs.trim();
            if !premises.is_empty()
                && !conclusion.is_empty()
                && let Ok(validity) = logic::check_argument(&premises, conclusion)
            {
                return if validity.valid {
                    DerivationCheck::Verified
                } else {
                    DerivationCheck::Invalid(format!(
                        "argument `{trimmed}` is logically invalid (counterexample: {:?})",
                        validity.counterexample
                    ))
                };
            }
        }
    }

    // 3. Check if the derivation is a unit-aware equation `LHS = RHS` (not `<=>` or `=>`).
    if !trimmed.contains("->")
        && !trimmed.contains("=>")
        && let Some((lhs, rhs)) = trimmed.split_once('=')
        && !rhs.contains('=')
    {
        let knowns = collect_known_quantities(draft);
        match (
            evaluate_formula(lhs.trim(), &knowns),
            evaluate_formula(rhs.trim(), &knowns),
        ) {
            (Ok(left_q), Ok(right_q)) => {
                if left_q.dim != right_q.dim {
                    return DerivationCheck::Invalid(format!(
                        "dimension mismatch in `{trimmed}`: left is {}, right is {}",
                        left_q.dim, right_q.dim
                    ));
                }
                let diff = (left_q.value - right_q.value).abs();
                let scale = left_q.value.abs().max(right_q.value.abs()).max(1e-12);
                if diff / scale <= 1e-3 {
                    return DerivationCheck::Verified;
                }
                return DerivationCheck::Invalid(format!(
                    "numeric mismatch in `{trimmed}`: {} != {}",
                    left_q.value, right_q.value
                ));
            }
            (Err(e), _) | (_, Err(e)) => {
                if matches!(e, super::units::UnitError::DimensionMismatch { .. }) {
                    return DerivationCheck::Invalid(e.to_string());
                }
            }
        }
    }

    // 4. Check if the derivation is a single propositional logic tautology.
    if let Ok(formula) = logic::parse(trimmed) {
        let table = logic::truth_table(&formula);
        if table.iter().all(|(_, val)| *val) {
            return DerivationCheck::Verified;
        }
        return DerivationCheck::Invalid(format!("formula `{trimmed}` is not a tautology"));
    }

    DerivationCheck::Unverifiable(
        "not a machine-checkable logic entailment or unit equation".to_string(),
    )
}

fn collect_known_quantities(draft: &LessonDraft) -> BTreeMap<String, Quantity> {
    let mut out = BTreeMap::new();
    for num in &draft.output.numeric {
        for (k, v) in &num.knowns {
            if let Ok(q) = parse_quantity(v) {
                out.insert(k.clone(), q);
            }
        }
    }
    out
}

fn explain_step_covered(step: &Step, claims: &[&Claim]) -> bool {
    if claims.is_empty() {
        return false;
    }
    let body_text: String = step
        .body
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if body_text.trim().is_empty() {
        return false;
    }

    let step_words = content_words(&format!("{} {}", step.title, body_text));
    if step_words.is_empty() {
        return false;
    }

    let step_id_lower = step.id.to_ascii_lowercase();
    for claim in claims {
        let claim_lower = claim.text.to_ascii_lowercase();
        if !step_id_lower.is_empty() && claim_lower.contains(&step_id_lower) {
            return true;
        }
        let claim_words = content_words(&claim.text);
        if shares_content_term(&step_words, &claim_words) {
            return true;
        }
    }
    false
}

fn content_words(text: &str) -> BTreeSet<String> {
    const STOPWORDS: &[&str] = &[
        "about", "after", "again", "also", "always", "among", "because", "been", "before", "being",
        "between", "both", "cannot", "could", "does", "doing", "done", "during", "each", "either",
        "every", "first", "from", "have", "having", "here", "into", "itself", "just", "make",
        "makes", "many", "more", "most", "much", "must", "never", "only", "other", "over", "same",
        "should", "since", "some", "such", "than", "that", "their", "them", "then", "there",
        "these", "they", "this", "those", "through", "under", "until", "very", "what", "when",
        "where", "which", "while", "with", "within", "without", "would", "your",
    ];
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4)
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect()
}

fn shares_content_term(a: &BTreeSet<String>, b: &BTreeSet<String>) -> bool {
    for wa in a {
        if b.contains(wa) {
            return true;
        }
        if wa.len() >= 5 {
            let stem = &wa[..5];
            if b.iter().any(|wb| wb.len() >= 5 && wb.starts_with(stem)) {
                return true;
            }
        }
    }
    false
}

fn truncate_label(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}
