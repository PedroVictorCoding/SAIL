//! Unit and integration tests for `factory::validate`.
//!
//! Uses synthetic clean-room fixtures and a fake [`RustEvaluator`] (`FakeSandbox`) to test
//! every check's passing, failing, `NeedsReview`, and `Unavailable` paths.

use super::*;
use crate::courses::content::{
    AnswerKey, Block, BlockKind, ChoiceOption, Lesson, RunLimits, Step, StepKind,
};
use crate::courses::engine::{NoSandbox, RustEvaluator};
use crate::courses::grade::{Diagnostic, RustRun};
use crate::factory::contracts::{
    CheckKind, Claim, DraftValidator, EducationalSource, FragmentKind, FragmentLookup,
    HIDDEN_TESTS_PASSED, IngestionStatus, LessonDraft, LessonDraftOutput, LessonStage,
    NumericFixture, QualityStatus, RubricFixture, RustFixture, SourceAnchorSet, SourceFragment,
    SourceLocation, SourceRights, SourceType, Stakes, ValidationContext, Verdict,
};
use std::collections::BTreeMap;
use std::sync::Arc;

const NOW_MS: i64 = 1_794_000_000_000;

/// In-memory [`FragmentLookup`] for synthetic validation tests.
#[derive(Default, Clone)]
struct FakeFragments {
    sources: BTreeMap<String, EducationalSource>,
    fragments: BTreeMap<String, SourceFragment>,
}

impl FakeFragments {
    fn with_restricted_rust_source() -> Self {
        let mut store = Self::default();
        store.sources.insert(
            "src-rust".to_string(),
            EducationalSource {
                source_id: "src-rust".to_string(),
                title: "Synthetic Systems Guide".to_string(),
                authors: vec!["A. Author".to_string()],
                source_type: SourceType::Markdown,
                content_hash: "hash-src-rust-v1".to_string(),
                revision: 1,
                language: "en".to_string(),
                access_scope: "learning:rust".to_string(),
                rights: SourceRights::Restricted,
                status: IngestionStatus::Indexed,
                imported_ms: NOW_MS - 10_000,
            },
        );
        store.fragments.insert(
            "frag-own-1".to_string(),
            SourceFragment {
                fragment_id: "frag-own-1".to_string(),
                source_id: "src-rust".to_string(),
                source_revision: 1,
                kind: FragmentKind::Definition,
                chapter_path: vec!["Chapter 1".to_string(), "Ownership".to_string()],
                location: SourceLocation::Lines { first: 1, last: 20 },
                text: "In Rust, every heap-allocated value has a single owning binding at any given point in execution, and reassigning that binding transfers ownership to the destination variable while invalidating the prior variable in the local scope.".to_string(),
                content_hash: "hash-frag-own-1".to_string(),
                quality: QualityStatus::Clean,
            },
        );
        store
    }
}

impl FragmentLookup for FakeFragments {
    fn source(&self, id: &str) -> Option<EducationalSource> {
        self.sources.get(id).cloned()
    }

    fn fragment(&self, id: &str) -> Option<SourceFragment> {
        self.fragments.get(id).cloned()
    }
}

/// Scriptable sandbox evaluator that inspects the submitted source to return different
/// [`RustRun`] outcomes for `reference_solution` versus `known_bad`.
struct ScriptedSandbox<F>(F)
where
    F: Fn(&str, RunLimits) -> RustRun + Send + Sync;

#[async_trait::async_trait]
impl<F> RustEvaluator for ScriptedSandbox<F>
where
    F: Fn(&str, RunLimits) -> RustRun + Send + Sync,
{
    async fn run(&self, source: &str, limits: RunLimits) -> RustRun {
        (self.0)(source, limits)
    }
}

fn valid_explain_step() -> Step {
    Step {
        id: "step-explain".to_string(),
        kind: StepKind::Explain,
        family: None,
        title: "Single ownership and move transfer".to_string(),
        objective_ids: vec!["obj.move".to_string()],
        confidence: false,
        body: vec![Block::p(
            "Each heap buffer has one owning variable. Reassigning a String transfers ownership to the new binding.",
        )],
        options: Vec::new(),
        multi: false,
        code: None,
        items: Vec::new(),
        template: None,
        placeholder: None,
        starter: None,
        limits: None,
        key: None,
        misconceptions: BTreeMap::new(),
        hints: Vec::new(),
        correct: Vec::new(),
        solution: Vec::new(),
    }
}

fn valid_choice_step() -> Step {
    let mut misconceptions = BTreeMap::new();
    misconceptions.insert(
        "opt-copy".to_string(),
        "A String owns a heap buffer and moves instead of copying implicitly.".to_string(),
    );
    Step {
        id: "step-predict".to_string(),
        kind: StepKind::Predict,
        family: Some("fam-move-basic".to_string()),
        title: "Predict variable validity after assignment".to_string(),
        objective_ids: vec!["obj.move".to_string()],
        confidence: true,
        body: vec![Block::p(
            "What happens when the first binding is read after moving its String?",
        )],
        options: vec![
            ChoiceOption {
                id: "opt-err".to_string(),
                text: "Compilation fails because ownership moved to the second binding."
                    .to_string(),
                code: false,
            },
            ChoiceOption {
                id: "opt-copy".to_string(),
                text: "Both bindings remain readable because String duplicates automatically."
                    .to_string(),
                code: false,
            },
        ],
        multi: false,
        code: Some(
            "let a = String::from(\" beacon \");\nlet b = a;\nprintln!(\"{a}\");".to_string(),
        ),
        items: Vec::new(),
        template: None,
        placeholder: None,
        starter: None,
        limits: None,
        key: Some(AnswerKey::Choice(vec!["opt-err".to_string()])),
        misconceptions,
        hints: vec![
            "Consider how many bindings can own the heap buffer at once.".to_string(),
            "After `let b = a;`, `a` is no longer initialized.".to_string(),
        ],
        correct: vec![Block::p("Correct: ownership moved to `b`.")],
        solution: vec![Block::p("Answer: compilation fails due to use after move.")],
    }
}

fn valid_transfer_step() -> Step {
    Step {
        id: "step-transfer".to_string(),
        kind: StepKind::Choice,
        family: Some("fam-move-fn-arg".to_string()),
        title: "Transfer: passing an owned String into a function".to_string(),
        objective_ids: vec!["obj.move".to_string()],
        confidence: true,
        body: vec![Block::p(
            "Passing a String by value to a helper function follows the same ownership rule.",
        )],
        options: vec![
            ChoiceOption {
                id: "moved".to_string(),
                text: "Ownership transfers into the function parameter.".to_string(),
                code: false,
            },
            ChoiceOption {
                id: "borrowed".to_string(),
                text: "The caller retains ownership without borrowing.".to_string(),
                code: false,
            },
        ],
        multi: false,
        code: None,
        items: Vec::new(),
        template: None,
        placeholder: None,
        starter: None,
        limits: None,
        key: Some(AnswerKey::Choice(vec!["moved".to_string()])),
        misconceptions: BTreeMap::new(),
        hints: vec![
            "By-value parameters take ownership just like `let` bindings.".to_string(),
            "Once moved into the parameter, the caller cannot use the String again.".to_string(),
        ],
        correct: vec![Block::p(
            "Correct: the function parameter now owns the String.",
        )],
        solution: vec![Block::p("Ownership transfers into the parameter.")],
    }
}

fn valid_draft() -> LessonDraft {
    LessonDraft {
        draft_id: "draft-own-1".to_string(),
        course_id: "rust-systems".to_string(),
        plan_revision: 1,
        lesson_plan_id: "lp-own-1".to_string(),
        output: LessonDraftOutput {
            lesson: Lesson {
                id: "lesson-own-1".to_string(),
                title: "Ownership and Moves".to_string(),
                minutes: 5,
                objective_ids: vec!["obj.move".to_string()],
                prerequisites: Vec::new(),
                steps: vec![
                    valid_explain_step(),
                    valid_choice_step(),
                    valid_transfer_step(),
                ],
            },
            claims: vec![Claim {
                text: "Reassigning an owning String binding transfers ownership and invalidates the previous variable.".to_string(),
                fragment_ids: vec!["frag-own-1".to_string()],
                derivation: None,
            }],
            rust: Vec::new(),
            numeric: Vec::new(),
            rubric: Vec::new(),
            review_variants: Vec::new(),
            follow_up: None,
            limitations: Vec::new(),
        },
        raw_json: "{\"lesson\":\"synthetic\"}".to_string(),
        output_hash: "hash-draft-own-1-v1".to_string(),
        built_from: vec![SourceAnchorSet {
            source_id: "src-rust".to_string(),
            source_revision: 1,
            content_hash: "hash-src-rust-v1".to_string(),
        }],
        stage: LessonStage::DraftGenerated,
        created_ms: NOW_MS,
    }
}

// --------------------------------------------------------------------------- 1. Schema tests

#[tokio::test]
async fn schema_passes_clean_draft_and_rejects_script_and_executable_markup() {
    let store = FakeFragments::with_restricted_rust_source();
    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };

    let clean = valid_draft();
    let receipts = SchemaValidator.validate(&clean, &ctx).await;
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].verdict, Verdict::Pass);
    assert_eq!(receipts[0].output_hash, clean.output_hash);
    assert_eq!(receipts[0].validator_version, schema::VALIDATOR_VERSION);

    // Specifically required: `<script>` in a body is rejected.
    let mut with_script = valid_draft();
    with_script.output.lesson.steps[0].body[0].text =
        "Normal explanation <script>alert(1)</script> injected.".to_string();
    let r_script = SchemaValidator.validate(&with_script, &ctx).await;
    assert!(
        matches!(&r_script[0].verdict, Verdict::Fail { reasons } if reasons.iter().any(|m| m.contains("<script"))),
        "expected `<script` failure, got {:?}",
        r_script[0].verdict
    );

    // Also reject `javascript:`, `onerror=`, `<iframe>`, raw HTML in `p`, and `data:` URLs.
    for payload in [
        "Click javascript:evil()",
        "<img src=x onerror=alert(1)>",
        "<iframe src=\"https://example.com\"></iframe>",
        "Paragraph with <div>raw html</div> tag",
        "Payload data:text/html;base64,PHNjcmlwdD4=",
    ] {
        let mut bad = valid_draft();
        bad.output.lesson.steps[0].body[0].text = payload.to_string();
        let res = SchemaValidator.validate(&bad, &ctx).await;
        assert!(
            matches!(res[0].verdict, Verdict::Fail { .. }),
            "payload `{payload}` should fail schema validation"
        );
    }
}

#[tokio::test]
async fn schema_rejects_mismatched_fixture_step_and_excess_hints() {
    let store = FakeFragments::with_restricted_rust_source();
    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };

    let mut bad = valid_draft();
    // Point a RustFixture at a Predict step instead of a RustEdit step, and exceed MAX_HINTS.
    bad.output.rust.push(RustFixture {
        step_id: "step-predict".to_string(),
        reference_solution: "pub fn add(a: i32, b: i32) -> i32 { a + b }".to_string(),
        hidden_tests: "fn hidden_check() { assert_eq!(add(1, 2), 3); }".to_string(),
        public_examples: Vec::new(),
        edition: "2021".to_string(),
        known_bad: Some("pub fn add(a: i32, b: i32) -> i32 { a - b }".to_string()),
    });
    bad.output.lesson.steps[1].hints = vec![
        "h1".into(),
        "h2".into(),
        "h3".into(),
        "h4".into(),
        "h5".into(),
    ];

    let receipts = SchemaValidator.validate(&bad, &ctx).await;
    match &receipts[0].verdict {
        Verdict::Fail { reasons } => {
            assert!(reasons.iter().any(|r| r.contains("RustFixture")));
            assert!(
                reasons
                    .iter()
                    .any(|r| r.contains("MAX_HINTS") || r.contains("hints"))
            );
        }
        other => panic!("expected Fail, got {other:?}"),
    }
}

// --------------------------------------------------------------------------- 2. Grounding tests

#[tokio::test]
async fn grounding_passes_valid_fragment_and_fails_on_fabricated_id_or_stale_hash() {
    let store = FakeFragments::with_restricted_rust_source();
    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };

    // Passing case.
    let clean = valid_draft();
    let ok = GroundingValidator.validate(&clean, &ctx).await;
    assert_eq!(ok[0].verdict, Verdict::Pass);

    // Specifically required: a fabricated fragment id -> Fail.
    let mut fabricated = valid_draft();
    fabricated.output.claims[0].fragment_ids = vec!["frag-fabricated-does-not-exist".to_string()];
    let res_fab = GroundingValidator.validate(&fabricated, &ctx).await;
    assert!(
        matches!(&res_fab[0].verdict, Verdict::Fail { reasons } if reasons.iter().any(|r| r.contains("frag-fabricated-does-not-exist"))),
        "expected Fail on fabricated fragment id, got {:?}",
        res_fab[0].verdict
    );

    // Specifically required: a stale hash -> Fail.
    let mut stale = valid_draft();
    stale.built_from[0].content_hash = "stale-hash-from-previous-import".to_string();
    let res_stale = GroundingValidator.validate(&stale, &ctx).await;
    assert!(
        matches!(&res_stale[0].verdict, Verdict::Fail { reasons } if reasons.iter().any(|r| r.contains("stale content_hash"))),
        "expected Fail on stale hash, got {:?}",
        res_stale[0].verdict
    );

    // Fragment with NeedsValidation quality -> Fail.
    let mut scanned_store = store.clone();
    scanned_store
        .fragments
        .get_mut("frag-own-1")
        .unwrap()
        .quality = QualityStatus::NeedsValidation {
        reason: "scanned formula".to_string(),
    };
    let scanned_ctx = ValidationContext {
        fragments: &scanned_store,
        now_ms: NOW_MS,
    };
    let res_scan = GroundingValidator.validate(&clean, &scanned_ctx).await;
    assert!(matches!(res_scan[0].verdict, Verdict::Fail { .. }));
}

#[tokio::test]
async fn grounding_marks_unsupported_claim_or_uncovered_explain_step_needs_review() {
    let store = FakeFragments::with_restricted_rust_source();
    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };

    // Claim with no fragment citations and no derivation -> NeedsReview.
    let mut unsupported = valid_draft();
    unsupported.output.claims[0].fragment_ids.clear();
    unsupported.output.claims[0].derivation = None;
    let res = GroundingValidator.validate(&unsupported, &ctx).await;
    assert!(matches!(res[0].verdict, Verdict::NeedsReview { .. }));

    // Explain step whose body has no claim covering it -> NeedsReview.
    let mut uncovered = valid_draft();
    let mut extra_explain = valid_explain_step();
    extra_explain.id = "explain-ungrounded".to_string();
    extra_explain.title = "Thermodynamic entropy in heat engines".to_string();
    extra_explain.body = vec![Block::p(
        "Carnot efficiency depends exclusively on the cold and hot reservoir temperatures.",
    )];
    uncovered.output.lesson.steps.push(extra_explain);
    let res_uncovered = GroundingValidator.validate(&uncovered, &ctx).await;
    assert!(
        matches!(&res_uncovered[0].verdict, Verdict::NeedsReview { reasons } if reasons.iter().any(|r| r.contains("explain-ungrounded"))),
        "expected NeedsReview for uncovered explain step, got {:?}",
        res_uncovered[0].verdict
    );
}

// --------------------------------------------------------------------------- 3. Copyright tests

#[tokio::test]
async fn copyright_passes_paraphrase_and_fails_on_30_word_verbatim_restricted_run() {
    let mut store = FakeFragments::with_restricted_rust_source();
    let thirty_five_word_source = "one two three four five six seven eight nine ten \
        eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty \
        twentyone twentytwo twentythree twentyfour twentyfive twentysix twentyseven twentyeight twentynine thirty \
        thirtyone thirtytwo thirtythree thirtyfour thirtyfive";
    store.fragments.get_mut("frag-own-1").unwrap().text = thirty_five_word_source.to_string();

    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };

    // Paraphrased draft (<= 25 shared words) passes.
    let clean = valid_draft();
    let ok = CopyrightValidator.validate(&clean, &ctx).await;
    assert_eq!(ok[0].verdict, Verdict::Pass);

    // Specifically required: a 30-word verbatim run from a `restricted` source fails,
    // with the offending run truncated to 8 words.
    let thirty_word_quote = "one two three four five six seven eight nine ten \
        eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty \
        twentyone twentytwo twentythree twentyfour twentyfive twentysix twentyseven twentyeight twentynine thirty";
    let mut plagiarised = valid_draft();
    plagiarised.output.lesson.steps[0].body[0].text =
        format!("Here is the exact passage: {thirty_word_quote}.");

    let fail = CopyrightValidator.validate(&plagiarised, &ctx).await;
    match &fail[0].verdict {
        Verdict::Fail { reasons } => {
            assert_eq!(reasons.len(), 1);
            let msg = &reasons[0];
            assert!(
                msg.contains("30 words exceeds limit of 25 words"),
                "msg: {msg}"
            );
            assert!(
                msg.contains("one two three four five six seven eight ..."),
                "expected 8-word truncation, got: {msg}"
            );
            assert!(
                !msg.contains("nine"),
                "offending run must be truncated to 8 words, got: {msg}"
            );
        }
        other => panic!("expected Fail on 30-word verbatim run, got {other:?}"),
    }
}

#[tokio::test]
async fn copyright_stays_fast_on_50_fragments_of_4kb_each() {
    let mut store = FakeFragments::with_restricted_rust_source();
    let mut claim_fids = Vec::new();
    for i in 0..50 {
        let fid = format!("frag-bulk-{i}");
        // ~650 words (~4.2 KB) per fragment.
        let text: String = (0..650)
            .map(|w| format!("sect{i}word{w}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.len() >= 4096);
        store.fragments.insert(
            fid.clone(),
            SourceFragment {
                fragment_id: fid.clone(),
                source_id: "src-rust".to_string(),
                source_revision: 1,
                kind: FragmentKind::Prose,
                chapter_path: vec!["Bulk".to_string()],
                location: SourceLocation::Lines { first: 1, last: 99 },
                text,
                content_hash: format!("hash-bulk-{i}"),
                quality: QualityStatus::Clean,
            },
        );
        claim_fids.push(fid);
    }

    let mut draft = valid_draft();
    draft.output.claims[0].fragment_ids = claim_fids;
    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };

    let start = std::time::Instant::now();
    let receipts = CopyrightValidator.validate(&draft, &ctx).await;
    let elapsed = start.elapsed();

    assert_eq!(receipts[0].verdict, Verdict::Pass);
    assert!(
        elapsed.as_millis() < 500,
        "expected fast Suffix Automaton copyright check (<500ms), took {elapsed:?}"
    );
}

// --------------------------------------------------------------------------- 4. Subject & Units tests

#[tokio::test]
async fn subject_rejects_logic_key_that_contradicts_logic_rs_and_passes_consistent_key() {
    let store = FakeFragments::with_restricted_rust_source();
    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };

    // Affirming the Consequent (`P -> Q`, `Q` |- `P`) is INVALID in propositional logic.
    // Keying it as `"valid"` contradicts `courses::logic` and must Fail.
    let mut bad_logic = valid_draft();
    bad_logic.course_id = "philosophy-logic".to_string();
    bad_logic.output.lesson.steps[1] = Step {
        id: "step-logic-contradiction".to_string(),
        kind: StepKind::Choice,
        family: Some("fam-logic-ac".to_string()),
        title: "Evaluate deductive validity".to_string(),
        objective_ids: vec!["obj.move".to_string()],
        confidence: true,
        body: vec![
            Block::p("Determine whether the following argument is valid:"),
            Block {
                kind: BlockKind::Code,
                text: "Premise 1: P -> Q\nPremise 2: Q\nConclusion: P".to_string(),
                lang: Some("text".to_string()),
            },
        ],
        options: vec![
            ChoiceOption {
                id: "valid".to_string(),
                text: "Valid: the conclusion follows necessarily.".to_string(),
                code: false,
            },
            ChoiceOption {
                id: "invalid".to_string(),
                text: "Invalid: affirming the consequent is a formal fallacy.".to_string(),
                code: false,
            },
        ],
        multi: false,
        code: None,
        items: Vec::new(),
        template: None,
        placeholder: None,
        starter: None,
        limits: None,
        // Contradicts `courses::logic`:
        key: Some(AnswerKey::Choice(vec!["valid".to_string()])),
        misconceptions: BTreeMap::new(),
        hints: vec!["Check truth assignment P=false, Q=true.".to_string()],
        correct: Vec::new(),
        solution: Vec::new(),
    };

    let fail_receipts = SubjectValidator.validate(&bad_logic, &ctx).await;
    let step_receipt = fail_receipts
        .iter()
        .find(|r| r.step_id.as_deref() == Some("step-logic-contradiction"))
        .unwrap();
    assert!(
        matches!(&step_receipt.verdict, Verdict::Fail { reasons } if reasons.iter().any(|r| r.contains("courses::logic"))),
        "expected Fail when logic key contradicts courses::logic, got {:?}",
        step_receipt.verdict
    );

    // Fixing the key to `"invalid"` passes.
    bad_logic.output.lesson.steps[1].key = Some(AnswerKey::Choice(vec!["invalid".to_string()]));
    let ok_receipts = SubjectValidator.validate(&bad_logic, &ctx).await;
    let fixed_receipt = ok_receipts
        .iter()
        .find(|r| r.step_id.as_deref() == Some("step-logic-contradiction"))
        .unwrap();
    assert_eq!(fixed_receipt.verdict, Verdict::Pass);
}

#[tokio::test]
async fn subject_units_2kg_times_3m_per_s2_equals_6n_passes_and_6j_fails_on_dimension() {
    let store = FakeFragments::with_restricted_rust_source();
    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };

    // Direct unit evaluator checks:
    let mut knowns = BTreeMap::new();
    knowns.insert("m".to_string(), "2 kg".to_string());
    knowns.insert("a".to_string(), "3 m/s²".to_string());

    let passing_fixture = NumericFixture {
        step_id: "step-predict".to_string(),
        knowns: knowns.clone(),
        assumptions: vec!["point mass".to_string()],
        formula: "m × a".to_string(),
        expected: "6 N".to_string(),
        relative_tolerance: 1e-6,
    };
    let q = check_numeric_fixture(&passing_fixture).expect("2 kg × 3 m/s² = 6 N must pass");
    assert!((q.value - 6.0).abs() < 1e-9);
    assert_eq!(
        q.dim,
        Dimension {
            m: 1,
            kg: 1,
            s: -2,
            ..Dimension::dimensionless()
        }
    );

    // Inline expression `2 kg × 3 m/s²` also evaluates to 6 N.
    let inline_q = parse_quantity("2 kg × 3 m/s²").expect("inline expression parses");
    assert!((inline_q.value - 6.0).abs() < 1e-9);
    assert_eq!(inline_q.dim, q.dim);

    // SubjectValidator pass for 6 N:
    let mut draft_ok = valid_draft();
    draft_ok.output.numeric.push(passing_fixture);
    let ok_receipts = SubjectValidator.validate(&draft_ok, &ctx).await;
    assert!(ok_receipts.iter().all(|r| r.verdict == Verdict::Pass));

    // Specifically required: expecting `6 J` fails on dimension, naming both dimensions.
    let mut draft_bad_dim = draft_ok.clone();
    draft_bad_dim.output.numeric[0].expected = "6 J".to_string();
    let bad_receipts = SubjectValidator.validate(&draft_bad_dim, &ctx).await;
    let failed = bad_receipts
        .iter()
        .find(|r| r.step_id.as_deref() == Some("step-predict"))
        .unwrap();
    match &failed.verdict {
        Verdict::Fail { reasons } => {
            let combined = reasons.join(" ");
            assert!(
                combined.contains("dimension mismatch"),
                "expected dimension mismatch in `{combined}`"
            );
            assert!(
                combined.contains("energy/work (J)") && combined.contains("force (N)"),
                "expected both dimensions (`energy/work (J)` and `force (N)`) named in `{combined}`"
            );
        }
        other => panic!("expected Fail on dimension mismatch (6 N vs 6 J), got {other:?}"),
    }
}

#[test]
fn units_evaluator_supports_derived_units_prefixes_sqrt_and_trig() {
    let mut knowns = BTreeMap::new();
    knowns.insert("V".to_string(), parse_quantity("12 V").unwrap());
    knowns.insert("R".to_string(), parse_quantity("2.4 kΩ").unwrap());
    let current = evaluate_formula("V / R", &knowns).unwrap();
    let expected_i = parse_quantity("5 mA").unwrap();
    assert_eq!(current.dim, expected_i.dim);
    assert!((current.value - expected_i.value).abs() < 1e-9);

    // Power = V * I = 12 V * 5 mA = 60 mW, energy over 10 s = 0.6 J, pressure = 6 N / 2 cm^2 = 30 kPa.
    let mut k2 = BTreeMap::new();
    k2.insert("g".to_string(), parse_quantity("9.81 m/s^2").unwrap());
    k2.insert("h".to_string(), parse_quantity("20 m").unwrap());
    let v = evaluate_formula("sqrt(2 * g * h) * cos(60 deg)", &k2).unwrap();
    assert_eq!(
        v.dim,
        Dimension {
            m: 1,
            s: -1,
            ..Dimension::dimensionless()
        }
    );
    assert!((v.value - (2.0_f64 * 9.81 * 20.0).sqrt() * 0.5).abs() < 1e-6);

    // Frequency and charge: 5 C * 60 Hz = 300 A.
    let q_i = parse_quantity("5 C * 60 Hz").unwrap();
    let expected_a = parse_quantity("300 A").unwrap();
    assert_eq!(q_i.dim, expected_a.dim);
    assert!((q_i.value - 300.0).abs() < 1e-9);
}

#[tokio::test]
async fn subject_marks_ambiguous_philosophy_step_needs_review_and_never_pass() {
    let store = FakeFragments::with_restricted_rust_source();
    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };

    // Specifically required: an ambiguous philosophy step becomes `NeedsReview` with
    // criteria listed, and never `Pass` by itself.
    let mut phil = valid_draft();
    phil.course_id = "philosophy-ethics".to_string();
    phil.output.rubric.push(RubricFixture {
        step_id: "step-predict".to_string(),
        criteria: vec![
            "Distinguishes between deontological duty and consequentialist outcomes.".to_string(),
            "Addresses the counterexample without begging the question.".to_string(),
        ],
        not_penalised: vec![
            "Defending either Kantian or utilitarian conclusions with coherent reasons."
                .to_string(),
        ],
    });

    let receipts = SubjectValidator.validate(&phil, &ctx).await;
    let rubric_receipt = receipts
        .iter()
        .find(|r| r.step_id.as_deref() == Some("step-predict"))
        .unwrap();
    match &rubric_receipt.verdict {
        Verdict::NeedsReview { reasons } => {
            assert!(reasons.iter().any(|r| r.contains("deontological duty")));
            assert!(reasons.iter().any(|r| r.contains("not penalised")));
        }
        other => panic!("expected NeedsReview for philosophy rubric step, got {other:?}"),
    }

    // If `not_penalised` is empty for philosophy, it must Fail.
    phil.output.rubric[0].not_penalised.clear();
    let bad_receipts = SubjectValidator.validate(&phil, &ctx).await;
    let failed_rubric = bad_receipts
        .iter()
        .find(|r| r.step_id.as_deref() == Some("step-predict"))
        .unwrap();
    assert!(matches!(failed_rubric.verdict, Verdict::Fail { .. }));
}

// --------------------------------------------------------------------------- 5. ExerciseTests tests

fn draft_with_rust_fixture() -> LessonDraft {
    let mut draft = valid_draft();
    draft.output.lesson.steps.push(Step {
        id: "step-rust-edit".to_string(),
        kind: StepKind::RustEdit,
        family: Some("fam-borrow-len".to_string()),
        title: "Borrow a slice without taking ownership".to_string(),
        objective_ids: vec!["obj.move".to_string()],
        confidence: false,
        body: vec![Block::p("Implement `count_bytes` over a borrowed `&str`.")],
        options: Vec::new(),
        multi: false,
        code: None,
        items: Vec::new(),
        template: None,
        placeholder: None,
        starter: Some("pub fn count_bytes(s: &str) -> usize {\n    todo!()\n}\n".to_string()),
        limits: Some(RunLimits {
            wall_ms: 10_000,
            mem_mb: 256,
        }),
        key: Some(AnswerKey::Rust {
            stdout: HIDDEN_TESTS_PASSED.to_string(),
        }),
        misconceptions: BTreeMap::new(),
        hints: vec![
            "Use `.len()` on the borrowed `&str`.".to_string(),
            "Return `s.len()` directly.".to_string(),
        ],
        correct: vec![Block::p("Correct.")],
        solution: vec![Block::p("pub fn count_bytes(s: &str) -> usize { s.len() }")],
    });
    draft.output.rust.push(RustFixture {
        step_id: "step-rust-edit".to_string(),
        reference_solution: "pub fn count_bytes(s: &str) -> usize {\n    s.len()\n}\n".to_string(),
        hidden_tests: "fn hidden_empty() { assert_eq!(count_bytes(\"\"), 0); }\nfn hidden_word() { assert_eq!(count_bytes(\"sail\"), 4); }\n".to_string(),
        public_examples: Vec::new(),
        edition: "2021".to_string(),
        known_bad: Some("pub fn count_bytes(_s: &str) -> usize {\n    0\n}\n".to_string()),
    });
    draft
}

#[tokio::test]
async fn exercise_tests_passes_valid_fixture_and_fails_broken_fixture_or_known_bad_that_passes() {
    let store = FakeFragments::with_restricted_rust_source();
    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };

    let draft = draft_with_rust_fixture();

    // 1. Passing case: reference_solution prints sentinel, known_bad panics (exits 101).
    let good_sandbox = Arc::new(ScriptedSandbox(|src: &str, _limits| {
        assert!(src.contains("hidden_empty();"));
        assert!(src.contains("hidden_word();"));
        if src.contains("s.len()") {
            RustRun::Compiled {
                exit_code: Some(0),
                stdout: format!("{HIDDEN_TESTS_PASSED}\n"),
                diagnostics: Vec::new(),
            }
        } else {
            RustRun::Compiled {
                exit_code: Some(101),
                stdout: String::new(),
                diagnostics: Vec::new(),
            }
        }
    }));
    let validator_ok = ExerciseTestsValidator::new(good_sandbox);
    let r_ok = validator_ok.validate(&draft, &ctx).await;
    assert_eq!(r_ok.len(), 1);
    assert_eq!(r_ok[0].verdict, Verdict::Pass);

    // 2. Specifically required: a broken Rust fixture (reference_solution fails to compile).
    let broken_sandbox = Arc::new(ScriptedSandbox(|_src: &str, _limits| {
        RustRun::CompileFailed {
            diagnostics: vec![Diagnostic {
                line: 2,
                column: 5,
                level: "error".to_string(),
                message: "[E0308] mismatched types".to_string(),
            }],
        }
    }));
    let validator_broken = ExerciseTestsValidator::new(broken_sandbox);
    let r_broken = validator_broken.validate(&draft, &ctx).await;
    assert!(
        matches!(&r_broken[0].verdict, Verdict::Fail { reasons } if reasons.iter().any(|r| r.contains("E0308"))),
        "expected Fail on broken Rust fixture, got {:?}",
        r_broken[0].verdict
    );

    // 3. Specifically required: a `known_bad` that passes -> Fail ("hidden tests do not bite").
    let non_biting_sandbox = Arc::new(ScriptedSandbox(|_src: &str, _limits| RustRun::Compiled {
        exit_code: Some(0),
        stdout: format!("{HIDDEN_TESTS_PASSED}\n"),
        diagnostics: Vec::new(),
    }));
    let validator_non_biting = ExerciseTestsValidator::new(non_biting_sandbox);
    let r_non_biting = validator_non_biting.validate(&draft, &ctx).await;
    assert!(
        matches!(&r_non_biting[0].verdict, Verdict::Fail { reasons } if reasons.iter().any(|r| r.contains("known_bad") && r.contains("bite"))),
        "expected Fail when known_bad passes, got {:?}",
        r_non_biting[0].verdict
    );

    // 4. Sandbox Unavailable -> Verdict::Unavailable, never Pass.
    let validator_unavail = ExerciseTestsValidator::new(Arc::new(NoSandbox));
    let r_unavail = validator_unavail.validate(&draft, &ctx).await;
    assert!(matches!(r_unavail[0].verdict, Verdict::Unavailable { .. }));
}

#[tokio::test]
async fn exercise_tests_rejects_wrong_edition_fn_main_and_forbidden_apis() {
    let store = FakeFragments::with_restricted_rust_source();
    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };
    let sandbox = Arc::new(ScriptedSandbox(|_src: &str, _limits| RustRun::Compiled {
        exit_code: Some(0),
        stdout: format!("{HIDDEN_TESTS_PASSED}\n"),
        diagnostics: Vec::new(),
    }));
    let validator = ExerciseTestsValidator::new(sandbox);

    let mut bad_edition = draft_with_rust_fixture();
    bad_edition.output.rust[0].edition = "2024".to_string();
    let r1 = validator.validate(&bad_edition, &ctx).await;
    assert!(matches!(r1[0].verdict, Verdict::Fail { .. }));

    for forbidden_snippet in [
        "fn main() {}",
        "pub fn count_bytes(s: &str) -> usize { unsafe { s.len() } }",
        "pub fn count_bytes(_s: &str) -> usize { let _ = std::fs::read(\"x\"); 0 }",
        "pub fn count_bytes(_s: &str) -> usize { let _ = std::process::Command::new(\"sh\"); 0 }",
        "pub fn count_bytes(_s: &str) -> usize { let _ = std::net::TcpStream::connect(\"127.0.0.1:80\"); 0 }",
        "pub fn count_bytes(_s: &str) -> usize { let _ = include!(\"secret.rs\"); 0 }",
        "pub fn count_bytes(_s: &str) -> usize { let _ = env!(\"PATH\"); 0 }",
    ] {
        let mut d = draft_with_rust_fixture();
        d.output.rust[0].reference_solution = forbidden_snippet.to_string();
        let res = validator.validate(&d, &ctx).await;
        assert!(
            matches!(res[0].verdict, Verdict::Fail { .. }),
            "snippet `{forbidden_snippet}` must be rejected"
        );
    }
}

// --------------------------------------------------------------------------- 6. Quality tests

#[tokio::test]
async fn quality_passes_balanced_lesson_and_fails_bad_duration_missing_transfer_or_hints() {
    let store = FakeFragments::with_restricted_rust_source();
    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };

    let clean = valid_draft();
    let ok = QualityValidator.validate(&clean, &ctx).await;
    assert_eq!(ok[0].verdict, Verdict::Pass);

    // Failing case: duration outside 3–8 min, uncovered objective, no transfer step, and
    // missing hint ladder.
    let mut bad = valid_draft();
    bad.output.lesson.minutes = 15;
    bad.output
        .lesson
        .objective_ids
        .push("obj.uncovered".to_string());
    // Remove transfer step and clear hints on the remaining scored step.
    bad.output.lesson.steps.pop();
    bad.output.lesson.steps[1].hints.clear();

    let res = QualityValidator.validate(&bad, &ctx).await;
    match &res[0].verdict {
        Verdict::Fail { reasons } => {
            assert!(reasons.iter().any(|r| r.contains("duration")));
            assert!(reasons.iter().any(|r| r.contains("obj.uncovered")));
            assert!(reasons.iter().any(|r| r.contains("transfer step")));
            assert!(reasons.iter().any(|r| r.contains("hint ladder")));
        }
        other => panic!("expected Quality Fail, got {other:?}"),
    }
}

// --------------------------------------------------------------------------- 7. publishable & ReceiptStore tests

#[tokio::test]
async fn publishable_accepts_current_receipts_and_rejects_replayed_receipt_for_edited_draft() {
    let store = FakeFragments::with_restricted_rust_source();
    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };
    let suite = ValidationSuite::new(Arc::new(NoSandbox));

    let draft = valid_draft();
    let receipts = suite.validate_all(&draft, &ctx).await;
    assert_eq!(publishable(&receipts, &draft, Stakes::Practice), Ok(()));
    assert_eq!(publishable(&receipts, &draft, Stakes::Assessment), Ok(()));

    // Specifically required: a receipt replayed for an edited draft must be rejected.
    let mut edited_draft = draft.clone();
    edited_draft.raw_json = "{\"lesson\":\"edited\"}".to_string();
    edited_draft.output_hash = "hash-draft-own-1-v2-edited".to_string();

    let replay_result = publishable(&receipts, &edited_draft, Stakes::Practice);
    assert!(
        replay_result.is_err(),
        "replayed receipts for an older output_hash must not authorize publishing an edited draft"
    );
    let errs = replay_result.unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("stale output_hash")),
        "expected stale output_hash error, got {errs:?}"
    );

    // Assessment stakes forbid any `NeedsReview` receipt even when a Pass receipt exists.
    let mut with_review = receipts.clone();
    with_review.push(make_receipt(
        &draft,
        CheckKind::Subject,
        Some("step-rubric".to_string()),
        Verdict::NeedsReview {
            reasons: vec!["rubric criterion: clarity".to_string()],
        },
        subject::VALIDATOR_VERSION,
        NOW_MS,
    ));
    assert_eq!(publishable(&with_review, &draft, Stakes::Practice), Ok(()));
    assert!(publishable(&with_review, &draft, Stakes::Assessment).is_err());

    // Round-trip receipts through SQLite `ReceiptStore`.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    ReceiptStore::init_tables(&conn).unwrap();
    ReceiptStore::save_receipts(&conn, &receipts).unwrap();
    let loaded = ReceiptStore::for_draft(&conn, &draft.draft_id).unwrap();
    assert_eq!(loaded.len(), receipts.len());
    assert_eq!(publishable(&loaded, &draft, Stakes::Practice), Ok(()));
}

#[tokio::test]
async fn subject_exact_key_rejects_multi_mismatch_and_duplicate_distractors() {
    let store = FakeFragments::with_restricted_rust_source();
    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };

    // Single-choice (`multi: false`) with 2 keys and duplicate distractor text -> Fail.
    let mut bad = valid_draft();
    bad.output.lesson.steps[1].multi = false;
    bad.output.lesson.steps[1].options.push(ChoiceOption {
        id: "opt-dup".to_string(),
        text: "Both bindings remain readable because String duplicates automatically.".to_string(),
        code: false,
    });
    bad.output.lesson.steps[1].key =
        Some(AnswerKey::Choice(vec!["opt-err".into(), "opt-copy".into()]));

    let receipts = SubjectValidator.validate(&bad, &ctx).await;
    let r = receipts
        .iter()
        .find(|r| r.step_id.as_deref() == Some("step-predict"))
        .unwrap();
    match &r.verdict {
        Verdict::Fail { reasons } => {
            assert!(reasons.iter().any(|m| m.contains("single-choice")));
            assert!(reasons.iter().any(|m| m.contains("duplicate")));
        }
        other => panic!("expected Fail on ExactKey violations, got {other:?}"),
    }
}

#[tokio::test]
async fn grounding_rechecks_claim_derivations_without_fragment_citations() {
    let store = FakeFragments::with_restricted_rust_source();
    let ctx = ValidationContext {
        fragments: &store,
        now_ms: NOW_MS,
    };

    // Valid logic derivation (`P -> Q, P |- Q`) and valid unit equation (`2 kg * 3 m/s^2 = 6 N`).
    let mut derived = valid_draft();
    derived.output.claims = vec![
        Claim {
            text: "Reassigning a String transfers ownership to the new binding.".to_string(),
            fragment_ids: Vec::new(),
            derivation: Some("P -> Q, P |- Q".to_string()),
        },
        Claim {
            text: "Force equals mass times acceleration.".to_string(),
            fragment_ids: Vec::new(),
            derivation: Some("2 kg * 3 m/s^2 = 6 N".to_string()),
        },
    ];
    let ok = GroundingValidator.validate(&derived, &ctx).await;
    assert_eq!(ok[0].verdict, Verdict::Pass);

    // Invalid logic derivation (`P -> Q, Q |- P`) must Fail grounding.
    derived.output.claims[0].derivation = Some("P -> Q, Q |- P".to_string());
    let fail = GroundingValidator.validate(&derived, &ctx).await;
    assert!(matches!(fail[0].verdict, Verdict::Fail { .. }));
}

#[test]
fn units_rejects_hostile_nesting_and_invalid_arithmetic_without_panicking() {
    // Deeply nested parentheses exceeding MAX_EXPR_DEPTH.
    let deep = format!("{}1{}", "(".repeat(64), ")".repeat(64));
    assert!(matches!(
        parse_quantity(&deep),
        Err(UnitError::BoundExceeded(_))
    ));

    // Oversized input exceeding MAX_EXPR_BYTES.
    let huge = "1 + ".repeat(700);
    assert!(matches!(
        parse_quantity(&huge),
        Err(UnitError::BoundExceeded(_))
    ));

    // Division by zero and sqrt of odd dimension.
    assert!(matches!(
        parse_quantity("10 m / 0 s"),
        Err(UnitError::Arithmetic(_))
    ));
    assert!(matches!(
        parse_quantity("sqrt(4 m)"),
        Err(UnitError::Arithmetic(_))
    ));
}
