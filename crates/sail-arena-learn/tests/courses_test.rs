//! Interactive Courses (#386) acceptance: evidence honesty, the review gate and sandbox
//! semantics, with an injected clock and no model anywhere.
use sail_arena_learn::courses::content::{RunLimits, catalog};
use sail_arena_learn::courses::grade::{Diagnostic, RustRun, parse_rustc_json};
use sail_arena_learn::courses::{
    AttemptRequest, CourseEngine, CourseStore, NoSandbox, RustEvaluator,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};

const DAY: i64 = 24 * 60 * 60 * 1000;
const T0: i64 = 1_793_000_000_000;

struct Fixture {
    engine: CourseEngine,
    now: Arc<AtomicI64>,
    seq: AtomicU32,
}

impl Fixture {
    fn new() -> Self {
        Self::with_rust(Arc::new(NoSandbox))
    }

    fn with_rust(rust: Arc<dyn RustEvaluator>) -> Self {
        let now = Arc::new(AtomicI64::new(T0));
        let clock = now.clone();
        let engine = CourseEngine::with_clock(
            CourseStore::open_in_memory().unwrap(),
            rust,
            Arc::new(move || clock.load(Ordering::SeqCst)),
        );
        Self {
            engine,
            now,
            seq: AtomicU32::new(0),
        }
    }

    fn advance_days(&self, days: i64) {
        self.now.fetch_add(days * DAY, Ordering::SeqCst);
    }

    fn req(&self, step: Option<&str>, answer: Value) -> AttemptRequest {
        let n = self.seq.fetch_add(1, Ordering::SeqCst);
        serde_json::from_value(json!({
            "step_id": step,
            "attempt_id": format!("a-{n}"),
            "answer": answer,
        }))
        .unwrap()
    }

    async fn lesson(&self, lesson: &str, step: &str, answer: Value) -> Value {
        self.engine
            .lesson_attempt(lesson, self.req(Some(step), answer))
            .await
            .unwrap()
    }

    fn state(&self, objective: &str) -> String {
        let ev = self.engine.evidence("rust-systems").unwrap();
        ev["objectives"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["objective_id"] == objective)
            .unwrap()["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Completes lesson 1 with every scored step right and no help.
    async fn pass_lesson_one(&self) {
        let l = "rust-own-move";
        self.lesson(l, "intro", Value::Null).await;
        self.lesson(l, "predict-move", json!({"choice": ["c"]}))
            .await;
        self.lesson(
            l,
            "copy-types",
            json!({"choice": ["i32", "bool", "tuple", "str-ref"]}),
        )
        .await;
        self.lesson(l, "fill-clone", json!({"text": " clone() "}))
            .await;
        self.lesson(l, "predict-copy", json!({"choice": ["a"]}))
            .await;
        let last = self
            .lesson(l, "predict-fn-move", json!({"choice": ["b"]}))
            .await;
        assert_eq!(last["gate_decision"], "lesson_complete");
    }
}

#[test]
fn embedded_packages_validate_and_views_hide_answers() {
    for track in catalog() {
        let Some(p) = track.package else { continue };
        p.validate().unwrap();
        for l in &p.lessons {
            for s in &l.steps {
                let view = serde_json::to_string(&s.view(&l.id, true)).unwrap();
                for secret in s.hints.iter().chain(s.misconceptions.values()) {
                    assert!(!view.contains(secret.as_str()), "{} leaks {secret}", s.id);
                }
                // A solution may restate a visible option; that's not a leak.
                for b in s
                    .solution
                    .iter()
                    .filter(|b| !s.options.iter().any(|o| o.text == b.text))
                {
                    assert!(!view.contains(&b.text), "{} leaks its solution", s.id);
                }
                assert!(!view.contains("\"key\""));
            }
        }
    }
}

#[test]
fn catalog_lists_planned_tracks_without_fake_lessons() {
    let f = Fixture::new();
    let c = f.engine.courses().unwrap();
    let courses = c["courses"].as_array().unwrap();
    assert_eq!(courses.len(), 3);
    assert_eq!(courses[0]["status"], "ready");
    assert_eq!(courses[1]["status"], "ready");
    for planned in &courses[2..] {
        assert_eq!(planned["status"], "planned");
        assert_eq!(planned["lesson_count"], 0);
    }
}

#[tokio::test]
async fn fresh_learner_is_untested_and_starts_with_the_placement_check() {
    let f = Fixture::new();
    for o in [
        "rust.move",
        "rust.copy",
        "rust.borrow-shared",
        "rust.borrow-mut",
    ] {
        assert_eq!(f.state(o), "untested");
    }
    assert_eq!(f.engine.next("rust-systems").unwrap()["kind"], "diagnostic");
}

#[tokio::test]
async fn diagnostic_skips_items_whose_prerequisite_failed() {
    let f = Fixture::new();
    let start = f.engine.diagnostic_start("rust-systems").await.unwrap();
    let id = start["id"].as_str().unwrap().to_string();
    assert_eq!(start["step"]["id"], "d-move");
    assert_eq!(start["step"]["hint_count"], 0);
    // Wrong on moves: the shared-borrow item depends on it and must be skipped.
    let g = f
        .engine
        .diagnostic_attempt(&id, f.req(Some("d-move"), json!({"choice": ["a"]})))
        .await
        .unwrap();
    assert_eq!(g["result"], "incorrect");
    assert_eq!(g["next"]["id"], "d-copy");
    let g = f
        .engine
        .diagnostic_attempt(&id, f.req(Some("d-copy"), json!({"choice": ["yes"]})))
        .await
        .unwrap();
    assert_eq!(g["next"]["id"], "d-mut");
    let g = f
        .engine
        .diagnostic_attempt(&id, f.req(Some("d-mut"), json!({"text": ""})))
        .await
        .unwrap();
    assert_eq!(g["done"], true);
    let report = f.engine.diagnostic_report(&id).unwrap();
    assert!(
        report["untested"]
            .as_array()
            .unwrap()
            .contains(&json!("rust.borrow-shared"))
    );
    assert_eq!(f.state("rust.copy"), "provisional");
    assert_eq!(f.state("rust.move"), "weak");
    assert_eq!(f.state("rust.borrow-shared"), "untested");
}

#[tokio::test]
async fn hinted_pass_is_assisted_and_never_certifies() {
    let f = Fixture::new();
    let l = "rust-own-move";
    f.lesson(l, "intro", Value::Null).await;
    let hint = f
        .engine
        .hint(
            sail_arena_learn::courses::Context::Lesson,
            l,
            Some("predict-move"),
        )
        .await
        .unwrap();
    assert_eq!(hint["level"], 1);
    let g = f.lesson(l, "predict-move", json!({"choice": ["c"]})).await;
    assert_eq!(g["result"], "correct");
    assert_eq!(g["assistance_state"], "hinted");
    assert_eq!(g["provisional_vs_retained"], "none");
    // The fill-clone step is the only other rust.move evidence: reveal it, then answer it.
    f.lesson(
        l,
        "copy-types",
        json!({"choice": ["i32", "bool", "tuple", "str-ref"]}),
    )
    .await;
    f.engine.reveal(l, "fill-clone").await.unwrap();
    let g = f.lesson(l, "fill-clone", json!({"text": "clone"})).await;
    assert_eq!(g["assistance_state"], "solution_seen");
    f.lesson(l, "predict-copy", json!({"choice": ["a"]})).await;
    // A wrong answer then a right retry, independent: rust.move gets one independent pass.
    let g = f
        .lesson(l, "predict-fn-move", json!({"choice": ["a"]}))
        .await;
    assert_eq!(g["gate_decision"], "retry");
    let g = f
        .lesson(l, "predict-fn-move", json!({"choice": ["b"]}))
        .await;
    assert_eq!(g["assistance_state"], "independent");
    assert_eq!(f.state("rust.copy"), "provisional");
}

#[tokio::test]
async fn assisted_only_objective_blocks_certification() {
    let f = Fixture::new();
    let l = "rust-own-move";
    f.lesson(l, "intro", Value::Null).await;
    for (step, ans) in [
        ("predict-move", json!({"choice": ["c"]})),
        (
            "copy-types",
            json!({"choice": ["i32", "bool", "tuple", "str-ref"]}),
        ),
        ("fill-clone", json!({"text": "clone"})),
        ("predict-copy", json!({"choice": ["a"]})),
        ("predict-fn-move", json!({"choice": ["b"]})),
    ] {
        if step != "predict-copy" && step != "copy-types" {
            f.engine.reveal(l, step).await.unwrap();
        }
        f.lesson(l, step, ans).await;
    }
    assert_eq!(f.state("rust.move"), "assisted");
    let adv = f.engine.advance(l).await.unwrap();
    assert_eq!(adv["certified"], false);
    assert!(
        adv["gate_reasons"][0]
            .as_str()
            .unwrap()
            .contains("without help")
    );
}

#[tokio::test]
async fn independent_today_is_provisional_then_delayed_unseen_pass_is_retained() {
    let f = Fixture::new();
    f.pass_lesson_one().await;
    assert_eq!(f.state("rust.move"), "provisional");
    let adv = f.engine.advance("rust-own-move").await.unwrap();
    assert_eq!(adv["certified"], true);
    // Same day: nothing is due, and a review can't be taken early.
    assert_eq!(
        f.engine.reviews_due("rust-systems").unwrap()["items"],
        json!([])
    );
    assert!(
        f.engine
            .review_attempt("r-move-struct", f.req(None, json!({"choice": ["b"]})))
            .await
            .is_err()
    );

    f.advance_days(1);
    assert_eq!(f.state("rust.move"), "due");
    let due = f.engine.reviews_due("rust-systems").unwrap();
    let items = due["items"].as_array().unwrap();
    let mv = items
        .iter()
        .find(|i| i["objective_id"] == "rust.move")
        .unwrap();
    let review_id = mv["id"].as_str().unwrap().to_string();
    assert!(
        review_id.starts_with("r-move"),
        "unseen variant for rust.move, got {review_id}"
    );
    let g = f
        .engine
        .review_attempt(&review_id, f.req(None, json!({"choice": ["b"]})))
        .await
        .unwrap();
    assert_eq!(g["result"], "correct");
    assert_eq!(f.state("rust.move"), "retained");
    assert_eq!(g["provisional_vs_retained"], "retained");
}

#[tokio::test]
async fn due_reviews_block_certified_advance_server_side() {
    let f = Fixture::new();
    f.pass_lesson_one().await;
    f.engine.advance("rust-own-move").await.unwrap();
    f.advance_days(1);
    // Do lesson two (reading is never blocked), then try to certify it.
    let l = "rust-own-borrow";
    f.lesson(l, "intro", Value::Null).await;
    f.lesson(l, "predict-lend", json!({"choice": ["a"]})).await;
    f.lesson(l, "which-line", json!({"choice": ["l4"]})).await;
    f.lesson(
        l,
        "order-mut",
        json!({"order": ["decl", "borrow", "add", "print"]}),
    )
    .await;
    let g = f
        .lesson(l, "fix-borrow", json!({"code": "fn main() {}"}))
        .await;
    assert_eq!(g["result"], "unavailable");
    f.lesson(l, "predict-two-mut", json!({"choice": ["a"]}))
        .await;
    let adv = f.engine.advance(l).await.unwrap();
    assert_eq!(adv["gate_decision"], "blocked_by_review");
    assert_eq!(adv["certified"], false);
    assert!(!adv["due"].as_array().unwrap().is_empty());
    assert_eq!(f.engine.next("rust-systems").unwrap()["kind"], "diagnostic");
}

#[tokio::test]
async fn delayed_failure_is_weak_and_rescheduled_not_green() {
    let f = Fixture::new();
    f.pass_lesson_one().await;
    f.advance_days(1);
    let due = f.engine.reviews_due("rust-systems").unwrap();
    let mv = due["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["objective_id"] == "rust.move")
        .unwrap()
        .clone();
    let g = f
        .engine
        .review_attempt(
            mv["id"].as_str().unwrap(),
            f.req(None, json!({"choice": ["a"]})),
        )
        .await
        .unwrap();
    assert_eq!(g["result"], "incorrect");
    assert_eq!(f.state("rust.move"), "weak");
    let ev = f.engine.evidence("rust-systems").unwrap();
    let o = ev["objectives"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["objective_id"] == "rust.move")
        .unwrap()
        .clone();
    assert!(o["retention_due_at"].is_string());
    assert!(o["last_delayed_success"].is_null());
    // Next day: a different variant is offered.
    f.advance_days(1);
    let due = f.engine.reviews_due("rust-systems").unwrap();
    let again = due["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["objective_id"] == "rust.move")
        .unwrap()
        .clone();
    assert_ne!(again["id"], mv["id"]);
}

#[tokio::test]
async fn backlog_gate_opens_after_a_bounded_session() {
    let f = Fixture::new();
    f.pass_lesson_one().await;
    f.advance_days(1);
    let due = f.engine.reviews_due("rust-systems").unwrap();
    assert!(due["catch_up"]["total_due"].as_u64().unwrap() >= 1);
    for item in due["items"].as_array().unwrap() {
        // Wrong answers still finish the session: the gate must not starve the learner.
        f.engine
            .review_attempt(
                item["id"].as_str().unwrap(),
                f.req(None, json!({"choice": ["zz"]})),
            )
            .await
            .ok();
        let any = item["step"]["options"][0]["id"].clone();
        f.engine
            .review_attempt(
                item["id"].as_str().unwrap(),
                f.req(None, json!({"choice": [any]})),
            )
            .await
            .unwrap();
    }
    let after = f.engine.reviews_due("rust-systems").unwrap();
    assert_eq!(after["items"], json!([]));
}

#[tokio::test]
async fn attempts_are_idempotent_by_attempt_id() {
    let f = Fixture::new();
    let req: AttemptRequest = serde_json::from_value(json!({
        "step_id": "predict-move", "attempt_id": "same-1", "answer": {"choice": ["c"]}
    }))
    .unwrap();
    let a = f
        .engine
        .lesson_attempt("rust-own-move", req.clone())
        .await
        .unwrap();
    let b = f.engine.lesson_attempt("rust-own-move", req).await.unwrap();
    assert_eq!(a, b);
    let ev = f.engine.evidence("rust-systems").unwrap();
    let mv = ev["objectives"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["objective_id"] == "rust.move")
        .unwrap()
        .clone();
    assert_eq!(mv["independent_passes"], 1);
}

#[tokio::test]
async fn bad_answer_shape_is_a_client_error_not_a_wrong_answer() {
    let f = Fixture::new();
    let err = f
        .engine
        .lesson_attempt(
            "rust-own-move",
            f.req(Some("predict-move"), json!({"choice": ["nope"]})),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        sail_arena_learn::courses::CourseError::Invalid(_)
    ));
    assert_eq!(f.state("rust.move"), "untested");
}

struct FakeSandbox {
    run: RustRun,
}

#[async_trait::async_trait]
impl RustEvaluator for FakeSandbox {
    async fn run(&self, _source: &str, _limits: RunLimits) -> RustRun {
        self.run.clone()
    }
}

#[tokio::test]
async fn rust_edit_is_graded_from_sandbox_output_only() {
    let ok = Fixture::with_rust(Arc::new(FakeSandbox {
        run: RustRun::Compiled {
            exit_code: Some(0),
            stdout: "9 2\n".into(),
            diagnostics: vec![],
        },
    }));
    let g = ok
        .lesson(
            "rust-own-borrow",
            "fix-borrow",
            json!({"code": "fn main(){}"}),
        )
        .await;
    assert_eq!(g["result"], "correct");
    assert_eq!(g["validator_kind"], "rustc");

    let fail = Fixture::with_rust(Arc::new(FakeSandbox {
        run: RustRun::CompileFailed {
            diagnostics: vec![Diagnostic {
                line: 7,
                column: 21,
                level: "error".into(),
                message: "[E0308] mismatched types".into(),
            }],
        },
    }));
    let g = fail
        .lesson(
            "rust-own-borrow",
            "fix-borrow",
            json!({"code": "fn main(){}"}),
        )
        .await;
    assert_eq!(g["result"], "incorrect");
    assert_eq!(g["feedback"]["diagnostics"][0]["line"], 7);

    let none = Fixture::new();
    let g = none
        .lesson(
            "rust-own-borrow",
            "fix-borrow",
            json!({"code": "fn main(){}"}),
        )
        .await;
    assert_eq!(g["result"], "unavailable");
    assert!(g["feedback"]["unavailable_reason"].is_string());
    assert_eq!(none.state("rust.borrow-shared"), "exposure");
}

#[test]
fn rustc_json_diagnostics_map_to_lines() {
    let stderr = concat!(
        r#"{"$message_type":"diagnostic","message":"mismatched types","code":{"code":"E0308"},"level":"error","spans":[{"line_start":7,"column_start":21,"is_primary":true}]}"#,
        "\n",
        r#"{"$message_type":"diagnostic","message":"aborting due to 1 previous error","code":null,"level":"error","spans":[]}"#,
        "\nnot json\n"
    );
    let d = parse_rustc_json(stderr);
    assert_eq!(d.len(), 1);
    assert_eq!((d[0].line, d[0].column), (7, 21));
    assert_eq!(d[0].message, "[E0308] mismatched types");
}

/// A tiny generated course, as the Course Factory would publish it.
fn generated_track(revision: u64, title: &str) -> sail_arena_learn::courses::content::Track {
    use sail_arena_learn::courses::content::{CoursePackage, Domain, Track};
    let package: CoursePackage = serde_json::from_value(json!({
        "id": "gen-rust", "version": format!("r{revision}"), "title": title,
        "domain": "rust", "summary": "Generated from the owner's books.",
        "objectives": [{ "id": "gen.move", "title": "Moves" }],
        "chapters": [{ "id": "gen-ch1", "title": "Ownership", "lessons": ["gen-l1"] }],
        "lessons": [{
            "id": "gen-l1", "title": "Moves", "minutes": 4, "objective_ids": ["gen.move"],
            "steps": [{
                "id": "s1", "kind": "choice", "family": "gen-move", "title": "Pick one",
                "objective_ids": ["gen.move"],
                "options": [{ "id": "a", "text": "One" }, { "id": "b", "text": "Two" }],
                "key": { "choice": ["a"] }
            }]
        }],
        "diagnostic": null,
        "reviews": [{
            "id": "gen-r1", "kind": "choice", "family": "gen-move-review", "title": "Again",
            "objective_ids": ["gen.move"],
            "options": [{ "id": "x", "text": "Three" }, { "id": "y", "text": "Four" }],
            "key": { "choice": ["y"] }
        }]
    }))
    .unwrap();
    Track {
        id: "gen-rust".into(),
        title: title.into(),
        domain: Domain::Rust,
        summary: "Generated from the owner's books.".into(),
        package: Some(Arc::new(package)),
        generated: Some(revision),
    }
}

#[tokio::test]
async fn published_generated_course_is_served_and_only_newer_revisions_replace_it() {
    let f = Fixture::new();
    f.engine
        .publish_generated(generated_track(2, "Second"))
        .unwrap();
    let course = f.engine.course("gen-rust").unwrap();
    assert_eq!(course["course"]["title"], "Second");
    let lesson = f.engine.lesson_view("gen-l1").unwrap();
    assert!(
        !lesson.to_string().contains("\"key\""),
        "answer key leaked: {lesson}"
    );

    // An older (or equal) revision never replaces a newer one.
    f.engine
        .publish_generated(generated_track(1, "First"))
        .unwrap();
    assert_eq!(
        f.engine.course("gen-rust").unwrap()["course"]["title"],
        "Second"
    );
    f.engine
        .publish_generated(generated_track(3, "Third"))
        .unwrap();
    assert_eq!(
        f.engine.course("gen-rust").unwrap()["course"]["title"],
        "Third"
    );
}

#[tokio::test]
async fn generated_course_cannot_replace_a_builtin_or_reuse_its_lesson_ids() {
    let f = Fixture::new();
    let mut builtin = generated_track(1, "Hijack");
    builtin.id = "rust-systems".into();
    Arc::get_mut(builtin.package.as_mut().unwrap()).unwrap().id = "rust-systems".into();
    let err = f.engine.publish_generated(builtin).unwrap_err().to_string();
    assert!(err.contains("built-in") || err.contains("clash"), "{err}");

    let mut clash = generated_track(1, "Clash");
    let pkg = Arc::get_mut(clash.package.as_mut().unwrap()).unwrap();
    pkg.lessons[0].id = "rust-own-move".into();
    pkg.chapters[0].lessons = vec!["rust-own-move".into()];
    let err = f.engine.publish_generated(clash).unwrap_err().to_string();
    assert!(err.contains("clash"), "{err}");
}
