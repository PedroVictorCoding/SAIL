//! End-to-end tests for the worker: real TLS in front of a fake llama-server, driven by the
//! host's pinned client (and by a raw client where the exact request path matters).
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use sail_mesh::{
    FORWARDED_ROUTES, HELLO_ROUTE, MESH_VERSION, MeshHello,
    identity::WorkerIdentity,
    tls::pinned_client,
    worker::{MAX_BODY_BYTES, WorkerConfig, serve},
};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex, Once,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, mpsc, oneshot};

// ---------------------------------------------------------------- fake llama-server

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: HeaderMap,
}

#[derive(Clone)]
struct StreamPlan {
    first: Vec<u8>,
    second: Vec<u8>,
    hold: bool,
    content_type: String,
}

impl Default for StreamPlan {
    fn default() -> Self {
        Self {
            first: b"data: hello\n\n".to_vec(),
            second: Vec::new(),
            hold: false,
            content_type: "text/event-stream".into(),
        }
    }
}

#[derive(Clone)]
struct Fake {
    address: SocketAddr,
    log: Arc<Mutex<Vec<Seen>>>,
    props: Arc<Mutex<serde_json::Value>>,
    stream: Arc<Mutex<StreamPlan>>,
    release: Arc<Notify>,
    upstream_dropped: Arc<AtomicBool>,
}

impl Fake {
    fn seen(&self) -> Vec<Seen> {
        self.log
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn clear(&self) {
        self.log
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    fn set_props(&self, props: serde_json::Value) {
        *self
            .props
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = props;
    }

    fn set_stream(&self, plan: StreamPlan) {
        *self
            .stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = plan;
    }

    fn dropped_upstream(&self) -> bool {
        self.upstream_dropped.load(Ordering::SeqCst)
    }
}

fn record(state: &Fake, method: &Method, uri: &Uri, headers: &HeaderMap) {
    state
        .log
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(Seen {
            method: method.as_str().to_owned(),
            path: uri.path().to_owned(),
            headers: headers.clone(),
        });
}

async fn health(
    State(state): State<Fake>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    record(&state, &method, &uri, &headers);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::HeaderName::from_static("x-upstream"), "leak"),
            (header::CACHE_CONTROL, "max-age=3600"),
        ],
        "{\"status\":\"ok\"}",
    )
        .into_response()
}

async fn props(
    State(state): State<Fake>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    record(&state, &method, &uri, &headers);
    let body = serde_json::to_vec(
        &*state
            .props
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
    .expect("test props serialize");
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::HeaderName::from_static("x-upstream"), "leak"),
        ],
        body,
    )
        .into_response()
}

async fn echo(State(state): State<Fake>, method: Method, uri: Uri, headers: HeaderMap) -> Response {
    record(&state, &method, &uri, &headers);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        "{\"ok\":true}",
    )
        .into_response()
}

async fn chat(State(state): State<Fake>, method: Method, uri: Uri, headers: HeaderMap) -> Response {
    record(&state, &method, &uri, &headers);
    let plan = state
        .stream
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let (sender, mut receiver) = mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    let body = futures_util::stream::poll_fn(move |context| receiver.poll_recv(context));
    let release = state.release.clone();
    let dropped = state.upstream_dropped.clone();
    tokio::spawn(async move {
        if sender.send(Ok(Bytes::from(plan.first))).await.is_err() {
            dropped.store(true, Ordering::SeqCst);
            return;
        }
        if plan.hold {
            tokio::select! {
                () = release.notified() => {}
                () = sender.closed() => {
                    dropped.store(true, Ordering::SeqCst);
                    return;
                }
            }
        }
        if sender.send(Ok(Bytes::from(plan.second))).await.is_err() {
            dropped.store(true, Ordering::SeqCst);
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, plan.content_type)
        .header(header::HeaderName::from_static("x-upstream"), "leak")
        .header(header::CACHE_CONTROL, "max-age=3600")
        .body(Body::from_stream(body))
        .expect("valid test response")
}

async fn start_fake() -> Fake {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake upstream");
    let fake = Fake {
        address: listener.local_addr().expect("fake upstream address"),
        log: Arc::new(Mutex::new(Vec::new())),
        props: Arc::new(Mutex::new(serde_json::json!({
            "model_path": "C:\\Users\\me\\models\\qwen.gguf",
            "default_generation_settings": { "n_ctx": 32768 },
            "modalities": { "vision": true },
            "chat_template": "{% if tools %}{{ tools }}{% endif %}"
        }))),
        stream: Arc::new(Mutex::new(StreamPlan::default())),
        release: Arc::new(Notify::new()),
        upstream_dropped: Arc::new(AtomicBool::new(false)),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/props", get(props))
        .route("/apply-template", post(echo))
        .route("/tokenize", post(echo))
        .route("/v1/chat/completions", post(chat))
        .with_state(fake.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    fake
}

// ---------------------------------------------------------------- worker harness

struct Worker {
    address: SocketAddr,
    worker_id: String,
    fingerprint: String,
    secret: String,
    _dir: tempfile::TempDir,
    stop: Option<oneshot::Sender<()>>,
    _task: tokio::task::JoinHandle<()>,
}

impl Worker {
    fn base(&self) -> String {
        format!("https://{}", self.address)
    }

    fn client(&self) -> reqwest::Client {
        pinned_client(&self.fingerprint, &self.secret, Duration::from_secs(5))
            .expect("pinned client")
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind free port")
        .local_addr()
        .expect("free port address")
        .port()
}

async fn wait_listening(address: SocketAddr) {
    for _ in 0..300 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("worker did not start listening on {address}");
}

async fn start_worker_on(
    identity: WorkerIdentity,
    dir: tempfile::TempDir,
    address: SocketAddr,
    upstream: url::Url,
    max_parallel: usize,
) -> Worker {
    let worker_id = identity.worker_id.clone();
    let fingerprint = identity.fingerprint();
    let secret = identity.secret.clone();
    let config = WorkerConfig {
        bind: address,
        upstream,
        max_parallel,
    };
    let (stop, wait) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _ = serve(&identity, &config, async move {
            let _ = wait.await;
        })
        .await;
    });
    let worker = Worker {
        address,
        worker_id,
        fingerprint,
        secret,
        _dir: dir,
        stop: Some(stop),
        _task: task,
    };
    wait_listening(address).await;
    worker
}

async fn worker_in_front_of(fake: &Fake, max_parallel: usize) -> Worker {
    let dir = tempfile::tempdir().expect("worker identity dir");
    let identity = WorkerIdentity::create(dir.path(), "test-worker").expect("worker identity");
    let upstream = url::Url::parse(&format!("http://{}", fake.address)).expect("upstream URL");
    start_worker_on(
        identity,
        dir,
        SocketAddr::from(([127, 0, 0, 1], free_port())),
        upstream,
        max_parallel,
    )
    .await
}

// ---------------------------------------------------------------- raw TLS client

/// A tiny HTTP/1.1 client that trusts any certificate and sends the request target exactly
/// as written, so `//health`, `/../health` and `/%2e%2e/health` reach the worker unchanged.
mod raw {
    use rustls::{
        DigitallySignedStruct, SignatureScheme,
        client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        crypto::WebPkiSupportedAlgorithms,
        pki_types::{CertificateDer, ServerName, UnixTime},
    };
    use std::{
        io::{Read, Write},
        net::SocketAddr,
        sync::Arc,
        time::Duration,
    };

    #[derive(Debug)]
    struct TrustAny {
        algorithms: WebPkiSupportedAlgorithms,
    }

    impl ServerCertVerifier for TrustAny {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.algorithms.supported_schemes()
        }
    }

    pub(super) fn request(
        address: SocketAddr,
        method: &str,
        path: &str,
        authorization: Option<&str>,
    ) -> String {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .expect("TLS versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(TrustAny {
                algorithms: provider.signature_verification_algorithms,
            }))
            .with_no_client_auth();
        let name = ServerName::try_from("worker.local").expect("server name");
        let connection =
            rustls::ClientConnection::new(Arc::new(config), name).expect("TLS connection");
        let tcp = std::net::TcpStream::connect(address).expect("connect worker");
        tcp.set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        tcp.set_write_timeout(Some(Duration::from_secs(10)))
            .expect("write timeout");
        let mut stream = rustls::StreamOwned::new(connection, tcp);
        let auth = authorization
            .map(|value| format!("Authorization: {value}\r\n"))
            .unwrap_or_default();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: worker\r\n{auth}Content-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(head.as_bytes()).expect("send request");
        stream.flush().expect("flush request");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read response");
        String::from_utf8_lossy(&response).into_owned()
    }
}

async fn raw_request(
    address: SocketAddr,
    method: &str,
    path: &str,
    authorization: Option<String>,
) -> (u16, String) {
    let method = method.to_owned();
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let response = raw::request(address, &method, &path, authorization.as_deref());
        let status = response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .expect("HTTP status line");
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_owned())
            .unwrap_or_default();
        (status, body)
    })
    .await
    .expect("raw request task")
}

// ---------------------------------------------------------------- tracing capture

static CAPTURED: Mutex<Vec<u8>> = Mutex::new(Vec::new());

struct CaptureWriter;

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        CAPTURED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CaptureWriter
    }
}

fn capture_log() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(CaptureWriter)
            .with_ansi(false)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

fn captured_log() -> String {
    String::from_utf8_lossy(
        &CAPTURED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
    .into_owned()
}

// ---------------------------------------------------------------- tests

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auth_rejects_with_the_same_answer_and_locks_out_guessing() {
    let fake = start_fake().await;
    let worker = worker_in_front_of(&fake, 2).await;
    let short = "Bearer short";
    let lower = format!("bearer {}", worker.secret);
    let submissions: Vec<Option<String>> = vec![
        None,
        Some("Bearer wrong-secret".to_owned()),
        Some(short.to_owned()),
        Some(lower),
    ];
    let mut bodies = Vec::new();
    for authorization in submissions {
        let (status, body) =
            raw_request(worker.address, "GET", "/health", authorization.clone()).await;
        assert_eq!(status, 401, "{authorization:?}");
        bodies.push((status, body));
    }
    let first = &bodies[0];
    for body in &bodies[1..] {
        assert_eq!(body, first, "every refusal looks the same");
    }
    assert_eq!(first.1, "{\"error\":\"unauthorized\"}");
    assert!(
        fake.seen().is_empty(),
        "auth failures never reach llama-server"
    );

    // The same address is throttled once the window fills; the correct secret does not help
    // until the lockout expires.
    let mut statuses = vec![401; 4];
    for _ in 4..25 {
        let (status, _) = raw_request(
            worker.address,
            "GET",
            "/health",
            Some("Bearer wrong".to_owned()),
        )
        .await;
        statuses.push(status);
        if status == 429 {
            break;
        }
    }
    assert_eq!(
        statuses.iter().filter(|status| **status == 401).count(),
        20,
        "twenty failed attempts are counted: {statuses:?}"
    );
    assert_eq!(statuses.last(), Some(&429), "{statuses:?}");
    let client = worker.client();
    let locked_out = client
        .get(format!("{}/health", worker.base()))
        .send()
        .await
        .expect("correct secret while locked out");
    assert_eq!(locked_out.status(), 429);
    assert_eq!(
        locked_out.text().await.expect("lockout body"),
        "{\"error\":\"too many attempts\"}"
    );
    assert!(
        fake.seen().is_empty(),
        "nothing was forwarded during the lockout"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forwards_the_allowlist_and_refuses_everything_else() {
    let fake = start_fake().await;
    let worker = worker_in_front_of(&fake, 2).await;
    let client = worker.client();
    let base = worker.base();

    let health = client
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("health");
    assert_eq!(health.status(), 200);
    assert_eq!(
        health.headers().get("x-upstream"),
        None,
        "upstream headers are not copied"
    );
    assert_eq!(
        health
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let body = health.text().await.expect("health body");
    assert_eq!(body, "{\"status\":\"ok\"}");

    let before = fake.seen().len();
    let props = client
        .get(format!("{base}/props"))
        .send()
        .await
        .expect("props");
    assert_eq!(props.status(), 200);
    let redacted: serde_json::Value = props.json().await.expect("props JSON");
    assert_eq!(redacted["model_path"], "qwen.gguf");
    assert!(
        !redacted.to_string().contains("Users"),
        "no directory in props"
    );

    for path in ["/apply-template", "/tokenize", "/v1/chat/completions"] {
        let response = client
            .post(format!("{base}{path}"))
            .header("cookie", "session=1")
            .header("x-forward-me", "no")
            .json(&serde_json::json!({"messages": []}))
            .send()
            .await
            .expect("forwarded route");
        assert_eq!(response.status(), 200, "{path}");
    }
    let forwarded = fake.seen();
    assert!(
        forwarded.len() >= before + 4,
        "every allowlisted route reached llama-server: {forwarded:?}"
    );
    for seen in &forwarded {
        assert!(
            seen.headers.get(header::AUTHORIZATION).is_none(),
            "{seen:?}"
        );
        assert_eq!(
            seen.headers.get("x-forward-me"),
            None,
            "request headers are not forwarded"
        );
        if seen.method == "POST" {
            assert_eq!(
                seen.headers
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some("application/json"),
                "only the JSON content type is forwarded"
            );
        }
    }
    assert!(forwarded.iter().any(|seen| seen.path == "/tokenize"));

    fake.clear();
    for path in [
        "/v1/models",
        "/slots",
        "/metrics",
        "/../health",
        "/%2e%2e/health",
        "//health",
        "/health/",
    ] {
        let (status, _) = raw_request(
            worker.address,
            "GET",
            path,
            Some(format!("Bearer {}", worker.secret)),
        )
        .await;
        assert_eq!(status, 404, "{path} is not forwarded");
    }
    for (method, path) in [("POST", "/health"), ("GET", "/v1/chat/completions")] {
        let (status, _) = raw_request(
            worker.address,
            method,
            path,
            Some(format!("Bearer {}", worker.secret)),
        )
        .await;
        assert_eq!(status, 404, "{method} {path}");
    }
    for (method, path) in [
        ("GET", "/health?x=1"),
        ("POST", "/v1/chat/completions?stream=1"),
    ] {
        let (status, body) = raw_request(
            worker.address,
            method,
            path,
            Some(format!("Bearer {}", worker.secret)),
        )
        .await;
        assert_eq!(status, 400, "{path}");
        assert_eq!(body, "{\"error\":\"query strings are not forwarded\"}");
    }
    assert!(
        fake.seen().is_empty(),
        "refused routes never reach llama-server"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn busy_state_follows_the_stream_and_the_client_disconnect() {
    let fake = start_fake().await;
    let worker = worker_in_front_of(&fake, 1).await;
    let client = worker.client();
    let base = worker.base();
    fake.set_stream(StreamPlan {
        hold: true,
        ..StreamPlan::default()
    });

    let hello = || async {
        client
            .get(format!("{base}{HELLO_ROUTE}"))
            .send()
            .await
            .expect("hello")
            .json::<MeshHello>()
            .await
            .expect("hello JSON")
    };
    assert_eq!(hello().await.active, 0);

    let mut first = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({"messages": []}))
        .send()
        .await
        .expect("first generation");
    assert_eq!(first.status(), 200);
    let chunk = first.chunk().await.expect("first chunk").expect("chunk");
    assert_eq!(&chunk[..], b"data: hello\n\n");
    assert_eq!(hello().await.active, 1);

    let second = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({"messages": []}))
        .send()
        .await
        .expect("second generation");
    assert_eq!(second.status(), 503);
    assert_eq!(
        second.text().await.expect("busy body"),
        "{\"error\":\"worker is busy\"}"
    );

    fake.release.notify_one();
    let rest = first.bytes().await.expect("first generation rest");
    assert!(rest.is_empty(), "the held-open stream ends: {rest:?}");
    let mut active = 1;
    for _ in 0..100 {
        active = hello().await.active;
        if active == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(active, 0, "a finished stream frees the slot");

    fake.set_stream(StreamPlan::default());
    let third = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({"messages": []}))
        .send()
        .await
        .expect("third generation");
    assert_eq!(third.status(), 200);
    let drained = third.bytes().await.expect("third body");
    assert_eq!(&drained[..], b"data: hello\n\n");

    // A client that walks away frees the slot too, and llama-server sees its request die.
    fake.upstream_dropped.store(false, Ordering::SeqCst);
    fake.set_stream(StreamPlan {
        hold: true,
        ..StreamPlan::default()
    });
    let mut fourth = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({"messages": []}))
        .send()
        .await
        .expect("fourth generation");
    let chunk = fourth.chunk().await.expect("chunk").expect("chunk");
    assert_eq!(&chunk[..], b"data: hello\n\n");
    drop(fourth);
    let mut dropped = false;
    for _ in 0..100 {
        dropped = fake.dropped_upstream();
        if dropped {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(dropped, "llama-server saw the request cancelled");
    let mut active = 1;
    for _ in 0..100 {
        active = hello().await.active;
        if active == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(active, 0, "a cancelled stream frees the slot");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streams_bytes_unchanged_and_a_chunk_before_the_upstream_finishes() {
    let fake = start_fake().await;
    let worker = worker_in_front_of(&fake, 1).await;
    let client = worker.client();
    fake.set_stream(StreamPlan {
        first: b"data: \xE2\x82".to_vec(),
        second: b"\xAC\n\n".to_vec(),
        hold: true,
        content_type: "text/event-stream".into(),
    });

    let mut response = client
        .post(format!("{}/v1/chat/completions", worker.base()))
        .json(&serde_json::json!({"messages": []}))
        .send()
        .await
        .expect("generation");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&header::HeaderValue::from_static("text/event-stream"))
    );
    let first = response.chunk().await.expect("chunk").expect("first chunk");
    assert_eq!(
        &first[..],
        b"data: \xE2\x82",
        "the first chunk is not held back"
    );
    fake.release.notify_one();
    let rest = response.bytes().await.expect("rest of the stream");
    assert_eq!(
        &rest[..],
        b"\xAC\n\n",
        "a UTF-8 code point split across chunks survives"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn limits_bodies_and_hides_upstream_failures() {
    let fake = start_fake().await;
    let worker = worker_in_front_of(&fake, 1).await;
    let client = worker.client();
    fake.clear();

    let oversized = client
        .post(format!("{}/v1/chat/completions", worker.base()))
        .body(vec![b'x'; MAX_BODY_BYTES + 1])
        .send()
        .await
        .expect("oversized body");
    assert_eq!(oversized.status(), 413);
    assert!(
        fake.seen()
            .iter()
            .all(|seen| seen.path != "/v1/chat/completions"),
        "an oversized body is not forwarded"
    );

    fake.set_props(serde_json::json!({
        "model_path": "qwen.gguf",
        "filler": "x".repeat(4 * 1024 * 1024 + 1024)
    }));
    let too_large = client
        .get(format!("{}/props", worker.base()))
        .send()
        .await
        .expect("oversized metadata");
    assert_eq!(too_large.status(), 502);
    assert_eq!(
        too_large.text().await.expect("metadata error"),
        "{\"error\":\"model server reply too large\"}"
    );

    let dead =
        url::Url::parse(&format!("http://127.0.0.1:{}", free_port())).expect("dead upstream");
    let dir = tempfile::tempdir().expect("dead worker dir");
    let identity = WorkerIdentity::create(dir.path(), "dead-worker").expect("identity");
    let dead_worker = start_worker_on(
        identity,
        dir,
        SocketAddr::from(([127, 0, 0, 1], free_port())),
        dead,
        1,
    )
    .await;
    let unavailable = dead_worker
        .client()
        .get(format!("{}/health", dead_worker.base()))
        .send()
        .await
        .expect("unavailable upstream");
    assert_eq!(unavailable.status(), 502);
    let body = unavailable.text().await.expect("unavailable body");
    assert_eq!(body, "{\"error\":\"model server unavailable\"}");
    assert!(
        !body.contains("127.0.0.1"),
        "no upstream address leaks: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hello_describes_the_worker_and_its_model_without_paths() {
    let fake = start_fake().await;
    let worker = worker_in_front_of(&fake, 3).await;
    let client = worker.client();
    let hello = || async {
        client
            .get(format!("{}{HELLO_ROUTE}", worker.base()))
            .send()
            .await
            .expect("hello")
            .json::<MeshHello>()
            .await
            .expect("hello JSON")
    };
    let first = hello().await;
    assert_eq!(first.version, MESH_VERSION);
    assert_eq!(first.worker_id, worker.worker_id);
    assert_eq!(first.name, "test-worker");
    assert_eq!(first.os, std::env::consts::OS);
    assert_eq!(first.arch, std::env::consts::ARCH);
    assert_eq!(first.max_parallel, 3);
    let model = first.model.expect("model from props");
    assert_eq!(model.file_name, "qwen.gguf");
    assert_eq!(model.context_tokens, 32768);
    assert!(model.vision);
    assert!(model.tools);

    fake.set_props(serde_json::json!({
        "model_path": "/srv/sail/models/gemma.gguf",
        "default_generation_settings": { "n_ctx": 8192 },
        "modalities": {},
        "chat_template": "plain"
    }));
    let second = hello().await;
    let model = second.model.expect("model after props change");
    assert_eq!(model.file_name, "gemma.gguf");
    assert_eq!(model.context_tokens, 8192);
    assert!(!model.vision);
    assert!(!model.tools);

    let dead =
        url::Url::parse(&format!("http://127.0.0.1:{}", free_port())).expect("dead upstream");
    let dir = tempfile::tempdir().expect("hello dir");
    let identity = WorkerIdentity::create(dir.path(), "lonely-worker").expect("identity");
    let lonely = start_worker_on(
        identity,
        dir,
        SocketAddr::from(([127, 0, 0, 1], free_port())),
        dead,
        1,
    )
    .await;
    let hello = lonely
        .client()
        .get(format!("{}{HELLO_ROUTE}", lonely.base()))
        .send()
        .await
        .expect("hello without upstream")
        .json::<MeshHello>()
        .await
        .expect("hello JSON");
    assert!(hello.model.is_none(), "no model while llama-server is down");
    assert!(
        hello.upstream_error.is_some(),
        "the worker explains the problem"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_secret_never_appears_anywhere() {
    capture_log();
    let fake = start_fake().await;
    let worker = worker_in_front_of(&fake, 1).await;
    let secret = worker.secret.clone();
    let client = worker.client();

    let hello = client
        .get(format!("{}{HELLO_ROUTE}", worker.base()))
        .send()
        .await
        .expect("hello")
        .text()
        .await
        .expect("hello body");
    assert!(!hello.contains(&secret), "hello does not echo the secret");

    let props = client
        .get(format!("{}/props", worker.base()))
        .send()
        .await
        .expect("props")
        .text()
        .await
        .expect("props body");
    assert!(!props.contains(&secret), "props do not echo the secret");

    let dead =
        url::Url::parse(&format!("http://127.0.0.1:{}", free_port())).expect("dead upstream");
    let dir = tempfile::tempdir().expect("no-echo dir");
    let identity = WorkerIdentity::create(dir.path(), "no-echo").expect("identity");
    let lonely = start_worker_on(
        identity,
        dir,
        SocketAddr::from(([127, 0, 0, 1], free_port())),
        dead,
        1,
    )
    .await;
    let body = lonely
        .client()
        .get(format!("{}/health", lonely.base()))
        .send()
        .await
        .expect("dead upstream")
        .text()
        .await
        .expect("dead body");
    assert!(
        !body.contains(&lonely.secret),
        "an upstream error does not echo the secret"
    );

    let (status, body) = raw_request(
        lonely.address,
        "GET",
        "/health",
        Some("Bearer wrong".to_owned()),
    )
    .await;
    assert_eq!(status, 401);
    assert!(!body.contains(&lonely.secret));

    assert!(
        !captured_log().contains(&secret),
        "no log line contains the secret:\n{}",
        captured_log()
    );
    assert!(
        !captured_log().contains(&lonely.secret),
        "no log line contains the second secret"
    );

    let code = sail_mesh::PairingCode {
        address: worker.address.to_string(),
        name: "test-worker".into(),
        fingerprint: worker.fingerprint.clone(),
        secret: secret.clone(),
    };
    let record =
        sail_mesh::WorkerRecord::from_code(&code, Vec::new(), "2026-11-14T00:00:00Z".into());
    let mut registry = sail_mesh::Registry::default();
    registry.upsert(record.clone());
    for debug in [
        format!("{code:?}"),
        format!("{record:?}"),
        format!("{registry:?}"),
    ] {
        assert!(!debug.contains(&secret), "Debug hides the secret: {debug}");
    }
}

#[test]
fn the_forwarded_allowlist_is_the_contract() {
    let paths: Vec<&str> = FORWARDED_ROUTES.iter().map(|(_, path)| *path).collect();
    assert_eq!(
        paths,
        vec![
            "/health",
            "/props",
            "/apply-template",
            "/tokenize",
            "/v1/chat/completions"
        ]
    );
}
