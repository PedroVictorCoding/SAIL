//! Round 29b G3: `/api/mind/mesh` compute overview and `POST /workers/:name/lend`
//! contract over real HTTP against a stub pool and the G2 fixtures.

use std::sync::{Arc, Mutex};

use axum::{Json, http::StatusCode};
use sail_dialogue::mesh_http::{self, Failure, MeshComputeBackend};
use sail_guardian::authority::ApprovalAuthority;
use sail_mind::protocol::mesh::LendMode;
use serde_json::{Value, json};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef";

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/ui/fixtures/mesh_compute")
        .join(name);
    let raw = std::fs::read_to_string(&path).expect("fixture readable");
    serde_json::from_str(&raw).expect("fixture valid json")
}

struct StubPool {
    overview: Mutex<Value>,
}

impl StubPool {
    fn new(initial: Value) -> Self {
        Self {
            overview: Mutex::new(initial),
        }
    }
}

#[async_trait::async_trait]
impl MeshComputeBackend for StubPool {
    async fn overview(&self) -> Value {
        self.overview.lock().unwrap().clone()
    }

    async fn refresh(&self) -> Result<Value, Failure> {
        Ok(self.overview.lock().unwrap().clone())
    }

    async fn set_lend(&self, name: &str, mode: LendMode) -> Result<Value, Failure> {
        match name {
            "windows" => {
                let mut guard = self.overview.lock().unwrap();
                let worker = guard["workers"]
                    .as_array_mut()
                    .and_then(|w| w.iter_mut().find(|item| item["name"] == "windows"))
                    .expect("windows worker in fixture");
                worker["compute"]["mode"] = serde_json::to_value(&mode).unwrap();
                Ok(worker.clone())
            }
            "laptop" => Err((StatusCode::CONFLICT, Json(json!({"error": "no_compute"})))),
            "offline" => Err((
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "worker offline is unreachable"})),
            )),
            _ => Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("unknown worker {name}")})),
            )),
        }
    }
}

async fn serve(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn mesh_compute_http_contract_and_fixtures() {
    let pool = Arc::new(StubPool::new(fixture("heavy_running.json")));
    let app = axum::Router::new().nest_service(
        "/api/mind/mesh",
        mesh_http::router_with_backend(pool.clone(), Some(ApprovalAuthority::new(TOKEN).unwrap())),
    );
    let base = serve(app).await;
    let client = reqwest::Client::new();

    // Owner token required on both overview and lend.
    let res = client
        .get(format!("{base}/api/mind/mesh"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    let res = client
        .post(format!("{base}/api/mind/mesh/workers/windows/lend"))
        .json(&json!({"mode": {"mode": "pause"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);

    // Overview shape matches heavy_running.json fixture.
    let res = client
        .get(format!("{base}/api/mind/mesh"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["heavy"]["model"], "gpt-oss-120b-MXFP4");
    assert_eq!(body["heavy"]["state"]["state"], "running");
    assert_eq!(body["heavy"]["worker"], "windows");
    assert_eq!(body["heavy"]["tokens_per_second"], 7.4);
    assert_eq!(body["workers"][0]["compute"]["state"]["state"], "lending");
    assert_eq!(
        body["workers"][0]["compute"]["devices"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Also verify heavy_stopped, lending, and no_compute fixture shapes through GET /.
    for name in ["heavy_stopped.json", "lending.json", "no_compute.json"] {
        let expected = fixture(name);
        *pool.overview.lock().unwrap() = expected.clone();
        let got: Value = client
            .get(format!("{base}/api/mind/mesh"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(got, expected, "overview mismatch for {name}");
    }

    // Restore lending fixture and test all four POST /workers/:name/lend status codes.
    *pool.overview.lock().unwrap() = fixture("lending.json");

    // 200 OK: play until 08:00.
    let res = client
        .post(format!("{base}/api/mind/mesh/workers/windows/lend"))
        .bearer_auth(TOKEN)
        .json(&json!({"mode": {"mode": "play", "until": "2026-11-22T08:00:00Z"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let updated: Value = res.json().await.unwrap();
    assert_eq!(updated["name"], "windows");
    assert_eq!(updated["compute"]["mode"]["mode"], "play");
    assert_eq!(updated["compute"]["mode"]["until"], "2026-11-22T08:00:00Z");

    // 404 Not Found: unknown worker.
    let res = client
        .post(format!("{base}/api/mind/mesh/workers/missing/lend"))
        .bearer_auth(TOKEN)
        .json(&json!({"mode": {"mode": "auto"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);

    // 409 Conflict: worker without --compute must return {"error":"no_compute"}.
    let res = client
        .post(format!("{base}/api/mind/mesh/workers/laptop/lend"))
        .bearer_auth(TOKEN)
        .json(&json!({"mode": {"mode": "pause"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 409);
    let err: Value = res.json().await.unwrap();
    assert_eq!(err, json!({"error": "no_compute"}));

    // 502 Bad Gateway: unreachable worker.
    let res = client
        .post(format!("{base}/api/mind/mesh/workers/offline/lend"))
        .bearer_auth(TOKEN)
        .json(&json!({"mode": {"mode": "auto"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 502);
}
