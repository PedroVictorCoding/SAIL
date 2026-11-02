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
