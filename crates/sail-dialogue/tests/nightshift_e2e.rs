//! Round 27 acceptance: one owner errand through the real service and HTTP routes.
//!
//! ask → shaped by the (fake) local model → approve (an edited order naming a missing file is
//! refused) → the night records a verified result and a morning report → the owner reads the
//! diff → Ship opens a draft PR through the (fake) shipper. The night itself is simulated
//! with the store's own transitions; the runner is covered by sail-forge's runner tests.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{Local, Utc};
use sail_dialogue::nightshift::NightShift;
use sail_dialogue::nightshift_http;
use sail_forge::nightshift::{
    Errand, ErrandId, ErrandStore, MorningItem, MorningOutcome, MorningReport, NightItem,
    NightResult, RanOn, RanOnKind, ShaperModel, Shipper, Size, Transition, WorkOrder,
};
use sail_guardian::authority::ApprovalAuthority;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";
const PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs\n\
--- a/src/lib.rs\n\
+++ b/src/lib.rs\n\
@@ -1 +1,2 @@\n\
 pub fn value() -> u32 { 1 }\n\
+// Next dividend date.\n";

fn order() -> WorkOrder {
    WorkOrder {
        title: "Show the next dividend date".into(),
        why: "The owner wants to see upcoming dividends at a glance.".into(),
        acceptance: vec!["The Today card shows the next dividend date".into()],
        target_files: vec!["src/lib.rs".into()],
        size: Size::S,
        notes_for_solver: String::new(),
    }
}

struct FakeShaper;

#[async_trait]
impl ShaperModel for FakeShaper {
    async fn complete(&self, _: &str, _: &str, _: CancellationToken) -> Result<String, String> {
        Ok(serde_json::to_string(&order()).unwrap())
    }
}

#[derive(Default)]
struct FakeShipper {
    shipped: Mutex<Vec<(String, String, String)>>,
}

#[async_trait]
impl Shipper for FakeShipper {
    async fn ship(
        &self,
        errand: &Errand,
        order: &WorkOrder,
        patch: &str,
    ) -> Result<String, String> {
        self.shipped.lock().unwrap().push((
            errand.id.as_str().to_string(),
            order.title.clone(),
            patch.to_string(),
        ));
        Ok("https://github.com/owner/sail/pull/999".into())
    }
}

struct Fixture {
    base: String,
    store: Arc<ErrandStore>,
    shipper: Arc<FakeShipper>,
    dir: std::path::PathBuf,
}

async fn fixture() -> Fixture {
    let dir = std::env::temp_dir().join(format!("sail-nightshift-e2e-{}", uuid::Uuid::new_v4()));
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn value() -> u32 { 1 }\n").unwrap();
    let store = Arc::new(ErrandStore::open(&dir.join("state")).unwrap());
    let shipper = Arc::new(FakeShipper::default());
    let service = NightShift::new(store.clone(), Arc::new(FakeShaper), shipper.clone(), repo);
    let auth = Some(ApprovalAuthority::new(TOKEN).unwrap());
    let app = axum::Router::new().nest_service(
        "/api/nightshift",
        nightshift_http::router(service, None, auth),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Fixture {
        base: format!("http://{addr}/api/nightshift"),
        store,
        shipper,
        dir,
    }
}

async fn overview(client: &reqwest::Client, base: &str) -> Value {
    let response = client.get(base).bearer_auth(TOKEN).send().await.unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

/// Poll the overview until the errand reaches `state` (shaping and shipping run in tasks).
async fn wait_for(client: &reqwest::Client, base: &str, id: &str, state: &str) -> Value {
    for _ in 0..100 {
        let all = overview(client, base).await;
        if let Some(errand) = all["errands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|errand| errand["id"] == id && errand["state"]["state"] == state)
        {
            return errand.clone();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("errand {id} never reached {state}");
}

/// What the runner records for a verified overnight result.
fn simulate_night(store: &ErrandStore, id: &ErrandId) {
    let ran_on = RanOn {
        kind: RanOnKind::Mesh,
        name: "windows-pc".into(),
        model: "qwen".into(),
    };
    store
        .transition(
            id,
            Utc::now(),
            Transition::Working {
                job_id: "job-e2e".into(),
                stage: "coding".into(),
                ran_on: ran_on.clone(),
            },
        )
        .unwrap();
    store.save_patch(id, PATCH).unwrap();
    store
        .transition(
            id,
            Utc::now(),
            Transition::Finished {
                result: NightResult {
                    job_id: "job-e2e".into(),
                    base_commit: "a".repeat(40),
                    ran_on,
                    files: Vec::new(),
                    tests: None,
                    gates: Vec::new(),
                    review: None,
                    summary: "Changed 1 file. Tests did not run.".into(),
                    model_note: None,
                    patch_bytes: PATCH.len() as u64,
                    finished_at: Utc::now(),
                },
                outcome: MorningOutcome::Ready,
                question_or_reason: None,
            },
        )
        .unwrap();
    store.set_morning(MorningReport {
        date: Local::now().format("%Y-%m-%d").to_string(),
        ready: 1,
        needs_you: 0,
        failed: 0,
        not_reached: 0,
        items: vec![MorningItem {
            item: NightItem::Errand { id: id.clone() },
            title: order().title,
            outcome: MorningOutcome::Ready,
            line: "Ready to review.".into(),
        }],
        spoken: "Overnight I finished 1 errand; it's ready for you to review.".into(),
        halted: None,
    });
}

#[tokio::test]
async fn an_owner_errand_goes_from_ask_to_a_draft_pr() {
    let fx = fixture().await;
    let client = reqwest::Client::new();

    // Everything is owner-only.
    let response = client.get(&fx.base).send().await.unwrap();
    assert_eq!(response.status(), 401);

    // Ask → shaped into a proposed work order by the local model.
    let response = client
        .post(format!("{}/errands", fx.base))
        .bearer_auth(TOKEN)
        .json(&json!({"ask": "Tonight, show the next dividend date on the Today card"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let created: Value = response.json().await.unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let proposed = wait_for(&client, &fx.base, &id, "proposed").await;
    assert_eq!(proposed["state"]["order"]["title"], order().title);

    // Nothing ran yet, so there is nothing to ship.
    let response = client
        .post(format!("{}/errands/{id}/ship", fx.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);

    // An edited order naming a file that does not exist is refused with reasons.
    let mut edited = order();
    edited.target_files = vec!["src/missing.rs".into()];
    let response = client
        .post(format!("{}/errands/{id}/approve", fx.base))
        .bearer_auth(TOKEN)
        .json(&json!({"order": edited}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 422);
    let refused: Value = response.json().await.unwrap();
    assert!(!refused["reasons"].as_array().unwrap().is_empty());

    // The order as shaped is approved for tonight and shows in tonight's plan.
    let response = client
        .post(format!("{}/errands/{id}/approve", fx.base))
        .bearer_auth(TOKEN)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let all = overview(&client, &fx.base).await;
    let queued = all["errands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|errand| errand["id"] == id.as_str())
        .unwrap();
    assert_eq!(queued["state"]["state"], "queued");

    // The night produces a verified result and the morning report.
    let errand_id = ErrandId::parse(&id).unwrap();
    simulate_night(&fx.store, &errand_id);
    let all = overview(&client, &fx.base).await;
    assert_eq!(all["morning"]["ready"], 1);
    assert_eq!(all["morning"]["items"][0]["item"]["id"], id.as_str());

    // The owner reads the diff exactly as stored.
    let response = client
        .get(format!("{}/errands/{id}/diff", fx.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()[reqwest::header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/x-diff")
    );
    assert_eq!(response.text().await.unwrap(), PATCH);

    // Ship → a draft PR with that patch; the errand ends Shipped with the PR link.
    let response = client
        .post(format!("{}/errands/{id}/ship", fx.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let shipped = wait_for(&client, &fx.base, &id, "shipped").await;
    assert_eq!(
        shipped["state"]["pr_url"],
        "https://github.com/owner/sail/pull/999"
    );
    let calls = fx.shipper.shipped.lock().unwrap().clone();
    assert_eq!(calls, vec![(id.clone(), order().title, PATCH.to_string())]);

    // A shipped errand can no longer be dropped.
    let response = client
        .post(format!("{}/errands/{id}/drop", fx.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);

    std::fs::remove_dir_all(&fx.dir).ok();
}
