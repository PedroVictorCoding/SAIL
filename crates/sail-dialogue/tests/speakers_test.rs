use base64::{Engine, engine::general_purpose::STANDARD};
use sail_arena::MemoryStore;
use sail_dialogue::speakers::{SpeakerAnalyzer, router_with_analyzer};
use sail_protocol::speaker::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
const TOKEN: &str = "speaker-owner-test-token-with-at-least-32-bytes";
struct Fixture;
#[async_trait::async_trait]
impl SpeakerAnalyzer for Fixture {
    async fn analyze(&self, samples: &[i16]) -> Result<SpeakerOutput, String> {
        let mut embeddings = vec![SpeakerVector {
            speaker: "SPEAKER_00".into(),
            values: vec![1., 0.],
        }];
        let mut segments = vec![SpeakerSegment {
            speaker: "SPEAKER_00".into(),
            start_ms: 0,
            end_ms: 4000,
        }];
        if samples[0] == 2 {
            embeddings.push(SpeakerVector {
                speaker: "SPEAKER_01".into(),
                values: vec![0., 1.],
            });
            segments.push(SpeakerSegment {
                speaker: "SPEAKER_01".into(),
                start_ms: 1000,
                end_ms: 3000,
            });
        }
        Ok(SpeakerOutput {
            version: 1,
            model_revision: "a".repeat(64),
            segments,
            embeddings,
        })
    }
}
async fn call(router: &axum::Router, body: Value, auth: bool) -> (u16, Value) {
    let mut req = axum::http::Request::post("/request").header("content-type", "application/json");
    if auth {
        req = req.header("authorization", format!("Bearer {TOKEN}"));
    }
    let r = router
        .clone()
        .oneshot(req.body(axum::body::Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = r.status().as_u16();
    assert_eq!(r.headers()["cache-control"], "no-store");
    let bytes = axum::body::to_bytes(r.into_body(), 4_000_000)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test]
async fn owner_workflow_enroll_identify_verify_reopen_forget_and_reject_mixed_speakers() {
    let path = std::env::temp_dir().join(format!("speaker-{}.db", uuid::Uuid::new_v4()));
    let store = Arc::new(MemoryStore::open(&path).unwrap());
    let router = router_with_analyzer(store.clone(), Arc::new(Fixture), Some(TOKEN));
    let pcm = STANDARD.encode(vec![0; 16_000 * 4 * 2]);
    let id = uuid::Uuid::new_v4().to_string();
    assert_eq!(call(&router, json!({"action":"list"}), false).await.0, 401);
    let unknown = call(&router, json!({"action":"analyze","pcm":pcm}), true).await;
    assert_eq!(unknown.0, 200);
    assert_eq!(unknown.1["matches"][0]["decision"], "unknown");
    assert!(unknown.1.get("embeddings").is_none());
    assert_eq!(
        call(
            &router,
            json!({"action":"enroll","pcm":pcm,"name":"Alice","request_id":id,"consent":false}),
            true
        )
        .await
        .0,
        400
    );
    assert_eq!(
        call(
            &router,
            json!({"action":"enroll","pcm":pcm,"name":"Alice","request_id":id,"consent":true}),
            true
        )
        .await
        .0,
        200
    );
    assert_eq!(
        call(
            &router,
            json!({"action":"enroll","pcm":pcm,"name":"Alice","request_id":id,"consent":true}),
            true
        )
        .await
        .0,
        409
    );
    let sealed = store.speaker_profiles().unwrap().1.pop().unwrap().sealed;
    assert!(!String::from_utf8_lossy(&sealed).contains("Alice"));
    let matched = call(
        &router,
        json!({"action":"verify","pcm":pcm,"profile_id":id}),
        true,
    )
    .await;
    assert_eq!(matched.1["matches"][0]["name"], "Alice");
    assert_eq!(matched.1["advisory_only"], true);
    let mut mixed = vec![0u8; 16_000 * 4 * 2];
    mixed[0] = 2;
    assert_eq!(call(&router,json!({"action":"enroll","pcm":STANDARD.encode(mixed),"name":"Mixed","request_id":uuid::Uuid::new_v4(),"consent":true}),true).await.0,422);
    drop(router);
    drop(store);
    let store = Arc::new(MemoryStore::open(&path).unwrap());
    let router = router_with_analyzer(store.clone(), Arc::new(Fixture), Some(TOKEN));
    assert_eq!(
        call(&router, json!({"action":"list"}), true).await.1["profiles"][0]["name"],
        "Alice"
    );
    assert_eq!(
        call(&router, json!({"action":"delete","id":id}), true)
            .await
            .0,
        200
    );
    assert!(
        call(&router, json!({"action":"list"}), true).await.1["profiles"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let now = call(&router, json!({"action":"analyze","pcm":pcm}), true).await;
    assert_eq!(now.1["matches"][0]["decision"], "unknown");
    drop(router);
    drop(store);
    let _ = std::fs::remove_file(path);
}
struct Waiting {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl SpeakerAnalyzer for Waiting {
    async fn analyze(&self, samples: &[i16]) -> Result<SpeakerOutput, String> {
        self.started.notify_one();
        self.release.notified().await;
        Fixture.analyze(samples).await
    }
}
#[tokio::test]
async fn deletion_during_enrollment_prevents_the_late_write() {
    let store = Arc::new(MemoryStore::open_in_memory().unwrap());
    let backend = Arc::new(Waiting {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let router = router_with_analyzer(store.clone(), backend.clone(), Some(TOKEN));
    let id = uuid::Uuid::new_v4().to_string();
    let request_router = router.clone();
    let request_id = id.clone();
    let job = tokio::spawn(async move {
        call(&request_router,json!({"action":"enroll","pcm":STANDARD.encode(vec![0;128000]),"name":"Alice","request_id":request_id,"consent":true}),true).await
    });
    backend.started.notified().await;
    assert_eq!(
        call(
            &router,
            json!({"action":"analyze","pcm":STANDARD.encode(vec![0;128000])}),
            true
        )
        .await
        .0,
        429
    );
    assert_eq!(
        call(&router, json!({"action":"delete","id":id}), true)
            .await
            .0,
        200
    );
    backend.release.notify_one();
    assert_eq!(job.await.unwrap().0, 409);
    assert!(store.speaker_profiles().unwrap().1.is_empty());
}
struct Unavailable;
#[async_trait::async_trait]
impl SpeakerAnalyzer for Unavailable {
    async fn analyze(&self, _: &[i16]) -> Result<SpeakerOutput, String> {
        Err("test unavailable".into())
    }
}
#[tokio::test]
async fn absent_model_and_invalid_audio_do_not_create_profiles() {
    let store = Arc::new(MemoryStore::open_in_memory().unwrap());
    let router = router_with_analyzer(store.clone(), Arc::new(Unavailable), Some(TOKEN));
    assert_eq!(
        call(&router, json!({"action":"analyze","pcm":"invalid!"}), true)
            .await
            .0,
        400
    );
    assert_eq!(
        call(
            &router,
            json!({"action":"analyze","pcm":STANDARD.encode(vec![0;128000])}),
            true
        )
        .await
        .0,
        503
    );
    assert!(store.speaker_profiles().unwrap().1.is_empty());
    let disabled = router_with_analyzer(store, Arc::new(Unavailable), None);
    assert_eq!(call(&disabled, json!({"action":"list"}), true).await.0, 503);
}
