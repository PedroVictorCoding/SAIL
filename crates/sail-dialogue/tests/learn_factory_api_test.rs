//! Course Factory HTTP contract (library, queue, settings): owner auth, upload, duplicate
//! detection, input validation and the global pause. No model or sandbox is built.
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use sail_arena_learn::factory::{
    jobs::JobQueue, service::FactoryService, settings::SettingsStore, sources::SourceRegistry,
};
use sail_continuity::Continuity;
use sail_guardian::authority::ApprovalAuthority;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "factory-test-owner-token-0123456789";

fn app(blobs: &std::path::Path) -> axum::Router {
    let factory = FactoryService::new(
        Arc::new(SourceRegistry::open_in_memory(blobs.to_path_buf()).unwrap()),
        Arc::new(JobQueue::new(Arc::new(
            Continuity::open_in_memory().unwrap(),
        ))),
        Arc::new(SettingsStore::open_in_memory().unwrap()),
        Arc::new(|| 1_000),
    );
    axum::Router::new().nest_service(
        "/api/learn",
        sail_dialogue::learn_factory_http::router(
            Arc::new(factory),
            Some(ApprovalAuthority::new(TOKEN).unwrap()),
        ),
    )
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn json_req(method: &str, path: &str, body: Option<Value>, auth: bool) -> Request<Body> {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/api/learn{path}"));
    if auth {
        req = req.header("authorization", format!("Bearer {TOKEN}"));
    }
    match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap()
}

fn upload(query: &str, bytes: &'static [u8]) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/api/learn/factory/sources?{query}"))
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/octet-stream")
        .body(Body::from(bytes))
        .unwrap()
}

const NOTES: &[u8] =
    b"# Ownership\n\nEach value has one owner.\n\n## Moves\n\nAssigning moves it.\n";

#[tokio::test]
async fn factory_library_queue_and_settings_contract() {
    let blobs = std::env::temp_dir().join(format!("sail-cf-http-{}", uuid::Uuid::new_v4()));
    let app = app(&blobs);

    // Every route needs the owner.
    for (m, p) in [
        ("GET", "/factory/sources"),
        ("GET", "/factory/jobs"),
        ("GET", "/factory/settings"),
        ("POST", "/factory/jobs/pause"),
    ] {
        let (status, _) = send(&app, json_req(m, p, None, false)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{m} {p}");
    }
    let unauth_upload = Request::builder()
        .method("POST")
        .uri("/api/learn/factory/sources?file_name=n.md")
        .body(Body::from(NOTES))
        .unwrap();
    assert_eq!(send(&app, unauth_upload).await.0, StatusCode::UNAUTHORIZED);

    // Upload: indexed now, restricted by default, duplicate detected.
    let (status, body) = send(
        &app,
        upload("file_name=notes.md&title=Notes&scope=learning:rust", NOTES),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["duplicate"], false);
    assert_eq!(body["source"]["status"]["state"], "indexed");
    assert_eq!(body["source"]["rights"]["kind"], "restricted");
    let id = body["source"]["source_id"].as_str().unwrap().to_string();
    let (_, again) = send(&app, upload("file_name=copy.md&scope=learning:rust", NOTES)).await;
    assert_eq!(again["duplicate"], true);
    assert_eq!(again["source"]["source_id"], id.as_str());

    // Bad input is a 422 with a message, never a 500.
    for q in [
        "file_name=n.md&scope=memory:private",
        "file_name=n.md&rights=public",
        "file_name=n.md&rights=open",
    ] {
        let (status, body) = send(&app, upload(q, NOTES)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{q}: {body}");
        assert!(body["error"].is_string());
    }
    // Blank form fields mean "use the default", as the web form sends them.
    let (status, body) = send(
        &app,
        upload(
            "file_name=blank.md&title=&rights=&license=&scope=",
            b"# Blank\n\nFields.\n",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["source"]["access_scope"], "learning:general");
    assert_eq!(body["source"]["rights"]["kind"], "restricted");
    let blank_id = body["source"]["source_id"].as_str().unwrap().to_string();
    send(
        &app,
        json_req(
            "DELETE",
            &format!("/factory/sources/{blank_id}"),
            None,
            true,
        ),
    )
    .await;

    // Unsupported formats are refused (new bytes: the same bytes would be a duplicate).
    let (status, _) = send(
        &app,
        upload(
            "file_name=tool.exe&scope=learning:rust",
            b"MZ\x90\x00 not a book",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = send(&app, upload("file_name=n.md&bogus=1", NOTES)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unknown query fields are refused"
    );

    // Library lists it with a fragment count; removal hides it.
    let (_, lib) = send(&app, json_req("GET", "/factory/sources", None, true)).await;
    assert_eq!(lib["sources"].as_array().unwrap().len(), 1);
    assert!(lib["sources"][0]["fragment_count"].as_u64().unwrap() >= 2);
    assert_eq!(lib["sources"][0]["title"], "Notes");
    let (status, _) = send(
        &app,
        json_req("DELETE", &format!("/factory/sources/{id}"), None, true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, lib) = send(&app, json_req("GET", "/factory/sources", None, true)).await;
    assert!(lib["sources"].as_array().unwrap().is_empty());
    let (status, _) = send(
        &app,
        json_req("DELETE", "/factory/sources/nope", None, true),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Queue view and the global pause.
    let (_, jobs) = send(&app, json_req("GET", "/factory/jobs", None, true)).await;
    assert_eq!(jobs["paused"], false);
    assert!(jobs["running"].is_null());
    let (_, paused) = send(&app, json_req("POST", "/factory/jobs/pause", None, true)).await;
    assert_eq!(paused["paused"], true);
    let (_, jobs) = send(&app, json_req("GET", "/factory/jobs", None, true)).await;
    assert_eq!(jobs["paused"], true);
    send(&app, json_req("POST", "/factory/jobs/resume", None, true)).await;

    // Settings: defaults, a valid save, an out-of-range value and an unknown field.
    let (_, s) = send(&app, json_req("GET", "/factory/settings", None, true)).await;
    assert_eq!(s["settings"]["idle_only"], true);
    let good = json!({"enabled": true, "idle_only": false,
                      "max_model_minutes_per_day": 30, "context_tokens": 8000});
    let (status, s) = send(&app, json_req("PUT", "/factory/settings", Some(good), true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(s["settings"]["max_model_minutes_per_day"], 30);
    let bad = json!({"enabled": true, "idle_only": false,
                     "max_model_minutes_per_day": 30, "context_tokens": 10});
    let (status, _) = send(&app, json_req("PUT", "/factory/settings", Some(bad), true)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let extra = json!({"enabled": true, "idle_only": false, "max_model_minutes_per_day": 30,
                       "context_tokens": 8000, "skip_validation": true});
    let (status, _) = send(
        &app,
        json_req("PUT", "/factory/settings", Some(extra), true),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let _ = std::fs::remove_dir_all(blobs);
}
