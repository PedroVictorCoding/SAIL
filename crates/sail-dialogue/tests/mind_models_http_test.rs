//! Default reasoning model routes: owner auth, catalog view, saving an installed model and
//! refusing unknown or missing ones. Uses a temporary data root and model directory, and
//! never starts a model.
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use sail_guardian::authority::ApprovalAuthority;
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "mind-models-test-owner-token-0123456789";

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    auth: bool,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/api/mind/models{path}"));
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
async fn default_model_can_be_listed_and_saved_for_the_next_start() {
    let base = std::env::temp_dir().join(format!("sail-mind-models-{}", uuid::Uuid::new_v4()));
    let (data, runtime) = (base.join("data"), base.join("runtime"));
    std::fs::create_dir_all(data.join("models")).unwrap();
    std::fs::create_dir_all(runtime.join("models")).unwrap();
    // SAFETY: this test binary has a single test, so nothing reads the environment concurrently.
    unsafe {
        std::env::set_var("SAIL_DATA", &data);
        std::env::set_var("SAIL_DATA_ALLOW_UNMOUNTED", "1");
        std::env::set_var("SAIL_RUNTIME", &runtime);
        std::env::remove_var("SAIL_MODEL_PATH");
    }
    // Qwen (running) and the small LFM3 model with its projector are installed; LFM2.5 isn't.
    for file in [
        "Qwen3.6-35B-A3B-Q4_K_M.gguf",
        "LFM3-Omni-2B-Q4_K_M.gguf",
        "mmproj-LFM3-Omni-2B-Q4_K_M.gguf",
    ] {
        std::fs::write(data.join("models").join(file), b"gguf").unwrap();
    }
    let running = sail_common::models::primary_model_path();
    assert!(
        running.ends_with("Qwen3.6-35B-A3B-Q4_K_M.gguf"),
        "{}",
        running.display()
    );
    let app = axum::Router::new().nest_service(
        "/api/mind/models",
        sail_dialogue::mind_models_http::router(
            Some(ApprovalAuthority::new(TOKEN).unwrap()),
            running,
        ),
    );

    let (status, _) = call(&app, "GET", "/", None, false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, view) = call(&app, "GET", "/", None, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["running"], "qwen3.6-35b-a3b");
    assert_eq!(view["default"], "qwen3.6-35b-a3b");
    assert_eq!(view["restart_required"], false);
    let installed: Vec<_> = view["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["id"].as_str().unwrap().to_string(),
                m["installed"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        installed,
        vec![
            ("qwen3.6-35b-a3b".to_string(), true),
            ("lfm2.5-24b-a2b-uncensored".to_string(), false),
            ("lfm3-omni-2b".to_string(), true),
        ]
    );

    let (status, _) = call(
        &app,
        "PUT",
        "/default",
        Some(json!({"model": "nope"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, err) = call(
        &app,
        "PUT",
        "/default",
        Some(json!({"model": "lfm2.5-24b-a2b-uncensored"})),
        true,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a model that isn't installed can't be the default"
    );
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("LFM2.5-24B-A2B-Uncensored-AD-Q4_K_M.gguf")
    );

    let (status, view) = call(
        &app,
        "PUT",
        "/default",
        Some(json!({"model": "lfm3-omni-2b"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["default"], "lfm3-omni-2b");
    assert_eq!(
        view["running"], "qwen3.6-35b-a3b",
        "nothing reloads until SAIL restarts"
    );
    assert_eq!(view["restart_required"], true);
    let saved = std::fs::read_to_string(data.join("mind/default-model.json")).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&saved).unwrap()["model"],
        "lfm3-omni-2b"
    );
    // The next start resolves the saved default.
    assert!(sail_common::models::primary_model_path().ends_with("LFM3-Omni-2B-Q4_K_M.gguf"));

    let _ = std::fs::remove_dir_all(&base);
}
