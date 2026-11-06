//! Interactive Courses HTTP contract (#386): owner auth, hidden answers, the sandbox's
//! `unavailable` result, idempotent attempts and the server-side gate. No model is built.
//!
//! `SAIL_WRITE_COURSE_FIXTURE=1` also writes real responses to
//! `tests/fixtures/courses_api_sample.json` for the web client's tests.
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use sail_arena_learn::courses::{CourseEngine, CourseStore, NoSandbox};
use sail_guardian::authority::ApprovalAuthority;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "courses-test-owner-token-0123456789";

fn app() -> axum::Router {
    let engine = CourseEngine::new(CourseStore::open_in_memory().unwrap(), Arc::new(NoSandbox));
    axum::Router::new().nest_service(
        "/api/learn",
        sail_dialogue::learn_courses_http::router(
            Arc::new(engine),
            Some(ApprovalAuthority::new(TOKEN).unwrap()),
        ),
    )
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    auth: bool,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/api/learn{path}"));
    if auth {
        req = req.header("authorization", format!("Bearer {TOKEN}"));
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn courses_api_contract() {
    let app = app();
    let mut sample = serde_json::Map::new();

    let (status, _) = call(&app, "GET", "/courses", None, false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, courses) = call(&app, "GET", "/courses", None, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(courses["courses"][0]["id"], "rust-systems");
    assert_eq!(courses["courses"][0]["next"]["kind"], "diagnostic");
    sample.insert("GET /courses".into(), courses);

    let (_, course) = call(&app, "GET", "/courses/rust-systems", None, true).await;
    assert_eq!(course["chapters"][0]["lessons"][0]["state"], "not_started");
    sample.insert("GET /courses/rust-systems".into(), course);
    let (status, planned) = call(&app, "GET", "/courses/physics-engineering", None, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(planned["chapters"], json!([]));

    let (_, start) = call(
        &app,
        "POST",
        "/diagnostics",
        Some(json!({"course_id": "rust-systems"})),
        true,
    )
    .await;
    let diag = start["id"].as_str().unwrap().to_string();
    sample.insert("POST /diagnostics".into(), start.clone());
    let (_, g) = call(
        &app,
        "POST",
        &format!("/diagnostics/{diag}/attempt"),
        Some(json!({"step_id": "d-move", "attempt_id": "d1", "answer": {"choice": ["b"]}, "confidence": "high"})),
        true,
    )
    .await;
    assert_eq!(g["result"], "correct");
    assert_eq!(g["next"]["id"], "d-copy");
    sample.insert("POST /diagnostics/:id/attempt".into(), g);
    let (status, view) = call(&app, "GET", &format!("/diagnostics/{diag}"), None, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        view["step"]["id"], "d-copy",
        "GET shows the current item without side effects"
    );
    assert_eq!(view["done"], false);
    let (_, resumed) = call(
        &app,
        "POST",
        "/diagnostics",
        Some(json!({"course_id": "rust-systems"})),
        true,
    )
    .await;
    assert_eq!(
        resumed["id"],
        json!(diag),
        "POST resumes the open check instead of starting another"
    );
    sample.insert("GET /diagnostics/:id".into(), view);
    let (_, course) = call(&app, "GET", "/courses/rust-systems", None, true).await;
    assert_eq!(course["diagnostic"]["session_id"], json!(diag));
    let (status, _) = call(
        &app,
        "GET",
        &format!("/diagnostics/{diag}/report"),
        None,
        true,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "course.diagnostic.session_id is reportable"
    );

    let (_, lesson) = call(&app, "GET", "/lessons/rust-own-move", None, true).await;
    let text = lesson.to_string();
    assert!(
        !text.contains("\"key\"")
            && !text.contains("misconceptions")
            && !text.contains("\"solution\"")
    );
    assert_eq!(lesson["progress"]["current_step_id"], "intro");
    sample.insert("GET /lessons/:id".into(), lesson);

    let (_, read) = call(
        &app,
        "POST",
        "/lessons/rust-own-move/attempt",
        Some(json!({"step_id": "intro", "attempt_id": "x1", "answer": null})),
        true,
    )
    .await;
    assert_eq!(read["next_step_id"], "predict-move");

    let wrong = json!({"step_id": "predict-move", "attempt_id": "x2", "answer": {"choice": ["a"]}, "confidence": "high"});
    let (_, g) = call(
        &app,
        "POST",
        "/lessons/rust-own-move/attempt",
        Some(wrong.clone()),
        true,
    )
    .await;
    assert_eq!(g["result"], "incorrect");
    assert_eq!(g["gate_decision"], "retry");
    assert!(!g["feedback"]["explanation"].as_array().unwrap().is_empty());
    sample.insert("POST /lessons/:id/attempt (incorrect)".into(), g.clone());
    let (_, again) = call(
        &app,
        "POST",
        "/lessons/rust-own-move/attempt",
        Some(wrong),
        true,
    )
    .await;
    assert_eq!(again, g, "a retried attempt_id returns the stored grade");

    let (_, hint) = call(
        &app,
        "POST",
        "/lessons/rust-own-move/hint",
        Some(json!({"step_id": "predict-move"})),
        true,
    )
    .await;
    assert_eq!(hint["level"], 1);
    sample.insert("POST /lessons/:id/hint".into(), hint);
    let (_, g) = call(
        &app,
        "POST",
        "/lessons/rust-own-move/attempt",
        Some(json!({"step_id": "predict-move", "attempt_id": "x3", "answer": {"choice": ["c"]}})),
        true,
    )
    .await;
    assert_eq!(g["assistance_state"], "hinted");
    sample.insert("POST /lessons/:id/attempt (correct, hinted)".into(), g);

    let (_, reveal) = call(
        &app,
        "POST",
        "/lessons/rust-own-move/reveal",
        Some(json!({"step_id": "copy-types"})),
        true,
    )
    .await;
    assert_eq!(reveal["assistance_state"], "solution_seen");
    sample.insert("POST /lessons/:id/reveal".into(), reveal);

    let (status, err) = call(
        &app,
        "POST",
        "/lessons/rust-own-move/attempt",
        Some(json!({"step_id": "copy-types", "attempt_id": "x4", "answer": {"order": ["a"]}})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(err["error"].is_string());

    let (status, _) = call(&app, "POST", "/lessons/rust-own-move/advance", None, true).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an unfinished lesson can't advance"
    );

    let (_, rust) = call(&app, "POST", "/lessons/rust-own-borrow/attempt", Some(json!({"step_id": "fix-borrow", "attempt_id": "r1", "answer": {"code": "fn main() { println!(\"9 2\"); }"}})), true).await;
    assert_eq!(
        rust["result"], "unavailable",
        "no sandbox means unavailable, never incorrect"
    );
    assert_eq!(rust["validator_kind"], "rustc");
    sample.insert(
        "POST /lessons/:id/attempt (rust_edit, sandbox unavailable)".into(),
        rust,
    );

    let (_, due) = call(&app, "GET", "/reviews/due?course=rust-systems", None, true).await;
    assert_eq!(due["items"], json!([]));
    sample.insert("GET /reviews/due".into(), due);
    let (_, ev) = call(
        &app,
        "GET",
        "/skills/evidence?course=rust-systems",
        None,
        true,
    )
    .await;
    assert_eq!(ev["objectives"].as_array().unwrap().len(), 4);
    sample.insert("GET /skills/evidence".into(), ev);
    let (_, next) = call(&app, "GET", "/next?course=rust-systems", None, true).await;
    sample.insert("GET /next".into(), next);

    if std::env::var_os("SAIL_WRITE_COURSE_FIXTURE").is_some() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/courses_api_sample.json"
        );
        std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
        std::fs::write(
            path,
            serde_json::to_string_pretty(&Value::Object(sample)).unwrap() + "\n",
        )
        .unwrap();
    }
}
