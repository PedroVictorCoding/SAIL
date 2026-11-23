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

struct LoopbackWorker {
    identity: sail_mesh::identity::WorkerIdentity,
    address: String,
    _dir: tempfile::TempDir,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for LoopbackWorker {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("free port")
        .local_addr()
        .expect("free port address")
        .port()
}

async fn start_loopback_worker(
    name: &str,
    compute: Option<sail_mesh::compute::ComputeHandle>,
) -> LoopbackWorker {
    let dir = tempfile::tempdir().expect("worker dir");
    let identity = sail_mesh::identity::WorkerIdentity::create(dir.path(), name).expect("identity");
    let port = free_port();
    let address = format!("127.0.0.1:{port}");
    let config = sail_mesh::worker::WorkerConfig {
        bind: ([127, 0, 0, 1], port).into(),
        upstream: sail_mesh::worker::parse_upstream("http://127.0.0.1:9").expect("upstream"),
        max_parallel: 1,
        gate: sail_mesh::rest::RestGate::default(),
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = sail_mesh::identity::WorkerIdentity::load(dir.path()).expect("load identity");
    tokio::spawn(async move {
        let _ = sail_mesh::worker::serve_with_compute(&served, &config, compute, async {
            let _ = stopped.await;
        })
        .await;
    });
    for _ in 0..300 {
        if tokio::net::TcpStream::connect(&address).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    LoopbackWorker {
        identity,
        address,
        _dir: dir,
        stop: Some(stop),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_compute_http_real_pool_and_loopback_worker() {
    let compute_dir = tempfile::tempdir().expect("compute dir");
    let gate = sail_mesh::compute::tunnel::LendGate::new();
    let handle = sail_mesh::compute::ComputeHandle::new(
        gate,
        free_port(),
        compute_dir.path(),
        sail_mind::protocol::mesh::LendWindow {
            start: "01:00".into(),
            end: "08:00".into(),
        },
        vec![sail_mind::protocol::mesh::ComputeDevice {
            name: "CPU".into(),
            description: "test CPU".into(),
            lend_bytes: 1024,
        }],
    );
    let desk = start_loopback_worker("desk", Some(handle.clone())).await;
    let laptop = start_loopback_worker("laptop", None).await;
    let offline = start_loopback_worker("offline", None).await;
    let offline_code = offline.identity.pairing_code(&offline.address);
    drop(offline);

    let reg_dir = tempfile::tempdir().expect("registry dir");
    let registry_path = sail_mesh::Registry::path_in(reg_dir.path());
    let mut reg = sail_mesh::Registry::default();
    for code in [
        desk.identity.pairing_code(&desk.address),
        laptop.identity.pairing_code(&laptop.address),
        offline_code,
    ] {
        reg.upsert(sail_mesh::WorkerRecord::from_code(
            &code,
            Vec::new(),
            "2026-11-23T00:00:00Z".into(),
        ));
    }
    reg.save(&registry_path).expect("save registry");

    let pool = sail_mind::mesh::MeshPool::new(
        sail_mind::mesh::MeshSettings {
            mode: sail_mind::protocol::mesh::MeshMode::Offload,
            priorities: vec![sail_mind::protocol::resources::WorkPriority::Background],
            registry_path,
        },
        sail_mind::LocalModelConfig::default(),
    );
    pool.refresh().await;

    let app = axum::Router::new().nest_service(
        "/api/mind/mesh",
        mesh_http::router_with_pool(pool, Some(ApprovalAuthority::new(TOKEN).unwrap())),
    );
    let base = serve(app).await;
    let client = reqwest::Client::new();

    // GET / returns overview with heavy: null and desk compute populated.
    let overview: Value = client
        .get(format!("{base}/api/mind/mesh"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(overview["heavy"], Value::Null);
    let desk_entry = overview["workers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "desk")
        .unwrap();
    assert_eq!(desk_entry["compute"]["mode"]["mode"], "auto");

    // 200 OK: real MeshPool::set_lend round-trips to the loopback worker.
    let res = client
        .post(format!("{base}/api/mind/mesh/workers/desk/lend"))
        .bearer_auth(TOKEN)
        .json(&json!({"mode": {"mode": "pause"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let updated: Value = res.json().await.unwrap();
    assert_eq!(updated["compute"]["mode"]["mode"], "pause");
    assert_eq!(handle.snapshot().mode, LendMode::Pause);

    // 404 Not Found: unknown worker.
    let res = client
        .post(format!("{base}/api/mind/mesh/workers/missing/lend"))
        .bearer_auth(TOKEN)
        .json(&json!({"mode": {"mode": "auto"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);

    // 409 Conflict: worker without --compute returns {"error":"no_compute"}.
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
