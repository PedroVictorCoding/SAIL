//! The pinned TLS client must trust exactly one certificate and nothing else, must not follow
//! redirects, and must ignore proxy settings. The worker must refuse the wrong secret even
//! when the certificate is right.
use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use axum_server::tls_rustls::RustlsConfig;
use sail_mesh::{
    identity::WorkerIdentity,
    tls::{pinned_client, server_config},
    worker::{WorkerConfig, serve},
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::task::JoinHandle;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind free port")
        .local_addr()
        .expect("free port address")
        .port()
}

fn address(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

async fn wait_listening(address: SocketAddr) {
    for _ in 0..300 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("server did not start listening on {address}");
}

fn spawn_tls(cert_der: Vec<u8>, key_der: Vec<u8>, port: u16, app: Router) -> JoinHandle<()> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind TLS listener");
    let tls = RustlsConfig::from_config(Arc::new(
        server_config(&cert_der, &key_der).expect("TLS server config"),
    ));
    tokio::spawn(async move {
        let _ = axum_server::from_tcp_rustls(listener, tls)
            .serve(app.into_make_service())
            .await;
    })
}

async fn plain_upstream() -> SocketAddr {
    let app = Router::new().fallback(|| async { "{\"status\":\"ok\"}" });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream");
    let address = listener.local_addr().expect("upstream address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    address
}

async fn start_worker(
    identity: WorkerIdentity,
    upstream: SocketAddr,
) -> (SocketAddr, JoinHandle<()>, tokio::sync::oneshot::Sender<()>) {
    let worker_address = address(free_port());
    let config = WorkerConfig {
        bind: worker_address,
        upstream: url::Url::parse(&format!("http://{upstream}")).expect("upstream URL"),
        max_parallel: 1,
        gate: Default::default(),
    };
    let (stop, wait) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _ = serve(&identity, &config, async move {
            let _ = wait.await;
        })
        .await;
    });
    wait_listening(worker_address).await;
    (worker_address, task, stop)
}

async fn redirect(State(target): State<String>) -> Response {
    (
        StatusCode::FOUND,
        [(header::LOCATION, target.as_str())],
        "no",
    )
        .into_response()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_different_certificate_on_the_same_port_is_refused() {
    let first_dir = tempfile::tempdir().expect("first identity dir");
    let second_dir = tempfile::tempdir().expect("second identity dir");
    let first = WorkerIdentity::create(first_dir.path(), "first").expect("first identity");
    let mut second = WorkerIdentity::create(second_dir.path(), "second").expect("second identity");
    // Same secret, different certificate: only the certificate differs.
    second.secret = first.secret.clone();
    let port = free_port();
    let app = Router::new().fallback(|| async { "ok" });

    let server = spawn_tls(
        first.cert_der.clone(),
        first.key_der.clone(),
        port,
        app.clone(),
    );
    wait_listening(address(port)).await;
    let client = pinned_client(&first.fingerprint(), &first.secret, Duration::from_secs(5))
        .expect("pinned client");
    let accepted = client
        .get(format!("https://{}/health", address(port)))
        .send()
        .await
        .expect("the pinned certificate is accepted");
    assert_eq!(accepted.status(), 200);
    drop(client);
    server.abort();
    let _ = server.await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let replacement = spawn_tls(second.cert_der.clone(), second.key_der.clone(), port, app);
    wait_listening(address(port)).await;
    let client = pinned_client(&first.fingerprint(), &first.secret, Duration::from_secs(5))
        .expect("pinned client");
    let refused = client
        .get(format!("https://{}/health", address(port)))
        .send()
        .await;
    assert!(
        refused.is_err(),
        "a different certificate at the same address is refused: {refused:?}"
    );
    replacement.abort();
    let _ = replacement.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_publicly_valid_but_unpinned_certificate_is_refused() {
    let ca_key = rcgen::KeyPair::generate().expect("CA key");
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).expect("CA certificate");

    let leaf_key = rcgen::KeyPair::generate().expect("leaf key");
    let leaf_params =
        rcgen::CertificateParams::new(vec!["localhost".to_owned()]).expect("leaf params");
    let issuer = rcgen::Issuer::from_params(&ca_params, &ca_key);
    let leaf = leaf_params
        .signed_by(&leaf_key, &issuer)
        .expect("CA-signed leaf certificate");
    let _ = ca.der();

    let port = free_port();
    let app = Router::new().fallback(|| async { "ok" });
    let server = spawn_tls(leaf.der().to_vec(), leaf_key.serialize_der(), port, app);
    wait_listening(address(port)).await;

    let unpinned = pinned_client(&"ab".repeat(32), "unused-secret", Duration::from_secs(5))
        .expect("pinned client");
    let refused = unpinned
        .get(format!("https://{}/health", address(port)))
        .send()
        .await;
    assert!(
        refused.is_err(),
        "a chain a browser would trust is still refused without the pin: {refused:?}"
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_correct_pin_with_the_wrong_secret_is_unauthorized() {
    let dir = tempfile::tempdir().expect("identity dir");
    let identity = WorkerIdentity::create(dir.path(), "worker").expect("identity");
    let pin = identity.fingerprint();
    let upstream = plain_upstream().await;
    let (worker_address, task, _stop) = start_worker(identity, upstream).await;
    let client =
        pinned_client(&pin, "not-the-secret", Duration::from_secs(5)).expect("pinned client");
    let response = client
        .get(format!("https://{worker_address}/health"))
        .send()
        .await
        .expect("request reaches the worker");
    assert_eq!(response.status(), 401);
    assert_eq!(
        response.text().await.expect("refusal body"),
        "{\"error\":\"unauthorized\"}"
    );
    task.abort();
    let _ = task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_client_never_follows_redirects() {
    let dir = tempfile::tempdir().expect("identity dir");
    let identity = WorkerIdentity::create(dir.path(), "redirector").expect("identity");
    let port = free_port();
    let elsewhere = format!("https://127.0.0.1:{}/target", free_port());
    let app = Router::new()
        .route("/health", get(redirect))
        .with_state(elsewhere);
    let server = spawn_tls(
        identity.cert_der.clone(),
        identity.key_der.clone(),
        port,
        app,
    );
    wait_listening(address(port)).await;

    let client = pinned_client(
        &identity.fingerprint(),
        &identity.secret,
        Duration::from_secs(5),
    )
    .expect("pinned client");
    let response = client
        .get(format!("https://{}/health", address(port)))
        .send()
        .await
        .expect("the redirect itself is returned");
    assert_eq!(response.status(), 302, "the redirect is not followed");
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_client_ignores_proxy_settings() {
    let dir = tempfile::tempdir().expect("identity dir");
    let identity = WorkerIdentity::create(dir.path(), "proxy-free").expect("identity");
    let pin = identity.fingerprint();
    let secret = identity.secret.clone();
    let upstream = plain_upstream().await;
    let (worker_address, task, _stop) = start_worker(identity, upstream).await;

    let proxy = format!("http://127.0.0.1:{}", free_port());
    // SAFETY: only this test in this binary touches the proxy variables, no SAIL client in
    // this process reads them (they all use `no_proxy`), and the value points at a closed
    // loopback port so a mistaken proxy use fails instead of leaving the machine.
    unsafe {
        std::env::set_var("HTTPS_PROXY", &proxy);
        std::env::set_var("https_proxy", &proxy);
    }
    let client = pinned_client(&pin, &secret, Duration::from_secs(5)).expect("pinned client");
    let response = client
        .get(format!("https://{worker_address}/health"))
        .send()
        .await;
    unsafe {
        std::env::remove_var("HTTPS_PROXY");
        std::env::remove_var("https_proxy");
    }
    let response = response.expect("the worker is reached directly, not through a proxy");
    assert_eq!(response.status(), 200);
    task.abort();
    let _ = task.await;
}
