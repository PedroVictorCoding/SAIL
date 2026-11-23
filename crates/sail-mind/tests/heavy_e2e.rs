//! Mind's half of the C8 loopback acceptance test. Dialogue's companion target
//! exercises the same fixture through its real MeshPicker and Night Shift runner.
#![cfg(target_os = "linux")]

#[path = "support/heavy_e2e.rs"]
mod support;

use std::{sync::atomic::Ordering, time::Duration};

#[tokio::test]
async fn pinned_worker_lends_to_mind_and_pause_interrupts_the_pooled_completion() {
    if !support::enter_fixture(
        "pinned_worker_lends_to_mind_and_pause_interrupts_the_pooled_completion",
    )
    .await
    {
        return;
    }
    let mut stack = support::Stack::start().await;
    let pooled = stack.lane.endpoint().unwrap();
    assert_eq!(pooled.worker, support::WORKER);
    assert_ne!(pooled.url, stack.host_endpoint);
    let request = tokio::spawn(async move {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(pooled.url)
            .json(&serde_json::json!({"messages": []}))
            .send()
            .await
    });
    assert!(
        support::wait_for(
            || stack.model.with_extension("inflight").is_file(),
            Duration::from_secs(3)
        )
        .await
    );
    stack.pause().await;
    assert!(
        tokio::time::timeout(Duration::from_secs(3), request)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(stack.host_completions.load(Ordering::Relaxed), 0);
    stack.mind.shutdown().await.unwrap();
}
