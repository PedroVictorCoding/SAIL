//! Integration test for the Return Briefing HTTP API (`/api/briefing`).

use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

use sail_bridge::spotify::Spotify;
use sail_dialogue::briefing::BriefingEngine;
use sail_dialogue::briefing_http;
use sail_guardian::authority::ApprovalAuthority;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

async fn setup_test_server() -> (String, PathBuf, tokio::task::JoinHandle<()>) {
    let tmp =
        std::env::temp_dir().join(format!("sail-briefing-http-test-{}", uuid::Uuid::new_v4()));
    let spotify = Arc::new(Spotify::open(tmp.join("spotify")));
    let engine = Arc::new(BriefingEngine::new(spotify, None, None, None, tmp.clone()));
    let authority = Some(ApprovalAuthority::new(TOKEN).unwrap());
    let router = briefing_http::router(engine, authority);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    (format!("http://{addr}"), tmp, handle)
}

#[tokio::test]
async fn latest_is_readable_without_auth_and_returns_complete_briefing() {
    let (base_url, dir, handle) = setup_test_server().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{base_url}/latest"))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::OK);

    let val: Value = res.json().await.unwrap();
    let briefing = &val["briefing"];

    assert!(briefing["id"].is_string());
    assert!(briefing["greeting"].is_string());
    assert!(briefing["staged_music"].is_object());
    assert!(briefing["learning_plan"].is_object());
    assert!(briefing["events_to_watch"].is_array());
    assert!(briefing["news_radar"].is_array());
    assert!(briefing["project_continuity"].is_object());
    assert!(briefing["system_sentinel"].is_object());

    handle.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn refresh_requires_owner_token() {
    let (base_url, dir, handle) = setup_test_server().await;
    let client = reqwest::Client::new();

    // 1. Missing token -> 401
    let res = client
        .post(format!("{base_url}/refresh"))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::UNAUTHORIZED);

    // 2. Valid token -> 200
    let res = client
        .post(format!("{base_url}/refresh"))
        .header("Authorization", format!("Bearer {TOKEN}"))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::OK);

    handle.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn play_music_and_dismiss_actions() {
    let (base_url, dir, handle) = setup_test_server().await;
    let client = reqwest::Client::new();

    // Play music action with valid token
    let play_body = json!({
        "uri": "sail:ambient:focus"
    });
    let res = client
        .post(format!("{base_url}/actions/play-music"))
        .header("Authorization", format!("Bearer {TOKEN}"))
        .json(&play_body)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::OK);
    let val: Value = res.json().await.unwrap();
    assert_eq!(val["ok"], true);

    // Dismiss action
    let res = client
        .post(format!("{base_url}/actions/dismiss"))
        .header("Authorization", format!("Bearer {TOKEN}"))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), reqwest::StatusCode::OK);

    handle.abort();
    let _ = std::fs::remove_dir_all(dir);
}
