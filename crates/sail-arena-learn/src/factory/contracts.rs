//! Course Factory contracts (M0). Every stage of the pipeline reads and writes these types,
//! so each stage can be built, stored and tested on its own:
//!
//! ```text
//! sources → fragments → concept graph → curriculum plan → authoring jobs
//!         → lesson drafts → validation receipts → package revision → CourseEngine
//! ```
//!
//! Rules that hold across the module:
//! - Model output is **data**. It is parsed into [`LessonDraft`] / [`ConceptProposal`] /
//!   [`CurriculumProposal`] with `deny_unknown_fields` and never carries a receipt, a
//!   publish decision or a score. Receipts are created only by server validators.
//! - Source text is **untrusted data**. Nothing in a fragment can change policy, job state
//!   or publication.
//! - The objective knowledge graph ([`ConceptGraph`]) never holds learner competence.
//!   Competence stays in `courses::evidence`.
//! - Every stored record names the revision of the record it was built from, so a source
//!   change can find everything that depended on it ([`Invalidation`]).
use crate::courses::content::{Domain, Lesson, Step};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type SourceId = String;
pub type FragmentId = String;
pub type ConceptId = String;
pub type ObjectiveId = String;
pub type CourseId = String;
pub type DraftId = String;
pub type ReceiptId = String;

/// Hex SHA-256 of the exact bytes or text it describes.
pub type ContentHash = String;

/// The one hash function every factory record uses: lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> ContentHash {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ------------------------------------------------------------------------------- sources

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    /// PDF with a text layer.
    PdfText,
    /// PDF without a usable text layer; text comes from the vision/OCR pipeline and every
    /// fragment starts as [`QualityStatus::NeedsValidation`].
    PdfScanned,
    /// DRM-free EPUB. A DRM-protected file is refused, never decrypted.
    Epub,
    Markdown,
    Text,
}

/// What the owner may do with a source. Set by the owner at import; never inferred from the
/// file's own text (a PDF saying "public domain" is a claim, not a licence).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SourceRights {
    /// The owner's own material; processing and private derivatives are fine.
    OwnerPrivate,
    /// Public domain or an open licence named by SPDX id (e.g. `MIT OR Apache-2.0`).
    Open { license: String },
    /// Copyrighted, licensed for the owner's private study only.
    Restricted,
    /// Not stated. Treated exactly like `Restricted`.
    Unknown,
}

impl SourceRights {
    /// Longest verbatim run (in words) a published lesson may quote from this source.
    /// Restricted material is used for grounding and original writing, not reproduction.
    pub fn max_quote_words(&self) -> usize {
        match self {
            Self::OwnerPrivate | Self::Open { .. } => 200,
            Self::Restricted | Self::Unknown => 25,
        }
    }

    /// Whether derived packages may leave this machine (export, sharing). Restricted and
    /// unknown sources keep their derivatives local.
    pub fn exportable(&self) -> bool {
        matches!(self, Self::OwnerPrivate | Self::Open { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum IngestionStatus {
    Queued,
    Extracting,
    Indexed,
    /// Indexed, but parts need the owner (e.g. scanned formulas, unreadable pages).
    NeedsReview {
        reason: String,
    },
    Failed {
        reason: String,
    },
    /// Removed or replaced; dependants are invalidated, history is kept.
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EducationalSource {
    pub source_id: SourceId,
    pub title: String,
    pub authors: Vec<String>,
    pub source_type: SourceType,
    /// SHA-256 of the imported bytes. Re-importing identical bytes is a no-op.
    pub content_hash: ContentHash,
    /// Starts at 1; bumps when the owner replaces the file under the same source id.
    pub revision: u64,
    /// BCP 47 tag, e.g. `en`.
    pub language: String,
    /// Arena memory scope the source belongs to, e.g. `learning:rust`.
    pub access_scope: String,
    pub rights: SourceRights,
    pub status: IngestionStatus,
    pub imported_ms: i64,
}

/// Where a fragment sits in its source. Page numbers are the printed page when the PDF
/// has page labels, else the 1-based physical page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SourceLocation {
    Page {
        page: String,
        physical: u32,
    },
    PageRange {
        first: String,
        last: String,
        physical_first: u32,
        physical_last: u32,
    },
    /// EPUB spine item plus an element id or a block index inside it.
    Epub {
        href: String,
        anchor: String,
    },
    /// Markdown / text: 1-based inclusive line range.
    Lines {
        first: u32,
        last: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FragmentKind {
    Heading,
    Prose,
    Definition,
    Theorem,
    Example,
    Exercise,
    Solution,
    Formula,
    Figure,
    Code,
    Bibliography,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum QualityStatus {
    Clean,
    /// Usable, but the extractor isn't sure (e.g. a heading guessed from font size).
    LowConfidence {
        reason: String,
    },
    /// Must not ground a published claim until checked (e.g. a formula read from a scan).
    NeedsValidation {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFragment {
    /// Stable across re-extraction of the same revision: derived from source id, revision,
    /// location and content hash.
    pub fragment_id: FragmentId,
    pub source_id: SourceId,
    pub source_revision: u64,
    pub kind: FragmentKind,
    /// Chapter → section → subsection titles, outermost first.
    pub chapter_path: Vec<String>,
    pub location: SourceLocation,
    /// Plain text. Formulas keep their structure as text (LaTeX or the source's notation).
    pub text: String,
    pub content_hash: ContentHash,
    pub quality: QualityStatus,
}

/// A citation from generated content back to source text. It resolves only when the
/// fragment exists at that revision and its hash still matches.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAnchor {
    pub source_id: SourceId,
    pub source_revision: u64,
    pub fragment_id: FragmentId,
    pub content_hash: ContentHash,
}

/// Read access to stored fragments, for validators and the authoring context builder.
pub trait FragmentLookup: Send + Sync {
    fn source(&self, id: &str) -> Option<EducationalSource>;
    fn fragment(&self, id: &str) -> Option<SourceFragment>;
}

// ----------------------------------------------------------------- concepts and curriculum

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeStatus {
    /// Inferred by a model or from chapter order; not yet checked.
    Proposed,
    /// Checked against definitions/examples (a cited use of the prerequisite).
    Supported,
    OwnerConfirmed,
    OwnerRejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrerequisiteEdge {
    pub requires: ConceptId,
    pub status: EdgeStatus,
    /// Why the edge exists, in one sentence, with the fragments that show it.
    pub rationale: String,
    pub anchors: Vec<SourceAnchor>,
}

/// Which deterministic validator grades this concept's exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidatorKind {
    /// Choice / order / fill with an exact key.
    ExactKey,
    /// Sandboxed compiler plus tests.
    RustCompiler,
    /// `courses::logic` validity and truth tables.
    FormalLogic,
    /// Numeric/symbolic equivalence within a tolerance.
    Math,
    /// Units, dimensions and numeric tolerance.
    Physics,
    /// Structure checks plus a rubric; never graded with false certainty.
    Rubric,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Concept {
    /// Stable, dotted, e.g. `rust.ownership.move`. Never reused for a different idea.
    pub id: ConceptId,
    pub name: String,
    pub description: String,
    pub objectives: Vec<ObjectiveSpec>,
    pub prerequisites: Vec<PrerequisiteEdge>,
    pub anchors: Vec<SourceAnchor>,
    /// 1 (intro) … 5 (graduate).
    pub difficulty: u8,
    pub exercise_families: Vec<String>,
    pub validator: ValidatorKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectiveSpec {
    pub id: ObjectiveId,
    /// "The learner can …" — observable, assessable.
    pub statement: String,
}

/// The objective knowledge map of one course, versioned. Not learner state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConceptGraph {
    pub course_id: CourseId,
    pub revision: u64,
    pub built_from: Vec<SourceAnchorSet>,
    pub concepts: Vec<Concept>,
}

/// The source revisions a record was built from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceAnchorSet {
    pub source_id: SourceId,
    pub source_revision: u64,
    pub content_hash: ContentHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Proposed,
    OwnerApproved,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurriculumPlan {
    pub course_id: CourseId,
    pub revision: u64,
    pub concept_graph_revision: u64,
    pub title: String,
    pub domain: Domain,
    /// e.g. "Second-year undergraduate".
    pub level: String,
    pub scope: String,
    pub outcomes: Vec<String>,
    pub units: Vec<UnitPlan>,
    /// Places where sources disagree, recorded rather than merged silently.
    pub conflicts: Vec<SourceConflict>,
    pub status: PlanStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitPlan {
    pub id: String,
    pub title: String,
    pub objective_ids: Vec<ObjectiveId>,
    pub lessons: Vec<LessonPlan>,
    /// Review checkpoint at the end of the unit.
    pub review_checkpoint: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonPlan {
    pub id: String,
    pub title: String,
    pub objective_ids: Vec<ObjectiveId>,
    /// Active minutes, 3–8 for ordinary lessons.
    pub minutes: u32,
    pub kind: LessonPlanKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonPlanKind {
    Lesson,
    Lab,
    MidCourseAssessment,
    FinalAssessment,
    Capstone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceConflict {
    pub concept_id: ConceptId,
    pub summary: String,
    pub anchors: Vec<SourceAnchor>,
}

// -------------------------------------------------------------------- model output shapes

/// What the concept-extraction model call returns for one bounded batch of fragments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConceptProposal {
    pub concepts: Vec<ProposedConcept>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedConcept {
    pub id: ConceptId,
    pub name: String,
    pub description: String,
    pub objectives: Vec<String>,
    /// Concept ids with a one-sentence reason each.
    #[serde(default)]
    pub prerequisites: BTreeMap<ConceptId, String>,
    /// Fragment ids from the batch; resolved to anchors server-side.
    pub fragment_ids: Vec<FragmentId>,
    pub difficulty: u8,
    pub validator: ValidatorKind,
}

/// What the curriculum-planning call returns. The server assigns revision and status.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurriculumProposal {
    pub title: String,
    pub level: String,
    pub scope: String,
    pub outcomes: Vec<String>,
    pub units: Vec<UnitPlan>,
    #[serde(default)]
    pub conflicts: Vec<SourceConflict>,
}

/// One factual claim in a draft and what supports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub text: String,
    /// Fragment ids; the server turns them into [`SourceAnchor`]s and rejects unknown ones.
    #[serde(default)]
    pub fragment_ids: Vec<FragmentId>,
    /// Instead of a citation: a derivation a deterministic validator re-checks.
    #[serde(default)]
    pub derivation: Option<String>,
}

/// Printed by the generated test `main` after every hidden test returned.
pub const HIDDEN_TESTS_PASSED: &str = "SAIL_HIDDEN_TESTS_PASSED";

/// Compiler-backed exercise material that never reaches the learner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustFixture {
    pub step_id: String,
    pub reference_solution: String,
    /// Hidden test functions appended to the program under test: plain `fn hidden_*()` items
    /// that panic on failure. The validator adds a `main` that calls each one and prints
    /// [`HIDDEN_TESTS_PASSED`], because the sandbox checker runs `main`, not `cargo test`.
    /// The program under test must therefore not define `main` itself.
    pub hidden_tests: String,
    #[serde(default)]
    pub public_examples: Vec<String>,
    /// Rust edition the checker compiles with.
    pub edition: String,
    /// A deliberately wrong program the tests must reject (proves the tests bite).
    #[serde(default)]
    pub known_bad: Option<String>,
}

/// A physics/maths numeric answer and how to check it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NumericFixture {
    pub step_id: String,
    /// Known values with units, e.g. `{"m": "2 kg", "a": "3 m/s^2"}`.
    pub knowns: BTreeMap<String, String>,
    pub assumptions: Vec<String>,
    /// Expression over the knowns, e.g. `m * a`.
    pub formula: String,
    /// Expected answer with unit, e.g. `6 N`.
    pub expected: String,
    pub relative_tolerance: f64,
}

/// A rubric for answers that can't be graded by a key (philosophy, short writing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RubricFixture {
    pub step_id: String,
    pub criteria: Vec<String>,
    /// What a strong answer must not be marked down for (e.g. disagreeing with a doctrine).
    pub not_penalised: Vec<String>,
}

/// What the lesson-authoring model call returns. `lesson` reuses the course package's own
/// [`Lesson`] shape, so a validated draft publishes without translation.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LessonDraftOutput {
    pub lesson: Lesson,
    pub claims: Vec<Claim>,
    #[serde(default)]
    pub rust: Vec<RustFixture>,
    #[serde(default)]
    pub numeric: Vec<NumericFixture>,
    #[serde(default)]
    pub rubric: Vec<RubricFixture>,
    /// Unseen, equivalent variants for delayed review.
    #[serde(default)]
    pub review_variants: Vec<Step>,
    #[serde(default)]
    pub follow_up: Option<String>,
    #[serde(default)]
    pub limitations: Vec<String>,
}

/// A stored draft: the model output plus what the server knows about where it came from.
#[derive(Debug, Clone)]
pub struct LessonDraft {
    pub draft_id: DraftId,
    pub course_id: CourseId,
    pub plan_revision: u64,
    pub lesson_plan_id: String,
    pub output: LessonDraftOutput,
    /// The model's reply exactly as stored (after JSON extraction); `output` parses from it.
    pub raw_json: String,
    /// SHA-256 of `raw_json`. Receipts bind to it, so an edited draft needs new receipts.
    pub output_hash: ContentHash,
    /// Every source revision the authoring context included.
    pub built_from: Vec<SourceAnchorSet>,
    pub stage: LessonStage,
    pub created_ms: i64,
}

// ------------------------------------------------------------------------------- pipeline

/// Where a lesson draft is. Advances one stage at a time; each stage's result is durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonStage {
    Queued,
    SourceRetrieved,
    DraftGenerated,
    SchemaValidated,
    SourceGrounded,
    SubjectValidated,
    ExerciseTested,
    QualityReviewed,
    Ready,
    Published,
    // Terminal states.
    Failed,
    Cancelled,
    Blocked,
    NeedsReview,
    SourceInvalidated,
}

impl LessonStage {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Published
                | Self::Failed
                | Self::Cancelled
                | Self::Blocked
                | Self::NeedsReview
                | Self::SourceInvalidated
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    /// Parses, ids unique, `CoursePackage::validate` accepts it, no executable markup.
    Schema,
    /// Every claim resolves to a clean fragment or a checked derivation.
    Grounding,
    /// No verbatim run longer than the source's [`SourceRights::max_quote_words`].
    Copyright,
    /// Deterministic subject validator for the step's [`ValidatorKind`].
    Subject,
    /// Rust reference compiles, hidden tests pass, `known_bad` fails, in the sandbox.
    ExerciseTests,
    /// Structured quality report (optional reviewer model is extra evidence only).
    Quality,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "verdict")]
pub enum Verdict {
    Pass,
    Fail {
        reasons: Vec<String>,
    },
    /// Not provable by machine; the owner decides.
    NeedsReview {
        reasons: Vec<String>,
    },
    /// The validator couldn't run (sandbox down). Retry later; never a pass.
    Unavailable {
        reason: String,
    },
}

/// Proof that one server-side check ran on one exact draft. Created only by validator
/// code in this crate; model output has no field that can hold one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReceipt {
    pub receipt_id: ReceiptId,
    pub draft_id: DraftId,
    /// Must equal the draft's `output_hash`, or the receipt doesn't count.
    pub output_hash: ContentHash,
    pub check: CheckKind,
    /// Step id for per-step checks, `None` for whole-lesson checks.
    pub step_id: Option<String>,
    pub verdict: Verdict,
    /// e.g. `grounding/1`, bumped when a validator's rules change.
    pub validator_version: String,
    pub at_ms: i64,
}

/// What a validator needs besides the draft.
pub struct ValidationContext<'a> {
    pub fragments: &'a dyn FragmentLookup,
    pub now_ms: i64,
}

/// One validation stage. Implementations are deterministic except [`CheckKind::ExerciseTests`],
/// which calls the sandbox.
#[async_trait::async_trait]
pub trait DraftValidator: Send + Sync {
    fn check(&self) -> CheckKind;
    async fn validate(
        &self,
        draft: &LessonDraft,
        ctx: &ValidationContext<'_>,
    ) -> Vec<ValidationReceipt>;
}

/// Requirements a draft must meet before it can be published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stakes {
    /// Lessons and practice.
    Practice,
    /// Diagnostics, review gates, mid/final assessments: no `NeedsReview` receipts allowed.
    Assessment,
}

// ------------------------------------------------------------------------------ publication

/// An immutable, published version of a generated course. `package` is an ordinary
/// `CoursePackage`, served by `CourseEngine` exactly like an embedded one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageRevision {
    pub course_id: CourseId,
    /// Monotonic per course. Published revisions are never edited.
    pub revision: u64,
    /// SHA-256 of the canonical package JSON. Publishing the same hash twice is a no-op.
    pub package_hash: ContentHash,
    pub package_json: String,
    pub receipts: Vec<ReceiptId>,
    pub built_from: Vec<SourceAnchorSet>,
    /// The most restrictive rights of any source it was built from.
    pub rights: SourceRights,
    pub published_ms: i64,
}

/// What a source change affects. Learner attempt history and evidence are never deleted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Invalidation {
    pub concept_graphs: Vec<(CourseId, u64)>,
    pub plans: Vec<(CourseId, u64)>,
    pub drafts: Vec<DraftId>,
    /// Published revisions to re-validate; they stay served until a replacement passes,
    /// unless a grounding anchor no longer resolves (then the lesson is withdrawn).
    pub revisions: Vec<(CourseId, u64)>,
}

// ---------------------------------------------------------------------------- authoring jobs

/// One small, durable unit of background work. Stored as a Continuity task
/// (owner `sail-arena-learn`, kind `learn.factory.<kind>`), with this spec as its checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AuthoringJob {
    ExtractSource {
        source_id: SourceId,
        revision: u64,
    },
    /// A modern (compressed) PDF, extracted [`PDF_TEXT_PAGES_PER_CALL`] pages at a time
    /// through [`PdfTextExtractor`], so a 1,000-page book is many small resumable jobs.
    ExtractPdfPages {
        source_id: SourceId,
        revision: u64,
        /// 1-based physical page this chunk starts at.
        first_page: u32,
    },
    ExtractConcepts {
        course_id: CourseId,
        fragment_ids: Vec<FragmentId>,
    },
    BuildConceptGraph {
        course_id: CourseId,
    },
    BuildCurriculum {
        course_id: CourseId,
        concept_graph_revision: u64,
    },
    GenerateLesson {
        course_id: CourseId,
        plan_revision: u64,
        lesson_plan_id: String,
    },
    GenerateExerciseVariant {
        course_id: CourseId,
        objective_id: ObjectiveId,
        family: String,
    },
    GenerateReviewAssessment {
        course_id: CourseId,
        objective_id: ObjectiveId,
    },
    ValidateDraft {
        draft_id: DraftId,
    },
    PublishCourseRevision {
        course_id: CourseId,
    },
}

impl AuthoringJob {
    /// Continuity dedupe key: at most one active job per key.
    pub fn dedupe_key(&self) -> String {
        match self {
            Self::ExtractSource {
                source_id,
                revision,
            } => format!("extract:{source_id}@{revision}"),
            Self::ExtractPdfPages {
                source_id,
                revision,
                first_page,
            } => format!("pdf-pages:{source_id}@{revision}:{first_page}"),
            Self::ExtractConcepts {
                course_id,
                fragment_ids,
            } => {
                let first = fragment_ids.first().map(String::as_str).unwrap_or("");
                format!("concepts:{course_id}:{first}+{}", fragment_ids.len())
            }
            Self::BuildConceptGraph { course_id } => format!("graph:{course_id}"),
            Self::BuildCurriculum { course_id, .. } => format!("plan:{course_id}"),
            Self::GenerateLesson {
                course_id,
                lesson_plan_id,
                ..
            } => {
                format!("lesson:{course_id}:{lesson_plan_id}")
            }
            Self::GenerateExerciseVariant {
                course_id,
                objective_id,
                family,
            } => {
                format!("variant:{course_id}:{objective_id}:{family}")
            }
            Self::GenerateReviewAssessment {
                course_id,
                objective_id,
            } => {
                format!("review:{course_id}:{objective_id}")
            }
            Self::ValidateDraft { draft_id } => format!("validate:{draft_id}"),
            Self::PublishCourseRevision { course_id } => format!("publish:{course_id}"),
        }
    }

    /// Whether the job needs the reasoning model (and so must yield to Dialogue/voice).
    pub fn uses_model(&self) -> bool {
        matches!(
            self,
            Self::ExtractConcepts { .. }
                | Self::BuildCurriculum { .. }
                | Self::GenerateLesson { .. }
                | Self::GenerateExerciseVariant { .. }
                | Self::GenerateReviewAssessment { .. }
        )
    }
}

/// Why a job is worth doing now. Tiers follow the spec's ordering; lower tier runs first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum PriorityReason {
    ReviewDueSoon {
        objective_id: ObjectiveId,
        due_ms: i64,
    },
    WeakSkill {
        objective_id: ObjectiveId,
    },
    AheadOfProgress {
        lesson_plan_id: String,
        distance: u32,
    },
    MissingPrerequisite {
        objective_id: ObjectiveId,
    },
    ActiveCourseAssessment,
    RemainingChapter {
        unit_index: u32,
    },
    Enrichment,
    /// The owner asked for this course first, or "generate the next lesson now".
    OwnerRequest,
    /// Pipeline plumbing (extraction, validation, publish of work already started).
    Pipeline,
}

impl PriorityReason {
    /// 0 runs first. Validation/publish of a started draft outranks starting new work.
    pub fn tier(&self) -> u8 {
        match self {
            Self::OwnerRequest => 0,
            Self::Pipeline => 1,
            Self::ReviewDueSoon { .. } => 2,
            Self::WeakSkill { .. } => 3,
            Self::AheadOfProgress { .. } => 4,
            Self::MissingPrerequisite { .. } => 5,
            Self::ActiveCourseAssessment => 6,
            Self::RemainingChapter { .. } => 7,
            Self::Enrichment => 8,
        }
    }
}

/// A job with its deterministic priority and the reasons behind it (shown in the inspector).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedJob {
    pub job: AuthoringJob,
    pub reasons: Vec<PriorityReason>,
    /// `(tier, tie-break)`; smaller runs first. Same inputs → same order.
    pub priority: (u8, i64),
}

/// Why the model couldn't serve an authoring call. The scheduler maps these to job states.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorModelError {
    /// No model loaded or the backend is down: wait, don't retry in a loop.
    #[error("model unavailable: {0}")]
    Unavailable(String),
    /// Foreground work needs the model: discard partial output, resume from the last stage.
    #[error("preempted: {0}")]
    Preempted(String),
    /// Admission refused (memory pressure, budget spent).
    #[error("not admitted: {0}")]
    NotAdmitted(String),
    #[error("model call failed: {0}")]
    Failed(String),
}

/// A bounded authoring prompt. Built by the context builder, never from whole books.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthorPrompt {
    pub system: String,
    pub user: String,
    /// JSON schema name the reply must match (`concepts`, `curriculum`, `lesson`).
    pub schema: &'static str,
    pub max_output_tokens: u32,
    /// Thinking allowed before the reply, in tokens (`None`: answer directly). Set from
    /// `sail_common::request_body::Job::budget`, never by the model.
    pub reasoning_budget: Option<u32>,
}

/// The model as the factory sees it: background priority, cancellable, structured reply.
#[async_trait::async_trait]
pub trait AuthorModel: Send + Sync {
    async fn complete(
        &self,
        prompt: &AuthorPrompt,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<String, AuthorModelError>;
}

// ------------------------------------------------------------------------- course projects

/// What the owner asked the factory to build: a course over some sources. The
/// [`ConceptGraph`], [`CurriculumPlan`] and drafts all hang off `course_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CourseProject {
    pub course_id: CourseId,
    pub title: String,
    pub domain: Domain,
    /// Sources the course is grounded in. Order is the owner's reading order.
    pub source_ids: Vec<SourceId>,
    /// e.g. "Second-year undergraduate"; a hint to the curriculum planner, not a promise.
    pub level: String,
    pub status: ProjectStatus,
    pub created_ms: i64,
}

/// Where a course project is. The owner can pause it at any time; nothing is lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ProjectStatus {
    /// Sources are still being extracted.
    Extracting,
    /// Concept batches are being proposed and merged.
    Mapping,
    /// A plan is proposed and waits for the owner to approve it.
    AwaitingPlanApproval,
    /// Lessons are being generated and validated ahead of the learner.
    Generating,
    Paused,
    /// Every planned lesson is published or needs the owner.
    Complete,
    Failed {
        reason: String,
    },
}

// ---------------------------------------------------------------------- sandboxed PDF text

/// Pages per [`PdfTextExtractor::pages`] call. Keeps each sandbox reply well under the
/// runner's 2 MiB result frame even for dense pages.
pub const PDF_TEXT_PAGES_PER_CALL: u32 = 20;

/// Why sandboxed PDF text couldn't be produced. Mapped to job states like
/// [`AuthorModelError`]: `Unavailable` waits, `Refused` marks the source, `Failed` retries.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PdfTextError {
    /// The sandbox runner isn't configured or is down. Never fall back to in-daemon parsing.
    #[error("PDF text sandbox unavailable: {0}")]
    Unavailable(String),
    /// Encrypted, or the tool refused the file. The source becomes `Failed`/`NeedsReview`.
    #[error("PDF refused: {0}")]
    Refused(String),
    #[error("PDF text extraction failed: {0}")]
    Failed(String),
}

/// One page of sandbox output. `text` is untrusted data like any other source text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfPageText {
    /// 1-based physical page.
    pub page: u32,
    /// Printed page label when the PDF has one (`xii`, `37`).
    pub label: Option<String>,
    pub text: String,
}

/// Text extraction for PDFs the pure parser refuses (compressed object/xref streams,
/// incremental updates). Implemented outside this crate by a `pdftotext` run inside the
/// `sail-runner` sandbox; the daemon never parses those files itself.
#[async_trait::async_trait]
pub trait PdfTextExtractor: Send + Sync {
    /// Physical page count of the blob stored under `content_hash`.
    async fn page_count(&self, content_hash: &str) -> Result<u32, PdfTextError>;

    /// Text of pages `first..=last` (at most [`PDF_TEXT_PAGES_PER_CALL`]), in order.
    async fn pages(
        &self,
        content_hash: &str,
        first: u32,
        last: u32,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<Vec<PdfPageText>, PdfTextError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_rights_are_as_strict_as_restricted() {
        assert_eq!(
            SourceRights::Unknown.max_quote_words(),
            SourceRights::Restricted.max_quote_words()
        );
        assert!(!SourceRights::Unknown.exportable());
        assert!(
            SourceRights::Open {
                license: "MIT".into()
            }
            .exportable()
        );
    }

    #[test]
    fn draft_output_cannot_smuggle_a_receipt_or_a_publish_flag() {
        let json = r#"{"lesson":{"id":"l","title":"t","minutes":5,"objective_ids":[],"steps":[]},
            "claims":[],"receipts":[{"verdict":"pass"}]}"#;
        assert!(serde_json::from_str::<LessonDraftOutput>(json).is_err());
        let json = r#"{"lesson":{"id":"l","title":"t","minutes":5,"objective_ids":[],"steps":[]},
            "claims":[],"published":true}"#;
        assert!(serde_json::from_str::<LessonDraftOutput>(json).is_err());
    }

    #[test]
    fn priority_tiers_follow_the_spec_order() {
        let order = [
            PriorityReason::ReviewDueSoon {
                objective_id: "o".into(),
                due_ms: 0,
            },
            PriorityReason::WeakSkill {
                objective_id: "o".into(),
            },
            PriorityReason::AheadOfProgress {
                lesson_plan_id: "l".into(),
                distance: 1,
            },
            PriorityReason::MissingPrerequisite {
                objective_id: "o".into(),
            },
            PriorityReason::ActiveCourseAssessment,
            PriorityReason::RemainingChapter { unit_index: 3 },
            PriorityReason::Enrichment,
        ];
        let tiers: Vec<u8> = order.iter().map(PriorityReason::tier).collect();
        let mut sorted = tiers.clone();
        sorted.sort_unstable();
        assert_eq!(tiers, sorted);
    }

    #[test]
    fn sha256_hex_matches_a_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn terminal_stages_are_terminal() {
        assert!(LessonStage::Published.is_terminal());
        assert!(LessonStage::SourceInvalidated.is_terminal());
        assert!(!LessonStage::Ready.is_terminal());
    }
}
