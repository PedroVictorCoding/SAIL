//! Course Factory validation pipeline (`factory::validate`, M2/M5).
//!
//! Every check implements [`crate::factory::contracts::DraftValidator`] and emits
//! [`ValidationReceipt`]s bound to `draft.output_hash` and stamped with `validator_version`.
//!
//! Stages:
//! - [`schema::SchemaValidator`] (`CheckKind::Schema`): `CoursePackage::validate`, unique IDs,
//!   fixture linkage, length bounds, and executable-markup rejection.
//! - [`grounding::GroundingValidator`] (`CheckKind::Grounding`): fragment resolution against
//!   `draft.built_from`, content-hash freshness, quality checks, derivation re-verification,
//!   and explain-step claim coverage.
//! - [`copyright::CopyrightValidator`] (`CheckKind::Copyright`): Suffix-Automaton longest
//!   common normalized word run against [`SourceRights::max_quote_words`].
//! - [`subject::SubjectValidator`] (`CheckKind::Subject`): `ExactKey`, `FormalLogic`
//!   (via [`crate::courses::logic`]), `Math`/`Physics` (via [`units`]), and `Rubric`.
//! - [`exercise::ExerciseTestsValidator`] (`CheckKind::ExerciseTests`): sandboxed compilation
//!   and execution of `reference_solution` and `known_bad` against `hidden_tests`.
//! - [`quality::QualityValidator`] (`CheckKind::Quality`): deterministic pedagogical report
//!   covering objectives, step mix, transfer step presence, 3–8 min duration, and hint ladders.

pub mod copyright;
pub mod exercise;
pub mod grounding;
pub mod quality;
pub mod schema;
pub mod subject;
pub mod units;

pub use copyright::{CopyrightValidator, longest_common_word_run, normalize_words};
pub use exercise::{
    ExerciseTestsValidator, compose_rust_test_program, extract_hidden_test_names,
    validate_rust_fixture_static,
};
pub use grounding::{DerivationCheck, GroundingValidator, verify_claim_derivation};
pub use quality::{QualityReport, QualityValidator, evaluate_quality};
pub use schema::{SchemaValidator, detect_executable_markup, validate_schema};
pub use subject::{SubjectValidator, extract_formal_argument, validate_subject};
pub use units::{
    Dimension, Quantity, UnitError, check_numeric_fixture, evaluate_formula, parse_quantity,
    resolve_unit,
};

use crate::courses::engine::RustEvaluator;
use crate::factory::contracts::{
    CheckKind, DraftValidator, LessonDraft, Stakes, ValidationContext, ValidationReceipt, Verdict,
    sha256_hex,
};
use rusqlite::{Connection, params};
use std::sync::Arc;

/// Every check required before a [`LessonDraft`] may be published.
pub const REQUIRED_CHECKS: [CheckKind; 6] = [
    CheckKind::Schema,
    CheckKind::Grounding,
    CheckKind::Copyright,
    CheckKind::Subject,
    CheckKind::ExerciseTests,
    CheckKind::Quality,
];

/// Constructs a deterministic [`ValidationReceipt`] bound to `draft.draft_id` and
/// `draft.output_hash`.
pub fn make_receipt(
    draft: &LessonDraft,
    check: CheckKind,
    step_id: Option<String>,
    verdict: Verdict,
    validator_version: &str,
    at_ms: i64,
) -> ValidationReceipt {
    let check_tag = check_kind_slug(check);
    let step_part = step_id.as_deref().unwrap_or("_lesson_");
    let seed = format!(
        "{}/{}/{}/{}/{}",
        draft.draft_id, draft.output_hash, check_tag, step_part, validator_version
    );
    let digest = sha256_hex(seed.as_bytes());
    let short_hash = &digest[..16];
    let receipt_id = match &step_id {
        Some(sid) => format!("rcpt-{check_tag}-{sid}-{short_hash}"),
        None => format!("rcpt-{check_tag}-{short_hash}"),
    };
    ValidationReceipt {
        receipt_id,
        draft_id: draft.draft_id.clone(),
        output_hash: draft.output_hash.clone(),
        check,
        step_id,
        verdict,
        validator_version: validator_version.to_string(),
        at_ms,
    }
}

fn check_kind_slug(check: CheckKind) -> &'static str {
    match check {
        CheckKind::Schema => "schema",
        CheckKind::Grounding => "grounding",
        CheckKind::Copyright => "copyright",
        CheckKind::Subject => "subject",
        CheckKind::ExerciseTests => "exercise_tests",
        CheckKind::Quality => "quality",
    }
}

/// Verifies whether `draft` may be published under `stakes` given `receipts`.
///
/// Rules:
/// 1. Every check in [`REQUIRED_CHECKS`] must have at least one [`Verdict::Pass`] receipt for
///    `draft.draft_id` and the **current** `draft.output_hash`. Receipts for an older
///    `output_hash` never count.
/// 2. Any [`Verdict::Fail`] or [`Verdict::Unavailable`] receipt for the current `output_hash`
///    blocks publication.
/// 3. [`Stakes::Assessment`] additionally forbids any [`Verdict::NeedsReview`] receipt.
pub fn publishable(
    receipts: &[ValidationReceipt],
    draft: &LessonDraft,
    stakes: Stakes,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();

    if draft.output_hash.trim().is_empty() {
        errors.push("draft output_hash cannot be empty".to_string());
        return Err(errors);
    }

    for check in REQUIRED_CHECKS {
        let slug = check_kind_slug(check);
        let current: Vec<&ValidationReceipt> = receipts
            .iter()
            .filter(|r| {
                r.draft_id == draft.draft_id
                    && r.output_hash == draft.output_hash
                    && r.check == check
            })
            .collect();

        if current.is_empty() {
            let has_stale = receipts.iter().any(|r| {
                r.draft_id == draft.draft_id
                    && r.check == check
                    && r.output_hash != draft.output_hash
            });
            if has_stale {
                errors.push(format!(
                    "check `{slug}` only has receipts for a stale output_hash (current is `{}`)",
                    draft.output_hash
                ));
            } else {
                errors.push(format!(
                    "missing receipt for required check `{slug}` on output_hash `{}`",
                    draft.output_hash
                ));
            }
            continue;
        }

        for r in &current {
            let target = r
                .step_id
                .as_deref()
                .map(|s| format!("{slug} (step `{s}`)"))
                .unwrap_or_else(|| slug.to_string());

            match &r.verdict {
                Verdict::Pass => {}
                Verdict::Fail { reasons } => {
                    errors.push(format!("check `{target}` failed: {}", reasons.join("; ")));
                }
                Verdict::Unavailable { reason } => {
                    errors.push(format!(
                        "check `{target}` was unavailable ({reason}); unavailable is never a pass"
                    ));
                }
                Verdict::NeedsReview { reasons } => {
                    if stakes == Stakes::Assessment {
                        errors.push(format!(
                            "Assessment stakes forbid NeedsReview on `{target}`: {}",
                            reasons.join("; ")
                        ));
                    }
                }
            }
        }

        let has_pass = current.iter().any(|r| r.verdict == Verdict::Pass);
        if !has_pass {
            errors.push(format!(
                "required check `{slug}` has no Pass receipt for current output_hash `{}`",
                draft.output_hash
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Runs all six Course Factory validation checks in pipeline order.
pub struct ValidationSuite {
    /// Stage 1: schema, bounds, and executable-markup validator.
    pub schema: SchemaValidator,
    /// Stage 2: source grounding and derivation validator.
    pub grounding: GroundingValidator,
    /// Stage 3: verbatim quote-length copyright validator.
    pub copyright: CopyrightValidator,
    /// Stage 4: deterministic subject validator (`ExactKey`, `FormalLogic`, `Math`/`Physics`, `Rubric`).
    pub subject: SubjectValidator,
    /// Stage 5: sandboxed Rust exercise-tests validator.
    pub exercise_tests: ExerciseTestsValidator,
    /// Stage 6: deterministic pedagogical quality validator.
    pub quality: QualityValidator,
}

impl ValidationSuite {
    /// Creates a complete [`ValidationSuite`] using `rust` for sandboxed Rust exercise checks.
    pub fn new(rust: Arc<dyn RustEvaluator>) -> Self {
        Self {
            schema: SchemaValidator,
            grounding: GroundingValidator,
            copyright: CopyrightValidator,
            subject: SubjectValidator,
            exercise_tests: ExerciseTestsValidator::new(rust),
            quality: QualityValidator,
        }
    }

    /// Executes all six validators in order and collects their [`ValidationReceipt`]s.
    pub async fn validate_all(
        &self,
        draft: &LessonDraft,
        ctx: &ValidationContext<'_>,
    ) -> Vec<ValidationReceipt> {
        let mut out = Vec::new();
        out.extend(self.schema.validate(draft, ctx).await);
        out.extend(self.grounding.validate(draft, ctx).await);
        out.extend(self.copyright.validate(draft, ctx).await);
        out.extend(self.subject.validate(draft, ctx).await);
        out.extend(self.exercise_tests.validate(draft, ctx).await);
        out.extend(self.quality.validate(draft, ctx).await);
        out
    }
}

/// Error returned by [`ReceiptStore`] SQLite operations.
#[derive(Debug, thiserror::Error)]
#[error("validate receipt store: {0}")]
pub struct ReceiptStoreError(pub String);

fn store_err(e: impl std::fmt::Display) -> ReceiptStoreError {
    ReceiptStoreError(e.to_string())
}

/// SQLite persistence helper for [`ValidationReceipt`]s over `learn_factory_validate_receipts`.
pub struct ReceiptStore;

impl ReceiptStore {
    /// Creates the `learn_factory_validate_receipts` table and indexes if they do not exist.
    pub fn init_tables(conn: &Connection) -> Result<(), ReceiptStoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS learn_factory_validate_receipts (
                 receipt_id TEXT PRIMARY KEY,
                 draft_id TEXT NOT NULL,
                 output_hash TEXT NOT NULL,
                 check_kind TEXT NOT NULL,
                 step_id TEXT,
                 validator_version TEXT NOT NULL,
                 at_ms INTEGER NOT NULL,
                 receipt_json TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_learn_factory_validate_draft_hash
                 ON learn_factory_validate_receipts (draft_id, output_hash);",
        )
        .map_err(store_err)
    }

    /// Upserts a batch of [`ValidationReceipt`]s into `learn_factory_validate_receipts`.
    pub fn save_receipts(
        conn: &Connection,
        receipts: &[ValidationReceipt],
    ) -> Result<(), ReceiptStoreError> {
        for r in receipts {
            let check_str = check_kind_slug(r.check);
            let json = serde_json::to_string(r).map_err(store_err)?;
            conn.execute(
                "INSERT OR REPLACE INTO learn_factory_validate_receipts
                 (receipt_id, draft_id, output_hash, check_kind, step_id, validator_version, at_ms, receipt_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    r.receipt_id,
                    r.draft_id,
                    r.output_hash,
                    check_str,
                    r.step_id,
                    r.validator_version,
                    r.at_ms,
                    json
                ],
            )
            .map_err(store_err)?;
        }
        Ok(())
    }

    /// Loads all stored [`ValidationReceipt`]s for `draft_id`, ordered by timestamp and ID.
    pub fn for_draft(
        conn: &Connection,
        draft_id: &str,
    ) -> Result<Vec<ValidationReceipt>, ReceiptStoreError> {
        let mut stmt = conn
            .prepare(
                "SELECT receipt_json FROM learn_factory_validate_receipts
                 WHERE draft_id = ?1
                 ORDER BY at_ms ASC, receipt_id ASC",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map([draft_id], |row| row.get::<_, String>(0))
            .map_err(store_err)?;
        let mut out = Vec::new();
        for row in rows {
            let text = row.map_err(store_err)?;
            let receipt: ValidationReceipt = serde_json::from_str(&text).map_err(store_err)?;
            out.push(receipt);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
