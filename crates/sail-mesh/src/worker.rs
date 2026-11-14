//! The worker: an authenticated TLS front for the machine's own llama-server.
//!
//! It forwards only [`crate::FORWARDED_ROUTES`] to one configured upstream, caps concurrent
//! generations, and answers `/mesh/v1/hello`. Closing the host connection drops the upstream
//! request, which makes llama-server stop generating (that is how cancellation reaches it).
use crate::{FORWARDED_ROUTES, HELLO_ROUTE, MeshError, Result, identity::WorkerIdentity};
use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use sail_protocol::mesh::{MESH_VERSION, MeshHello, MeshModel};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

/// Largest request body forwarded: two 4 MB images as base64 plus prompt text.
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// Largest non-streaming upstream reply (metadata, template, tokens).
const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;
/// Failed authentications from one address that trip the throttle.
const AUTH_MAX_FAILURES: u32 = 20;
/// Window in which those failures are counted.
const AUTH_WINDOW: Duration = Duration::from_secs(60);
/// How long an address that tripped the throttle is refused.
const AUTH_LOCKOUT: Duration = Duration::from_secs(60);
/// Most addresses the throttle remembers; the one idle longest is evicted.
const AUTH_MAX_ADDRESSES: usize = 1024;

/// Where the auth throttle gets the time; tests substitute a clock they control.
pub(crate) type NowFn = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub bind: SocketAddr,
    /// llama-server base URL on this machine, for example `http://127.0.0.1:8080`.
    pub upstream: url::Url,
    pub max_parallel: usize,
}

struct Shared {
    identity_id: String,
    name: String,
    secret: Vec<u8>,
    upstream: url::Url,
    client: reqwest::Client,
    slots: Arc<Semaphore>,
    max_parallel: usize,
    active: AtomicUsize,
    auth: Mutex<AuthThrottle>,
}

#[derive(Debug, Clone, Copy)]
struct AuthEntry {
    failures: u32,
    window_start: Instant,
    blocked_until: Option<Instant>,
    last_seen: Instant,
}

/// Per-address failed-authentication tracker. Successful authentication says nothing here:
/// the window only moves forward on failures, so guessing cannot be spread out by a paired
/// host that happens to ask for the state in between.
pub(crate) struct AuthThrottle {
    now: NowFn,
    addresses: HashMap<IpAddr, AuthEntry>,
}

impl AuthThrottle {
    pub(crate) fn new(now: NowFn) -> Self {
        Self {
            now,
            addresses: HashMap::new(),
        }
    }

    /// Whether this address is inside a lockout right now.
    pub(crate) fn is_blocked(&self, address: IpAddr) -> bool {
        let now = (self.now)();
        self.addresses
            .get(&address)
            .and_then(|entry| entry.blocked_until)
            .is_some_and(|until| now < until)
    }

    /// Count one failed authentication and start a lockout when the window fills up.
    pub(crate) fn record_failure(&mut self, address: IpAddr) {
        let now = (self.now)();
        if !self.addresses.contains_key(&address) && self.addresses.len() >= AUTH_MAX_ADDRESSES {
            let oldest = self
                .addresses
                .iter()
                .min_by_key(|(_, entry)| entry.last_seen)
                .map(|(address, _)| *address);
            if let Some(oldest) = oldest {
                self.addresses.remove(&oldest);
            }
        }
        let entry = self.addresses.entry(address).or_insert(AuthEntry {
            failures: 0,
            window_start: now,
            blocked_until: None,
            last_seen: now,
        });
        if now.saturating_duration_since(entry.window_start) >= AUTH_WINDOW {
            entry.window_start = now;
            entry.failures = 0;
            entry.blocked_until = None;
        }
        entry.failures = entry.failures.saturating_add(1);
        entry.last_seen = now;
        if entry.failures >= AUTH_MAX_FAILURES {
            entry.blocked_until = Some(now + AUTH_LOCKOUT);
        }
    }
}

/// Check the upstream URL: plain HTTP(S) base URL, no credentials, query or path.
pub fn parse_upstream(raw: &str) -> Result<url::Url> {
    let url =
        url::Url::parse(raw).map_err(|e| MeshError::new(format!("Invalid --upstream URL: {e}")))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(MeshError::new(
            "--upstream must be a base URL like http://127.0.0.1:8080 (no path, query or credentials)",
        ));
    }
    Ok(url)
}

/// Build the worker's HTTP application (without TLS; [`serve`] adds it). Public so tests
/// and embedders can drive it directly.
pub fn router(identity: &WorkerIdentity, config: &WorkerConfig) -> Result<Router> {
    router_with_clock(identity, config, Arc::new(Instant::now))
}

/// [`router`] with an injectable clock, so the auth throttle can be tested without waiting.
pub(crate) fn router_with_clock(
    identity: &WorkerIdentity,
    config: &WorkerConfig,
    now: NowFn,
) -> Result<Router> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| MeshError::new(e.to_string()))?;
    let max_parallel = config.max_parallel.clamp(1, 16);
    let shared = Arc::new(Shared {
        identity_id: identity.worker_id.clone(),
        name: identity.name.clone(),
        secret: identity.secret.as_bytes().to_vec(),
        upstream: config.upstream.clone(),
        client,
        slots: Arc::new(Semaphore::new(max_parallel)),
        max_parallel,
        active: AtomicUsize::new(0),
        auth: Mutex::new(AuthThrottle::new(now)),
    });
    Ok(Router::new()
        .fallback(handle)
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(shared))
}

/// Serve the worker over TLS until `shutdown` resolves.
pub async fn serve(
    identity: &WorkerIdentity,
    config: &WorkerConfig,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let app = router(identity, config)?;
    let tls = crate::tls::server_config(&identity.cert_der, &identity.key_der)?;
    let tls = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls));
    let handle = axum_server::Handle::new();
    let stopper = handle.clone();
    tokio::spawn(async move {
        shutdown.await;
        stopper.graceful_shutdown(Some(Duration::from_secs(5)));
    });
    axum_server::bind_rustls(config.bind, tls)
        .handle(handle)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .map_err(|e| MeshError::new(format!("Worker server stopped: {e}")))
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(serde_json::json!({"error": message}))).into_response()
}

fn authorized(shared: &Shared, headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|token| crate::same_secret(token.trim().as_bytes(), &shared.secret))
}

/// The address a request came from; only meaningful when the server ran with connect info.
fn client_address(request: &Request) -> IpAddr {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect| connect.0.ip())
        .unwrap_or(IpAddr::from([0, 0, 0, 0]))
}

/// The upstream route for an exactly matching method and raw path, if the worker forwards it.
fn forwarded(method: &Method, path: &str) -> Option<&'static str> {
    FORWARDED_ROUTES
        .iter()
        .find(|(allowed_method, route)| *route == path && method.as_str() == *allowed_method)
        .map(|(_, route)| *route)
}

async fn handle(State(shared): State<Arc<Shared>>, request: Request) -> Response {
    let address = client_address(&request);
    {
        let auth = shared
            .auth
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if auth.is_blocked(address) {
            return error(StatusCode::TOO_MANY_REQUESTS, "too many attempts");
        }
    }
    if !authorized(&shared, request.headers()) {
        shared
            .auth
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .record_failure(address);
        // Same answer for a wrong secret and a missing one; nothing about the worker leaks.
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let path = request.uri().path();
    if request.uri().query().is_some() {
        return error(StatusCode::BAD_REQUEST, "query strings are not forwarded");
    }
    if path == HELLO_ROUTE && request.method() == Method::GET {
        return axum::Json(hello(&shared).await).into_response();
    }
    let method = request.method().clone();
    let Some(route) = forwarded(&method, path) else {
        return error(StatusCode::NOT_FOUND, "route is not forwarded by SAIL Mesh");
    };
    let generation = route == "/v1/chat/completions";
    let permit = if generation {
        match shared.slots.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "worker is busy"),
        }
    } else {
        None
    };
    let body = match axum::body::to_bytes(request.into_body(), MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
    };
    let url = shared.upstream.join(route).expect("static route");
    let mut upstream = shared.client.request(method, url);
    if !body.is_empty() {
        upstream = upstream
            .header(header::CONTENT_TYPE, "application/json")
            .body(body);
    }
    let response = match upstream.send().await {
        Ok(response) => response,
        Err(e) => {
            tracing::warn!(error = %e, route, "llama-server did not answer");
            return error(StatusCode::BAD_GATEWAY, "model server unavailable");
        }
    };
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_owned();
    if !generation {
        let mut data = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => data.extend_from_slice(&chunk),
                Err(_) => return error(StatusCode::BAD_GATEWAY, "model server disconnected"),
            }
            if data.len() > MAX_METADATA_BYTES {
                return error(StatusCode::BAD_GATEWAY, "model server reply too large");
            }
        }
        if route == "/props" {
            data = redact_props(&data);
        }
        // Only these two headers ever leave the worker: upstream headers may describe its
        // internals and must not reach the host.
        return (
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, "no-store".to_owned()),
            ],
            data,
        )
            .into_response();
    }
    // Stream the generation through. The permit and active count live as long as the body.
    shared.active.fetch_add(1, Ordering::SeqCst);
    let guard = ActiveGuard {
        shared: shared.clone(),
        _permit: permit,
    };
    let stream = response.bytes_stream().map(move |chunk| {
        let _held = &guard;
        chunk.map_err(std::io::Error::other)
    });
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(stream))
        .expect("valid response")
}

struct ActiveGuard {
    shared: Arc<Shared>,
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.shared.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// `/props` reports the full model path on the worker. The host only needs the file name
/// (to pick the right chat template), so the directory never leaves the worker.
fn redact_props(data: &[u8]) -> Vec<u8> {
    let Ok(mut props) = serde_json::from_slice::<serde_json::Value>(data) else {
        return data.to_vec();
    };
    if let Some(path) = props["model_path"].as_str() {
        let name = file_name(path);
        props["model_path"] = serde_json::Value::String(name);
    }
    serde_json::to_vec(&props).unwrap_or_else(|_| data.to_vec())
}

/// File name of a path written on any OS (llama-server on Windows reports `C:\...`).
fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned()
}

async fn hello(shared: &Shared) -> MeshHello {
    let (model, upstream_error) = match upstream_model(shared).await {
        Ok(model) => (Some(model), None),
        Err(e) => (None, Some(e)),
    };
    MeshHello {
        version: MESH_VERSION,
        worker_id: shared.identity_id.clone(),
        name: shared.name.clone(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        max_parallel: shared.max_parallel,
        active: shared.active.load(Ordering::SeqCst),
        model,
        upstream_error,
    }
}

async fn upstream_model(shared: &Shared) -> std::result::Result<MeshModel, String> {
    let get = |route: &str| {
        shared
            .client
            .get(shared.upstream.join(route).expect("static route"))
            .timeout(Duration::from_secs(5))
            .send()
    };
    let health = get("/health")
        .await
        .map_err(|_| "model server is not running".to_owned())?;
    if !health.status().is_success() {
        return Err(format!("model server is not ready ({})", health.status()));
    }
    let props: serde_json::Value = get("/props")
        .await
        .map_err(|_| "model server did not report its model".to_owned())?
        .json()
        .await
        .map_err(|_| "model server reported malformed properties".to_owned())?;
    let file = props["model_path"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(file_name)
        .ok_or_else(|| "model server did not report its model file".to_owned())?;
    let context = props["default_generation_settings"]["n_ctx"]
        .as_u64()
        .or_else(|| props["n_ctx"].as_u64())
        .unwrap_or(0) as usize;
    Ok(MeshModel {
        file_name: file,
        context_tokens: context,
        vision: props["modalities"]["vision"] == true,
        tools: props["chat_template"]
            .as_str()
            .is_some_and(|t| t.contains("tools") || t.contains("tool_calls")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeClock(Mutex<Instant>);

    impl FakeClock {
        fn new() -> Arc<Self> {
            Arc::new(Self(Mutex::new(Instant::now())))
        }

        fn advance(&self, duration: Duration) {
            *self
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) += duration;
        }

        fn now_fn(self: &Arc<Self>) -> NowFn {
            let clock = self.clone();
            Arc::new(move || {
                *clock
                    .0
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            })
        }
    }

    #[test]
    fn upstream_must_be_a_bare_base_url() {
        assert!(parse_upstream("http://127.0.0.1:8080").is_ok());
        assert!(parse_upstream("http://127.0.0.1:8080/").is_ok());
        for bad in [
            "ftp://x",
            "http://u:p@127.0.0.1:8080",
            "http://127.0.0.1:8080/v1",
            "http://127.0.0.1:8080/?a=1",
        ] {
            assert!(parse_upstream(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn forwarded_matches_only_the_exact_raw_path() {
        assert_eq!(forwarded(&Method::GET, "/health"), Some("/health"));
        assert_eq!(
            forwarded(&Method::POST, "/v1/chat/completions"),
            Some("/v1/chat/completions")
        );
        for path in [
            "/v1/models",
            "/slots",
            "/metrics",
            "/../health",
            "/%2e%2e/health",
            "//health",
            "/health/",
            "/health?x=1",
        ] {
            assert_eq!(forwarded(&Method::GET, path), None, "{path}");
        }
        assert_eq!(forwarded(&Method::POST, "/health"), None);
        assert_eq!(forwarded(&Method::GET, "/v1/chat/completions"), None);
    }

    #[test]
    fn throttle_blocks_after_twenty_failures_and_lifts() {
        let clock = FakeClock::new();
        let mut throttle = AuthThrottle::new(clock.now_fn());
        let address = IpAddr::from([192, 0, 2, 1]);
        for attempt in 1..=AUTH_MAX_FAILURES {
            assert!(!throttle.is_blocked(address), "attempt {attempt}");
            clock.advance(Duration::from_secs(1));
            throttle.record_failure(address);
        }
        assert!(throttle.is_blocked(address));
        clock.advance(AUTH_LOCKOUT - Duration::from_secs(1));
        assert!(throttle.is_blocked(address), "lockout lasts a minute");
        clock.advance(Duration::from_secs(2));
        assert!(!throttle.is_blocked(address), "lockout ends");
        assert!(!throttle.is_blocked(IpAddr::from([192, 0, 2, 2])));

        // A new window starts after the lockout: it takes twenty more failures to trip again.
        throttle.record_failure(address);
        assert!(!throttle.is_blocked(address));
        clock.advance(Duration::from_secs(1));
        throttle.record_failure(address);
        assert!(!throttle.is_blocked(address));
    }

    #[test]
    fn throttle_counts_failures_across_a_quiet_minute() {
        let clock = FakeClock::new();
        let mut throttle = AuthThrottle::new(clock.now_fn());
        let address = IpAddr::from([192, 0, 2, 1]);
        for _ in 0..10 {
            throttle.record_failure(address);
        }
        clock.advance(Duration::from_secs(30));
        for _ in 0..10 {
            throttle.record_failure(address);
        }
        assert!(throttle.is_blocked(address));
    }

    #[test]
    fn throttle_keeps_at_most_1024_addresses() {
        let clock = FakeClock::new();
        let mut throttle = AuthThrottle::new(clock.now_fn());
        let address = |index: u32| IpAddr::from(std::net::Ipv4Addr::from(index.to_be_bytes()));
        for index in 0..(AUTH_MAX_ADDRESSES as u32) {
            clock.advance(Duration::from_millis(1));
            throttle.record_failure(address(index));
        }
        assert_eq!(throttle.addresses.len(), AUTH_MAX_ADDRESSES);
        assert!(throttle.addresses.contains_key(&address(0)));
        clock.advance(Duration::from_millis(1));
        throttle.record_failure(address(AUTH_MAX_ADDRESSES as u32));
        assert_eq!(throttle.addresses.len(), AUTH_MAX_ADDRESSES);
        assert!(
            !throttle.addresses.contains_key(&address(0)),
            "the address idle longest is evicted"
        );
        assert!(throttle.addresses.contains_key(&address(1)));
    }

    #[test]
    fn props_keep_only_the_model_file_name() {
        for (path, name) in [
            (r"C:\Users\me\models\qwen.gguf", "qwen.gguf"),
            ("/home/me/models/qwen.gguf", "qwen.gguf"),
            ("qwen.gguf", "qwen.gguf"),
        ] {
            let data =
                serde_json::to_vec(&serde_json::json!({"model_path": path, "n_ctx": 4})).unwrap();
            let out: serde_json::Value = serde_json::from_slice(&redact_props(&data)).unwrap();
            assert_eq!(out["model_path"], name);
            assert_eq!(out["n_ctx"], 4);
        }
    }
}
