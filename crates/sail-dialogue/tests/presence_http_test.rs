//! Owner-only presence endpoints.
use std::sync::Arc;

use sail_dialogue::presence_http;
use sail_guardian::authority::ApprovalAuthority;
use sail_pulse::Pulse;
use sail_pulse::presence::store::PresenceStore;
use serde_json::{Value, json};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

fn store() -> (Arc<PresenceStore>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "sail-presence-http-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    (Arc::new(PresenceStore::open(&dir).unwrap()), dir)
}

async fn serve(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn presence_requires_the_owner_token() {
    let (presence, dir) = store();
    let auth = Some(ApprovalAuthority::new(TOKEN).unwrap());
    let app = axum::Router::new()
        .nest_service("/api/presence", presence_http::router(presence, None, auth));
    let base = serve(app).await;
    let client = reqwest::Client::new();

    for response in [
        client
            .get(format!("{base}/api/presence"))
            .send()
            .await
            .unwrap(),
        client
            .post(format!("{base}/api/presence/heartbeat"))
            .json(&json!({
                "device_id":"web-1","surface":"web","visible":true,"input_idle_secs":0
            }))
            .send()
            .await
            .unwrap(),
        client
            .get(format!("{base}/api/presence/settings"))
            .send()
            .await
            .unwrap(),
    ] {
        assert_eq!(response.status(), 401);
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn heartbeat_returns_state_and_rejects_bad_ids_and_large_bodies() {
    let (presence, dir) = store();
    let auth = Some(ApprovalAuthority::new(TOKEN).unwrap());
    let pulse = Arc::new(Pulse::open_in_memory().unwrap());
    let app = axum::Router::new().nest_service(
        "/api/presence",
        presence_http::router(presence, Some(pulse), auth),
    );
    let base = serve(app).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/api/presence/heartbeat"))
        .bearer_auth(TOKEN)
        .json(&json!({
            "device_id":"dock-7c1e",
            "surface":"dock",
            "visible":true,
            "input_idle_secs":12,
            "charging":true,
            "voice_ready":true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["state"]["place"], "dock");
    assert_eq!(body["state"]["active_device"], "dock-7c1e");
    assert_eq!(body["state"]["surfaces"][0]["surface"], "dock");
    assert_eq!(body["state"]["surfaces"][0]["input_idle_secs"], 12);
    assert_eq!(body["state"]["surfaces"][0]["can_speak"], true);

    let state: Value = client
        .get(format!("{base}/api/presence"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state["place"], "dock");

    let bad = client
        .post(format!("{base}/api/presence/heartbeat"))
        .bearer_auth(TOKEN)
        .json(&json!({
            "device_id":"bad id",
            "surface":"web",
            "visible":true,
            "input_idle_secs":0
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    let error: Value = bad.json().await.unwrap();
    assert!(error["error"].as_str().unwrap().contains("device_id"));

    let oversized = client
        .post(format!("{base}/api/presence/heartbeat"))
        .bearer_auth(TOKEN)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("x".repeat(5_000))
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), 413);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn settings_round_trip_echoes_clamped_values() {
    let (presence, dir) = store();
    let auth = Some(ApprovalAuthority::new(TOKEN).unwrap());
    let app = axum::Router::new()
        .nest_service("/api/presence", presence_http::router(presence, None, auth));
    let base = serve(app).await;
    let client = reqwest::Client::new();

    let defaults: Value = client
        .get(format!("{base}/api/presence/settings"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(defaults["away_after_secs"], 600);
    assert_eq!(defaults["greet_after_away_secs"], 2_700);
    assert_eq!(defaults["max_spoken_per_hour"], 3);
    assert_eq!(defaults["telegram_when_away"], false);
    assert_eq!(defaults["speak_on"], json!(["web", "dock"]));

    let mut body = defaults.clone();
    body["max_spoken_per_hour"] = json!(99);
    body["telegram_when_away"] = json!(true);
    body["speak_on"] = json!(["phone"]);
    let saved: Value = client
        .post(format!("{base}/api/presence/settings"))
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(saved["max_spoken_per_hour"], 6);
    assert_eq!(saved["telegram_when_away"], true);
    assert_eq!(saved["speak_on"], json!(["phone"]));

    let reread: Value = client
        .get(format!("{base}/api/presence/settings"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reread, saved);

    std::fs::remove_dir_all(&dir).ok();
}
