//! SAIL Mesh end to end: a real `sail-mesh` worker (pinned TLS, bearer secret, route
//! allowlist) in front of a fake llama-server, and Mind routing between it and the host's
//! own (fake) model. Nothing leaves 127.0.0.1.
use axum::{
    Json, Router,
    extract::State,
    response::IntoResponse,
    routing::{get, post},
};
use sail_mesh::{
    Registry, WorkerRecord,
    identity::WorkerIdentity,
    worker::{WorkerConfig, parse_upstream},
};
use sail_mind::{
    CancellationToken, LocalModelConfig, MindService,
    mesh::{MeshOpError, MeshPool, MeshSettings, pair_at},
    protocol::{mesh::*, resources::WorkPriority, *},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, mpsc};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Normal,
    /// 503 on generation: the worker's model is down.
    Failure,
    /// One delta, then the connection closes without a finish reason.
    CutAfterFirst,
    /// One delta, then nothing until the client goes away.
    Slow,
}

struct Fake {
    tag: &'static str,
    model_path: Option<&'static str>,
    mode: Mutex<Mode>,
    generations: AtomicUsize,
    /// Set when a slow stream's body was dropped (the client disconnected).
    dropped: Arc<AtomicBool>,
}

struct FakeServer {
    base: String,
    state: Arc<Fake>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fake_llama(tag: &'static str, model_path: Option<&'static str>) -> FakeServer {
    let state = Arc::new(Fake {
        tag,
        model_path,
        mode: Mutex::new(Mode::Normal),
        generations: AtomicUsize::new(0),
        dropped: Arc::new(AtomicBool::new(false)),
    });
    let app = Router::new()
        .route("/health", get(|| async { Json(json!({"status": "ok"})) }))
        .route(
            "/props",
            get(|State(s): State<Arc<Fake>>| async move {
                let mut props = json!({
                    "modalities": {"vision": false},
                    "chat_template": "{% if tools %}tools{% endif %}",
                    "default_generation_settings": {"n_ctx": 8192}
                });
                if let Some(path) = s.model_path {
                    props["model_path"] = json!(path);
                }
                Json(props)
            }),
        )
        .route(
            "/apply-template",
            post(|Json(_): Json<Value>| async { Json(json!({"prompt": "rendered"})) }),
        )
        .route(
            "/tokenize",
            post(|Json(_): Json<Value>| async { Json(json!({"tokens": [1, 2, 3]})) }),
        )
        .route("/v1/chat/completions", post(generate))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    FakeServer { base, state, task }
}

struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn generate(State(s): State<Arc<Fake>>, Json(_): Json<Value>) -> axum::response::Response {
    s.generations.fetch_add(1, Ordering::SeqCst);
    let mode = *s.mode.lock().await;
    if mode == Mode::Failure {
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "down").into_response();
    }
    let first = format!(
        "data: {}\n\n",
        json!({"choices": [{"delta": {"content": s.tag}}]})
    );
    let sse = |body: axum::body::Body| {
        axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(body)
            .unwrap()
    };
    use futures_util::StreamExt;
    match mode {
        Mode::CutAfterFirst => sse(axum::body::Body::from(first)),
        Mode::Slow => {
            let flag = DropFlag(s.dropped.clone());
            let stream = futures_util::stream::once(async move {
                Ok::<_, std::convert::Infallible>(bytes::Bytes::from(first))
            })
            .chain(futures_util::stream::pending())
            .map(move |chunk| {
                let _held = &flag;
                chunk
            });
            sse(axum::body::Body::from_stream(stream))
        }
        _ => sse(axum::body::Body::from(format!(
            "{first}data: {}\n\ndata: [DONE]\n\n",
            json!({"choices": [{"delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 3, "completion_tokens": 1}})
        ))),
    }
}

struct Worker {
    identity: WorkerIdentity,
    address: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    _dir: tempfile::TempDir,
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

async fn start_worker(upstream: &str, max_parallel: usize) -> Worker {
    let dir = tempfile::tempdir().unwrap();
    let identity = WorkerIdentity::create(dir.path(), "desk").unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = WorkerConfig {
        bind: ([127, 0, 0, 1], port).into(),
        upstream: parse_upstream(upstream).unwrap(),
        max_parallel,
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = WorkerIdentity::load(dir.path()).unwrap();
    tokio::spawn(async move {
        let _ = sail_mesh::worker::serve(&served, &config, async {
            let _ = stopped.await;
        })
        .await;
    });
    let address = format!("127.0.0.1:{port}");
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(&address).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Worker {
        identity,
        address,
        stop: Some(stop),
        _dir: dir,
    }
}

struct RoomyHost;
impl sail_common::resources::HostProbe for RoomyHost {
    fn sample(&self) -> std::io::Result<resources::HostSample> {
        Ok(resources::HostSample {
            total_ram_bytes: 32 * sail_common::resources::GIB,
            available_ram_bytes: 24 * sail_common::resources::GIB,
            swap_used_bytes: 0,
            swap_total_bytes: 0,
            logical_cpus: 12,
            memory_pressure_avg10: None,
        })
    }
}

struct Setup {
    service: MindService,
    pool: Arc<MeshPool>,
    host: FakeServer,
    remote: FakeServer,
    worker: Worker,
    registry: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

fn host_config(host: &FakeServer) -> LocalModelConfig {
    LocalModelConfig {
        endpoint: format!("{}/v1/chat/completions", host.base),
        managed: false,
        threads: 1,
        threads_batch: Some(1),
        ..Default::default()
    }
}

async fn setup(mode: MeshMode, record: impl FnOnce(&Worker) -> WorkerRecord) -> Setup {
    let host = fake_llama("host", None).await;
    let remote = fake_llama("worker", Some(r"C:\Users\me\models\remote-qwen.gguf")).await;
    let worker = start_worker(&remote.base, 1).await;
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::path_in(dir.path());
    let mut reg = Registry::default();
    reg.upsert(record(&worker));
    reg.save(&registry).unwrap();
    let scheduler = Arc::new(sail_common::resources::ResourceScheduler::new(
        Arc::new(RoomyHost),
        Default::default(),
    ));
    let config = host_config(&host);
    let backend =
        sail_mind::local::LocalGemmaBackend::with_scheduler(config.clone(), 1, scheduler.clone())
            .unwrap();
    let service = MindService::with_scheduler(
        vec![Arc::new(backend)],
        ResourceLimits {
            queue_timeout_ms: 2_000,
            inference_timeout_ms: 10_000,
            ..Default::default()
        },
        scheduler,
    )
    .unwrap();
    let pool = MeshPool::new(
        MeshSettings {
            mode,
            priorities: vec![WorkPriority::Background],
            registry_path: registry.clone(),
        },
        config,
    );
    service.attach_mesh(pool.clone());
    pool.refresh().await;
    Setup {
        service,
        pool,
        host,
        remote,
        worker,
        registry,
        _dir: dir,
    }
}

fn paired(worker: &Worker) -> WorkerRecord {
    let code = worker.identity.pairing_code(&worker.address);
    WorkerRecord::from_code(&code, vec![], "2026-11-14T00:00:00Z".into())
}

fn text(response: &InferenceResponse) -> &str {
    response.message.content.text().unwrap_or_default()
}

async fn infer(
    service: &MindService,
    priority: WorkPriority,
    events: Option<mpsc::Sender<InferenceEvent>>,
) -> Result<InferenceResponse> {
    let mut request = InferenceRequest::text("hello");
    request.generation = Generation::direct(32, 0.2);
    service
        .infer_with_priority(request, CancellationToken::new(), events, priority)
        .await
}

fn worker_status(pool: &MeshPool) -> MeshWorkerStatus {
    pool.status().workers.into_iter().next().unwrap()
}

#[tokio::test]
async fn background_work_runs_on_the_worker_and_voice_stays_on_the_host() {
    let s = setup(MeshMode::Offload, paired).await;
    let status = worker_status(&s.pool);
    assert_eq!(
        status.state,
        MeshWorkerState::Ready,
        "{:?}",
        status.last_error
    );
    let model = status.model.unwrap();
    assert_eq!(
        model.file_name, "remote-qwen.gguf",
        "worker path must be redacted"
    );
    assert!(model.tools);

    let (tx, mut rx) = mpsc::channel(16);
    let response = infer(&s.service, WorkPriority::Background, Some(tx))
        .await
        .unwrap();
    assert_eq!(text(&response), "worker");
    assert_eq!(response.model, "remote-qwen@desk");
    let mut started = 0;
    while let Ok(event) = rx.try_recv() {
        if matches!(event.payload, InferenceEventPayload::Started { .. }) {
            started += 1;
        }
    }
    assert_eq!(started, 1);

    let voice = infer(&s.service, WorkPriority::Voice, None).await.unwrap();
    assert_eq!(text(&voice), "host");
    let interactive = infer(&s.service, WorkPriority::Interactive, None)
        .await
        .unwrap();
    assert_eq!(
        text(&interactive),
        "host",
        "interactive is local unless opted in"
    );

    let mind = s.service.status().await;
    let mesh = mind.mesh.unwrap();
    assert_eq!((mesh.offloaded, mesh.fell_back), (1, 0));
    assert_eq!(mesh.workers[0].completed, 1);
    assert_eq!(mesh.workers[0].active, 0);
    let json = serde_json::to_string(&mesh).unwrap();
    assert!(
        !json.contains(&s.worker.identity.secret),
        "status must never carry the secret"
    );
}

#[tokio::test]
async fn a_worker_failure_before_output_falls_back_to_the_host() {
    let s = setup(MeshMode::Offload, paired).await;
    *s.remote.state.mode.lock().await = Mode::Failure;
    let (tx, mut rx) = mpsc::channel(16);
    let response = infer(&s.service, WorkPriority::Background, Some(tx))
        .await
        .unwrap();
    assert_eq!(text(&response), "host");
    assert_eq!(s.remote.state.generations.load(Ordering::SeqCst), 1);
    let mut started = 0;
    let mut failed = 0;
    while let Ok(event) = rx.try_recv() {
        match event.payload {
            InferenceEventPayload::Started { .. } => started += 1,
            InferenceEventPayload::Failed { .. } => failed += 1,
            _ => {}
        }
    }
    assert_eq!(
        (started, failed),
        (1, 0),
        "fallback must look like one clean request"
    );
    let mesh = s.pool.status();
    assert_eq!((mesh.offloaded, mesh.fell_back), (0, 1));
    assert_eq!(mesh.workers[0].failed, 1);

    // The failed worker is skipped for a while: the next request goes straight to the host.
    *s.remote.state.mode.lock().await = Mode::Normal;
    let next = infer(&s.service, WorkPriority::Background, None)
        .await
        .unwrap();
    assert_eq!(text(&next), "host");
    assert_eq!(s.remote.state.generations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_worker_failure_after_output_is_reported_not_spliced() {
    let s = setup(MeshMode::Offload, paired).await;
    *s.remote.state.mode.lock().await = Mode::CutAfterFirst;
    let (tx, mut rx) = mpsc::channel(16);
    let error = infer(&s.service, WorkPriority::Background, Some(tx))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BackendProtocol);
    assert_eq!(s.host.state.generations.load(Ordering::SeqCst), 0);
    let mut deltas = vec![];
    let mut failed = false;
    while let Ok(event) = rx.try_recv() {
        match event.payload {
            InferenceEventPayload::TextDelta { text } => deltas.push(text),
            InferenceEventPayload::Failed { .. } => failed = true,
            _ => {}
        }
    }
    assert_eq!(deltas, ["worker"]);
    assert!(failed);
}

#[tokio::test]
async fn without_events_a_cut_stream_still_falls_back() {
    let s = setup(MeshMode::Offload, paired).await;
    *s.remote.state.mode.lock().await = Mode::CutAfterFirst;
    let response = infer(&s.service, WorkPriority::Background, None)
        .await
        .unwrap();
    assert_eq!(
        text(&response),
        "host",
        "nothing reached the caller, so retrying is safe"
    );
}

#[tokio::test]
async fn cancelling_stops_generation_on_the_worker() {
    let s = setup(MeshMode::Offload, paired).await;
    *s.remote.state.mode.lock().await = Mode::Slow;
    let cancel = CancellationToken::new();
    let (tx, mut rx) = mpsc::channel(16);
    let mut request = InferenceRequest::text("hello");
    request.generation = Generation::direct(32, 0.2);
    let service = s.service.clone();
    let token = cancel.clone();
    let task = tokio::spawn(async move {
        service
            .infer_with_priority(request, token, Some(tx), WorkPriority::Background)
            .await
    });
    loop {
        let event = rx.recv().await.unwrap();
        if matches!(event.payload, InferenceEventPayload::TextDelta { .. }) {
            break;
        }
    }
    cancel.cancel();
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled);
    for _ in 0..100 {
        if s.remote.state.dropped.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        s.remote.state.dropped.load(Ordering::SeqCst),
        "the worker must drop the upstream stream when the host goes away"
    );
    assert_eq!(s.host.state.generations.load(Ordering::SeqCst), 0);
    assert_eq!(worker_status(&s.pool).active, 0);
}

#[tokio::test]
async fn a_wrong_secret_or_certificate_is_unreachable_and_work_stays_local() {
    let wrong_secret = setup(MeshMode::Offload, |w| WorkerRecord {
        secret: sail_mesh::identity::new_secret(),
        ..paired(w)
    })
    .await;
    let status = worker_status(&wrong_secret.pool);
    assert_eq!(status.state, MeshWorkerState::Unreachable);
    assert!(status.last_error.unwrap().contains("secret"));
    let response = infer(&wrong_secret.service, WorkPriority::Background, None)
        .await
        .unwrap();
    assert_eq!(text(&response), "host");

    let wrong_cert = setup(MeshMode::Offload, |w| WorkerRecord {
        fingerprint: "0".repeat(64),
        ..paired(w)
    })
    .await;
    let status = worker_status(&wrong_cert.pool);
    assert_eq!(status.state, MeshWorkerState::Unreachable);
    assert!(status.last_error.unwrap().contains("certificate"));
    assert_eq!(
        wrong_cert.remote.state.generations.load(Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn overflow_mode_waits_for_the_host_to_be_busy_and_disabled_workers_are_skipped() {
    let s = setup(MeshMode::Overflow, paired).await;
    let response = infer(&s.service, WorkPriority::Background, None)
        .await
        .unwrap();
    assert_eq!(text(&response), "host");

    let s = setup(MeshMode::Offload, |w| WorkerRecord {
        enabled: false,
        ..paired(w)
    })
    .await;
    assert_eq!(worker_status(&s.pool).state, MeshWorkerState::Disabled);
    let response = infer(&s.service, WorkPriority::Background, None)
        .await
        .unwrap();
    assert_eq!(text(&response), "host");
}

#[tokio::test]
async fn images_never_go_to_a_worker_without_vision() {
    let s = setup(MeshMode::Offload, paired).await;
    let mut request = InferenceRequest::text("x");
    request.messages = vec![Message {
        role: Role::User,
        content: Content::Parts(vec![
            ContentPart::Text {
                text: "what is this".into(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/png;base64,iVBORw0KGgo=".into(),
                },
            },
        ]),
        tool_calls: vec![],
        tool_call_id: None,
        name: None,
    }];
    let _ = s
        .service
        .infer_with_priority(
            request,
            CancellationToken::new(),
            None,
            WorkPriority::Background,
        )
        .await;
    assert_eq!(s.remote.state.generations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pairing_proves_the_code_against_the_running_worker() {
    let s = setup(MeshMode::Offload, paired).await;
    let path = s.registry.with_file_name("fresh.json");
    let code = s.worker.identity.pairing_code(&s.worker.address).render();
    let (record, hello, replaced) = pair_at(&path, &code, vec!["qwen".into()]).await.unwrap();
    assert!(!replaced);
    assert_eq!(record.serves, ["qwen"]);
    assert_eq!(hello.name, "desk");
    assert_eq!(Registry::load(&path).unwrap().workers, [record]);

    let forged = code.replace(&s.worker.identity.fingerprint(), &"1".repeat(64));
    assert!(matches!(
        pair_at(&path, &forged, vec![]).await,
        Err(MeshOpError::Unverified(_))
    ));
    assert!(matches!(
        pair_at(&path, "not a code", vec![]).await,
        Err(MeshOpError::BadInput(_))
    ));
    assert_eq!(Registry::load(&path).unwrap().workers.len(), 1);
}
