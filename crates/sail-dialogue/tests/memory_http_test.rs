use sail_arena::{MemoryStore, NewRecord, knowledge::*};
use std::sync::Arc;
const TOKEN: &str = "test-memory-owner-token-at-least-32-bytes";
#[tokio::test]
async fn owner_authentication_correction_and_generation_are_enforced() {
    let store = Arc::new(MemoryStore::open_in_memory().unwrap());
    store
        .put(&NewRecord {
            scope: "voice",
            session: "s",
            kind: "turn",
            turn: None,
            gist: "",
            body: "User: Remember: SAIL is local\nSAIL: ok",
            provenance: None,
        })
        .unwrap();
    let claim = store
        .knowledge()
        .consolidate("voice", &Access::owner(), 0, 10)
        .unwrap()
        .claims[0];
    let app = sail_dialogue::memory_http::router(
        store.clone(),
        Some(sail_guardian::authority::ApprovalAuthority::new(TOKEN).unwrap()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let read = format!("http://{addr}/read");
    let change = format!("http://{addr}/change");
    assert_eq!(
        client
            .post(&read)
            .json(&serde_json::json!({"action":"overview"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let r=client.post(&change).bearer_auth(TOKEN).json(&serde_json::json!({"generation":0,"action":{"action":"correct","claim":claim,"value":"private"}})).send().await.unwrap();
    assert!(r.status().is_success(), "{}", r.text().await.unwrap());
    let original = store
        .knowledge()
        .claim(claim, &Access::owner())
        .unwrap()
        .unwrap();
    assert_eq!(original.status, EpistemicStatus::Superseded);
    let current = store
        .knowledge()
        .claims_about(original.subject, &Access::owner(), false)
        .unwrap();
    assert_eq!(current[0].status, EpistemicStatus::UserReported);
    let r=client.post(&change).bearer_auth(TOKEN).json(&serde_json::json!({"generation":0,"action":{"action":"forget_claim","claim":current[0].id}})).send().await.unwrap();
    assert!(r.status().is_success());
    let r=client.post(&change).bearer_auth(TOKEN).json(&serde_json::json!({"generation":0,"action":{"action":"pin","entity":original.subject,"pinned":true}})).send().await.unwrap();
    assert_eq!(r.status(), 409);
    task.abort();
}

#[tokio::test]
async fn learning_requires_owner_and_a_live_session_and_is_not_retroactive() {
    let arena = Arc::new(sail_arena::ArenaSubsystem::new());
    arena.begin_voice_session("live");
    let app = sail_dialogue::memory_http::router_with_arena(
        arena.clone(),
        Some(sail_guardian::authority::ApprovalAuthority::new(TOKEN).unwrap()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("http://{addr}/learning");
    let client = reqwest::Client::new();
    let body = serde_json::json!({"session":"live","enabled":true});
    let denied = client.post(&url).json(&body).send().await.unwrap();
    assert_eq!(denied.status(), 401);
    assert!(arena.learning_epoch("live").is_none());
    let before = arena.learning_epoch("live");
    let enabled = client
        .post(&url)
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(enabled.status().is_success());
    assert_eq!(enabled.headers()["cache-control"], "no-store");
    assert!(arena.learning_epoch("live").is_some());
    assert!(
        arena
            .record_turn_at_epoch("live", 1, "pre-consent words", "ok", before)
            .is_none()
    );
    assert!(
        arena
            .record_turn("live", 2, "Remember: Boat is blue", "ok")
            .is_some()
    );
    let revoked = client
        .post(&url)
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({"session":"live","enabled":false}))
        .send()
        .await
        .unwrap();
    assert!(revoked.status().is_success());
    assert_eq!(arena.store().stats().unwrap().records, 0);
    arena.release_session("live");
    let stale = client
        .post(&url)
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), 400);
    task.abort();
}
