use axum::{
    Json, Router,
    extract::State,
    response::IntoResponse,
    routing::{get, post},
};
use sail_mind::{
    CancellationToken, LocalModelConfig, MindService, backend::InferenceBackend, protocol::*,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{Mutex, Notify, mpsc};
#[derive(Clone, Copy)]
enum Mode {
    Normal,
    Truncated,
    Slow,
    Failure,
    Tool,
}
struct Fixture {
    mode: Mutex<Mode>,
    vision: bool,
    context: usize,
    requests: AtomicUsize,
}
struct TestServer {
    url: String,
    state: Arc<Fixture>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn fixture(vision: bool, context: usize) -> TestServer {
    let state = Arc::new(Fixture {
        mode: Mutex::new(Mode::Normal),
        vision,
        context,
        requests: AtomicUsize::new(0),
    });
    let app=Router::new().route("/health",get(||async{Json(json!({"status":"ok"}))}))
        .route("/props",get(|State(s):State<Arc<Fixture>>|async move{Json(json!({"modalities":{"vision":s.vision,"tools":true},"default_generation_settings":{"n_ctx":s.context}}))}))
        .route("/apply-template",post(|Json(_):Json<Value>|async{Json(json!({"prompt":"A rendered local chat template"}))}))
        .route("/tokenize",post(|Json(_):Json<Value>|async{Json(json!({"tokens":[1,2,3,4,5,6,7,8]}))}))
        .route("/v1/chat/completions",post(generate)).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestServer { url, state, task }
}
async fn generate(
    State(state): State<Arc<Fixture>>,
    Json(request): Json<Value>,
) -> axum::response::Response {
    state.requests.fetch_add(1, Ordering::SeqCst);
    assert_eq!(request["stream"], true);
    assert_eq!(request["chat_template_kwargs"]["enable_thinking"], false);
    let mode = *state.mode.lock().await;
    if matches!(mode, Mode::Failure) {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "model unavailable",
        )
            .into_response();
    }
    let delta = if matches!(mode, Mode::Tool) {
        json!({"tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"os_status","arguments":"{}"}}]})
    } else {
        json!({"content":"Olá mundo"})
    };
    let first = format!("data: {}\r\n\r\n", json!({"choices":[{"delta":delta}]}));
    if matches!(mode, Mode::Slow) {
        use futures_util::StreamExt;
        let stream = futures_util::stream::once(async move {
            Ok::<_, std::convert::Infallible>(bytes::Bytes::from(first))
        })
        .chain(futures_util::stream::pending());
        return axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from_stream(stream))
            .unwrap();
    }
    let finish = if matches!(mode, Mode::Truncated) {
        "data: [DONE]\r\n\r\n".into()
    } else {
        format!(
            "data: {}\r\n\r\ndata: [DONE]\r\n\r\n",
            json!({"choices":[{"delta":{},"finish_reason":if matches!(mode,Mode::Tool){"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":8,"completion_tokens":3}})
        )
    };
    // Byte-level fragments deliberately split UTF-8 and SSE delimiters.
    let parts = (first + &finish)
        .into_bytes()
        .chunks(3)
        .map(|s| Ok::<_, std::convert::Infallible>(bytes::Bytes::copy_from_slice(s)))
        .collect::<Vec<_>>();
    axum::response::Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from_stream(futures_util::stream::iter(
            parts,
        )))
        .unwrap()
}
fn service(server: &TestServer, limits: ResourceLimits) -> MindService {
    MindService::new(
        LocalModelConfig {
            endpoint: server.url.clone(),
            managed: false,
            ..Default::default()
        },
        limits,
    )
    .unwrap()
}
#[tokio::test]
async fn typed_local_text_and_vision_routing_preserve_version_ids_usage_and_unicode() {
    let server = fixture(true, 8192).await;
    let mind = service(&server, Default::default());
    let mut request = InferenceRequest::text("Olá");
    request.model = Some("gemma-4".into());
    let (tx, mut rx) = mpsc::channel(32);
    let response = mind
        .infer(request.clone(), CancellationToken::new(), Some(tx))
        .await
        .unwrap();
    assert_eq!(response.request_id, request.request_id);
    assert_eq!(response.message.content.text(), Some("Olá mundo"));
    assert_eq!(response.usage.output_tokens, Some(3));
    assert_eq!(response.usage.input_tokens, Some(8));
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        assert_eq!(event.version, 1);
        assert_eq!(event.request_id, request.request_id);
        events.push(event.payload);
    }
    assert!(matches!(
        events.first(),
        Some(InferenceEventPayload::Started { .. })
    ));
    assert!(matches!(
        events.last(),
        Some(InferenceEventPayload::Completed { .. })
    ));
    let mut vision = InferenceRequest::text("Describe");
    vision.messages[0].content = Content::Parts(vec![
        ContentPart::Text {
            text: "What is this?".into(),
        },
        ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jf3sAAAAASUVORK5CYII=".into(),
            },
        },
    ]);
    mind.infer(vision, CancellationToken::new(), None)
        .await
        .unwrap();
    assert!(
        mind.status().await.models[0]
            .capabilities
            .as_ref()
            .unwrap()
            .vision
    );
    assert_eq!(mind.status().await.metrics.completed, 2);
}
#[tokio::test]
async fn unsupported_vision_remote_images_and_unknown_models_do_not_reach_generation() {
    let server = fixture(false, 8192).await;
    let mind = service(&server, Default::default());
    let mut request = InferenceRequest::text("Image");
    request.messages[0].content = Content::Parts(vec![ContentPart::ImageUrl {
        image_url: ImageUrl {
            url: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jf3sAAAAASUVORK5CYII=".into(),
        },
    }]);
    assert_eq!(
        mind.infer(request, CancellationToken::new(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedCapability
    );
    let mut request = InferenceRequest::text("Image");
    request.messages[0].content = Content::Parts(vec![ContentPart::ImageUrl {
        image_url: ImageUrl {
            url: "https://example.com/private.png".into(),
        },
    }]);
    assert_eq!(
        mind.infer(request, CancellationToken::new(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenEndpoint
    );
    let mut request = InferenceRequest::text("Hello");
    request.model = Some("download-another-model".into());
    assert_eq!(
        mind.infer(request, CancellationToken::new(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnknownModel
    );
    assert_eq!(server.state.requests.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn version_context_output_and_input_budgets_are_enforced_before_inference() {
    let server = fixture(false, 32).await;
    let mind = service(&server, Default::default());
    assert_eq!(
        mind.infer(
            InferenceRequest::text("hello"),
            CancellationToken::new(),
            None
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::ResourceLimit
    );
    let mut request = InferenceRequest::text("hello");
    request.version = 99;
    assert_eq!(
        mind.infer(request, CancellationToken::new(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedVersion
    );
    let mut request = InferenceRequest::text("hello");
    request.generation.max_tokens = 99999;
    assert_eq!(
        mind.infer(request, CancellationToken::new(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    let mut request = InferenceRequest::text("x".repeat(150000));
    request.generation.max_tokens = 1;
    assert_eq!(
        mind.infer(request, CancellationToken::new(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit
    );
    assert_eq!(server.state.requests.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn cancellation_dropped_callers_and_recovery_release_the_budget() {
    let server = fixture(false, 8192).await;
    *server.state.mode.lock().await = Mode::Slow;
    let mind = service(&server, Default::default());
    let cancel = CancellationToken::new();
    let request = InferenceRequest::text("slow");
    let id = request.request_id;
    let (tx, mut rx) = mpsc::channel(32);
    let task = tokio::spawn({
        let mind = mind.clone();
        let cancel = cancel.clone();
        async move { mind.infer(request, cancel, Some(tx)).await }
    });
    while let Some(event) = rx.recv().await {
        if matches!(event.payload, InferenceEventPayload::TextDelta { .. }) {
            break;
        }
    }
    assert!(mind.cancel(id));
    assert_eq!(task.await.unwrap().unwrap_err().code, ErrorCode::Cancelled);
    assert_eq!(mind.status().await.metrics.active, 0);
    let (tx, mut rx) = mpsc::channel(32);
    let task = tokio::spawn({
        let mind = mind.clone();
        async move {
            mind.infer(
                InferenceRequest::text("drop"),
                CancellationToken::new(),
                Some(tx),
            )
            .await
        }
    });
    while let Some(event) = rx.recv().await {
        if matches!(event.payload, InferenceEventPayload::TextDelta { .. }) {
            break;
        }
    }
    task.abort();
    let _ = task.await;
    assert_eq!(mind.status().await.metrics.active, 0);
    *server.state.mode.lock().await = Mode::Normal;
    mind.infer(
        InferenceRequest::text("recover"),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(mind.status().await.metrics.cancelled, 2);
}
#[tokio::test]
async fn stream_disconnect_http_failure_and_response_caps_are_not_fake_success() {
    let server = fixture(false, 8192).await;
    let mind = service(&server, Default::default());
    *server.state.mode.lock().await = Mode::Truncated;
    assert_eq!(
        mind.infer(
            InferenceRequest::text("truncate"),
            CancellationToken::new(),
            None
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::BackendProtocol
    );
    *server.state.mode.lock().await = Mode::Failure;
    assert_eq!(
        mind.infer(
            InferenceRequest::text("fail"),
            CancellationToken::new(),
            None
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::BackendUnavailable
    );
    *server.state.mode.lock().await = Mode::Normal;
    let small = service(
        &server,
        ResourceLimits {
            max_response_bytes: 2,
            ..Default::default()
        },
    );
    assert_eq!(
        small
            .infer(
                InferenceRequest::text("small"),
                CancellationToken::new(),
                None
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::ResourceLimit
    );
    mind.infer(
        InferenceRequest::text("retry"),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn typed_tool_calls_require_an_advertised_function() {
    let server = fixture(false, 8192).await;
    *server.state.mode.lock().await = Mode::Tool;
    let mind = service(&server, Default::default());
    assert_eq!(
        mind.infer(
            InferenceRequest::text("no tools"),
            CancellationToken::new(),
            None
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::BackendProtocol
    );
    let mut request = InferenceRequest::text("status");
    request.tools = vec![ToolDefinition {
        kind: "function".into(),
        function: FunctionDefinition {
            name: "os_status".into(),
            description: "OS".into(),
            parameters: json!({"type":"object"}),
        },
    }];
    request.tool_choice = ToolChoice::Auto;
    let response = mind
        .infer(request, CancellationToken::new(), None)
        .await
        .unwrap();
    assert_eq!(response.message.tool_calls[0].function.name, "os_status");
}

struct BlockingBackend {
    entered: Notify,
    release: Notify,
}
#[async_trait::async_trait]
impl InferenceBackend for BlockingBackend {
    fn name(&self) -> &str {
        "blocking"
    }
    fn aliases(&self) -> Vec<String> {
        vec![]
    }
    async fn start(&self) -> Result<ModelStatus> {
        Ok(self.status().await)
    }
    async fn status(&self) -> ModelStatus {
        ModelStatus {
            version: VERSION,
            state: ModelState::Ready,
            model: "blocking".into(),
            capabilities: None,
            requested_backend: ComputeBackend::Cpu,
            observed_backend: None,
            owned_process: false,
            process_id: None,
            last_error: None,
            active_profile: None,
            hardware: None,
        }
    }
    async fn infer(
        &self,
        request: &InferenceRequest,
        _: Option<&mpsc::Sender<InferenceEvent>>,
        _: &CancellationToken,
        _: &ResourceLimits,
    ) -> Result<InferenceResponse> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(InferenceResponse {
            version: VERSION,
            request_id: request.request_id,
            model: "blocking".into(),
            message: Message::text(Role::Assistant, "done"),
            finish_reason: "stop".into(),
            usage: Default::default(),
            timings: Default::default(),
        })
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn queue_bound_queue_deadline_and_shutdown_cancel_all_requests() {
    let backend = Arc::new(BlockingBackend {
        entered: Notify::new(),
        release: Notify::new(),
    });
    let mind = MindService::with_backends(
        vec![backend.clone()],
        ResourceLimits {
            max_parallel: 1,
            max_queued: 1,
            queue_timeout_ms: 200,
            ..Default::default()
        },
    )
    .unwrap();
    let first = tokio::spawn({
        let mind = mind.clone();
        async move {
            mind.infer(
                InferenceRequest::text("first"),
                CancellationToken::new(),
                None,
            )
            .await
        }
    });
    backend.entered.notified().await;
    let queued = tokio::spawn({
        let mind = mind.clone();
        async move {
            mind.infer(
                InferenceRequest::text("queued"),
                CancellationToken::new(),
                None,
            )
            .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while mind.status().await.metrics.queued == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        mind.infer(
            InferenceRequest::text("full"),
            CancellationToken::new(),
            None
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Busy
    );
    assert_eq!(
        queued.await.unwrap().unwrap_err().code,
        ErrorCode::QueueTimeout
    );
    mind.shutdown().await.unwrap();
    assert_eq!(first.await.unwrap().unwrap_err().code, ErrorCode::Cancelled);
    assert_eq!(
        mind.infer(
            InferenceRequest::text("after shutdown"),
            CancellationToken::new(),
            None
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::ShuttingDown
    );
}
#[tokio::test]
async fn inference_deadline_and_disconnected_consumers_are_bounded() {
    let backend = Arc::new(BlockingBackend {
        entered: Notify::new(),
        release: Notify::new(),
    });
    let mind = MindService::with_backends(
        vec![backend],
        ResourceLimits {
            inference_timeout_ms: 30,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        mind.infer(
            InferenceRequest::text("deadline"),
            CancellationToken::new(),
            None
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::DeadlineExceeded
    );
    assert_eq!(mind.status().await.metrics.active, 0);
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    assert_eq!(
        mind.infer(
            InferenceRequest::text("closed"),
            CancellationToken::new(),
            Some(tx)
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::ConsumerClosed
    );
}

#[tokio::test]
async fn queued_cancellation_and_duplicate_ids_do_not_cancel_another_request() {
    let server = fixture(false, 8192).await;
    *server.state.mode.lock().await = Mode::Slow;
    let mind = service(&server, Default::default());
    let request = InferenceRequest::text("active");
    let duplicate = request.clone();
    let (tx, mut rx) = mpsc::channel(32);
    let active = tokio::spawn({
        let mind = mind.clone();
        async move {
            mind.infer(request, CancellationToken::new(), Some(tx))
                .await
        }
    });
    while let Some(event) = rx.recv().await {
        if matches!(event.payload, InferenceEventPayload::TextDelta { .. }) {
            break;
        }
    }
    assert_eq!(
        mind.infer(duplicate, CancellationToken::new(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    let queued = InferenceRequest::text("queued");
    let id = queued.request_id;
    let task = tokio::spawn({
        let mind = mind.clone();
        async move { mind.infer(queued, CancellationToken::new(), None).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while mind.status().await.metrics.queued == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(mind.cancel(id));
    assert_eq!(task.await.unwrap().unwrap_err().code, ErrorCode::Cancelled);
    assert_eq!(mind.status().await.metrics.active, 1);
    active.abort();
    let _ = active.await;
    assert_eq!(mind.status().await.metrics.cancelled, 2);
}
#[tokio::test]
async fn external_local_service_is_not_owned_or_stopped() {
    let server = fixture(false, 8192).await;
    let backend = sail_mind::local::LocalGemmaBackend::new(
        LocalModelConfig {
            endpoint: server.url.clone(),
            managed: true,
            ..Default::default()
        },
        1,
    )
    .unwrap();
    let status = backend.start().await.unwrap();
    assert!(!status.owned_process);
    assert!(status.observed_backend.is_none());
    backend.shutdown().await.unwrap();
    let response = reqwest::get(server.url.replace("/v1/chat/completions", "/health"))
        .await
        .unwrap();
    assert!(response.status().is_success());
}
struct NamedBackend {
    label: &'static str,
    calls: AtomicUsize,
}
#[async_trait::async_trait]
impl InferenceBackend for NamedBackend {
    fn name(&self) -> &str {
        self.label
    }
    fn aliases(&self) -> Vec<String> {
        vec![format!("{}-alias", self.label)]
    }
    async fn start(&self) -> Result<ModelStatus> {
        Ok(self.status().await)
    }
    async fn status(&self) -> ModelStatus {
        ModelStatus {
            version: VERSION,
            state: ModelState::Ready,
            model: self.label.into(),
            capabilities: None,
            requested_backend: ComputeBackend::Cpu,
            observed_backend: None,
            owned_process: false,
            process_id: None,
            last_error: None,
            active_profile: None,
            hardware: None,
        }
    }
    async fn infer(
        &self,
        r: &InferenceRequest,
        _: Option<&mpsc::Sender<InferenceEvent>>,
        _: &CancellationToken,
        _: &ResourceLimits,
    ) -> Result<InferenceResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(InferenceResponse {
            version: VERSION,
            request_id: r.request_id,
            model: self.label.into(),
            message: Message::text(Role::Assistant, self.label),
            finish_reason: "stop".into(),
            usage: Default::default(),
            timings: Default::default(),
        })
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn routing_is_explicit_and_ambiguous_model_registrations_are_rejected() {
    let first = Arc::new(NamedBackend {
        label: "first",
        calls: AtomicUsize::new(0),
    });
    let second = Arc::new(NamedBackend {
        label: "second",
        calls: AtomicUsize::new(0),
    });
    let mind = MindService::with_backends(vec![first.clone(), second.clone()], Default::default())
        .unwrap();
    let mut request = InferenceRequest::text("route");
    request.model = Some("second-alias".into());
    assert_eq!(
        mind.infer(request, CancellationToken::new(), None)
            .await
            .unwrap()
            .model,
        "second"
    );
    assert_eq!(first.calls.load(Ordering::SeqCst), 0);
    assert_eq!(second.calls.load(Ordering::SeqCst), 1);
    assert!(
        MindService::with_backends(
            vec![
                first.clone(),
                Arc::new(NamedBackend {
                    label: "first",
                    calls: AtomicUsize::new(0)
                })
            ],
            Default::default()
        )
        .is_err()
    );
}
