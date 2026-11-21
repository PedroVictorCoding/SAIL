//! The compute tunnel end to end: a loopback TLS worker in front of a loopback echo
//! "RPC server", driven by the host tunnel and by a raw TLS client where the HTTP matters.
use sail_mesh::{
    compute::tunnel::LendGate,
    compute::{ComputeHandle, tunnel::HostTunnel},
    identity::WorkerIdentity,
    rest::RestGate,
    tls::pinned_client,
    worker::{WorkerConfig, serve_with_compute},
};
use sail_protocol::mesh::{
    COMPUTE_ROUTE, COMPUTE_UPGRADE, ComputeDevice, LEND_ROUTE, LendMode, LendWindow, MESH_VERSION,
    MeshCompute,
};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
};

// ---------------------------------------------------------------- fake RPC echo server

async fn start_echo() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind echo server");
    let address = listener.local_addr().expect("echo address");
    let connections = Arc::new(AtomicUsize::new(0));
    let seen = connections.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            seen.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let (mut reader, mut writer) = socket.into_split();
                let _ = tokio::io::copy(&mut reader, &mut writer).await;
            });
        }
    });
    (address, connections)
}

// ---------------------------------------------------------------- worker harness

struct TestWorker {
    address: SocketAddr,
    fingerprint: String,
    secret: String,
    gate: LendGate,
    dir: tempfile::TempDir,
    stop: Option<oneshot::Sender<()>>,
    _task: tokio::task::JoinHandle<()>,
}

impl TestWorker {
    fn record(&self, name: &str) -> sail_mesh::WorkerRecord {
        sail_mesh::WorkerRecord {
            name: name.into(),
            address: self.address.to_string(),
            fingerprint: self.fingerprint.clone(),
            secret: self.secret.clone(),
            serves: Vec::new(),
            enabled: true,
            paired_at: "2026-11-21T00:00:00Z".into(),
        }
    }
}

impl Drop for TestWorker {
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

async fn start_worker(rpc_port: u16, lending: bool) -> TestWorker {
    let dir = tempfile::tempdir().expect("worker data dir");
    let identity = WorkerIdentity::create(dir.path(), "compute-test").expect("identity");
    let gate = LendGate::new();
    gate.set(lending);
    let handle = ComputeHandle::new(
        gate.clone(),
        rpc_port,
        dir.path(),
        LendWindow {
            start: "01:00".into(),
            end: "08:00".into(),
        },
        vec![ComputeDevice {
            name: "CPU".into(),
            description: "test CPU".into(),
            lend_bytes: 1024,
        }],
    );
    let fingerprint = identity.fingerprint();
    let secret = identity.secret.clone();
    let address = SocketAddr::from(([127, 0, 0, 1], free_port()));
    let config = WorkerConfig {
        bind: address,
        upstream: url::Url::parse("http://127.0.0.1:9").expect("upstream URL"),
        max_parallel: 1,
        gate: RestGate::default(),
    };
    let worker_gate = gate.clone();
    let (stop, wait) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _ = serve_with_compute(&identity, &config, Some(handle), async move {
            let _ = wait.await;
        })
        .await;
    });
    for _ in 0..300 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    TestWorker {
        address,
        fingerprint,
        secret,
        gate: worker_gate,
        dir,
        stop: Some(stop),
        _task: task,
    }
}

// ---------------------------------------------------------------- raw TLS client

/// Sends one HTTP request and returns (status, headers, body). Trusts any certificate, so it
/// also drives the forged-pin case where the worker is real but the pin is wrong.
fn raw_request(
    address: SocketAddr,
    method: &str,
    path: &str,
    authorization: Option<&str>,
    upgrade: Option<&str>,
) -> (u16, Vec<(String, String)>, String) {
    use rustls::{
        DigitallySignedStruct, SignatureScheme,
        client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        crypto::WebPkiSupportedAlgorithms,
        pki_types::{CertificateDer, ServerName, UnixTime},
    };
    use std::{
        io::{Read, Write},
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
    let connection = rustls::ClientConnection::new(Arc::new(config), name).expect("TLS connection");
    let socket = std::net::TcpStream::connect(address).expect("connect worker");
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    socket
        .set_write_timeout(Some(Duration::from_secs(10)))
        .expect("write timeout");
    let mut stream = rustls::StreamOwned::new(connection, socket);
    let auth = authorization
        .map(|value| format!("Authorization: {value}\r\n"))
        .unwrap_or_default();
    let upgrade = upgrade
        .map(|token| format!("Connection: Upgrade\r\nUpgrade: {token}\r\n"))
        .unwrap_or_default();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: worker\r\n{auth}{upgrade}Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).expect("send request");
    stream.flush().expect("flush request");
    let mut response = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => panic!("read response: {error:?}"),
        }
    }
    let text = String::from_utf8_lossy(&response).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .map(|(head, body)| (head.to_owned(), body.to_owned()))
        .unwrap_or((text.clone(), String::new()));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .expect("HTTP status line");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    (status, headers, body)
}

async fn raw(
    address: SocketAddr,
    method: &'static str,
    path: &'static str,
    authorization: Option<String>,
    upgrade: Option<String>,
) -> (u16, Vec<(String, String)>, String) {
    tokio::task::spawn_blocking(move || {
        raw_request(
            address,
            method,
            path,
            authorization.as_deref(),
            upgrade.as_deref(),
        )
    })
    .await
    .expect("raw request task")
}

// ---------------------------------------------------------------- tests

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eight_mebibytes_round_trip_in_both_directions() {
    let (rpc, _connections) = start_echo().await;
    let worker = start_worker(rpc.port(), true).await;
    let tunnel = HostTunnel::bind(&worker.record("windows"))
        .await
        .expect("host tunnel");
    let mut stream = tokio::net::TcpStream::connect(tunnel.local_addr())
        .await
        .expect("connect host tunnel");

    let payload = vec![0xA5u8; 8 * 1024 * 1024];
    let (mut reader, mut writer) = stream.split();
    let write = async {
        writer.write_all(&payload).await.expect("send 8 MiB");
        writer.flush().await.expect("flush 8 MiB");
    };
    let read = async {
        let mut echoed = vec![0u8; payload.len()];
        reader.read_exact(&mut echoed).await.expect("read 8 MiB");
        echoed
    };
    let ((), echoed) = tokio::join!(write, read);
    assert_eq!(echoed, payload, "every byte survives both directions");

    for _ in 0..100 {
        let stats = tunnel.stats();
        if stats.bytes_up >= payload.len() as u64 && stats.bytes_down >= payload.len() as u64 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let stats = tunnel.stats();
    assert_eq!(stats.opened_total, 1);
    assert_eq!(stats.refused_total, 0);
    assert!(stats.bytes_up >= payload.len() as u64, "{stats:?}");
    assert!(stats.bytes_down >= payload.len() as u64, "{stats:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_that_is_not_lending_refuses_before_the_upgrade() {
    let (rpc, connections) = start_echo().await;
    let worker = start_worker(rpc.port(), false).await;
    let (status, headers, body) = raw(
        worker.address,
        "GET",
        COMPUTE_ROUTE,
        Some(format!("Bearer {}", worker.secret)),
        Some(COMPUTE_UPGRADE.into()),
    )
    .await;
    assert_eq!(status, 429, "{body}");
    let retry = headers
        .iter()
        .find(|(name, _)| name == "retry-after")
        .map(|(_, value)| value.as_str());
    assert_eq!(retry, Some("60"));
    assert!(body.contains("not lending"), "{body}");
    assert_eq!(connections.load(Ordering::SeqCst), 0, "no RPC connection");

    let tunnel = HostTunnel::bind(&worker.record("windows"))
        .await
        .expect("host tunnel");
    let mut stream = tokio::net::TcpStream::connect(tunnel.local_addr())
        .await
        .expect("connect host tunnel");
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut byte))
        .await
        .expect("the host tunnel closes promptly");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    let stats = tunnel.stats();
    assert_eq!(stats.refused_total, 1, "{stats:?}");
    assert_eq!(stats.opened_total, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leaving_lending_closes_open_tunnels_within_five_seconds() {
    let (rpc, _connections) = start_echo().await;
    let worker = start_worker(rpc.port(), true).await;
    let gate = worker.gate.clone();
    let tunnel = HostTunnel::bind(&worker.record("windows"))
        .await
        .expect("host tunnel");
    let mut stream = tokio::net::TcpStream::connect(tunnel.local_addr())
        .await
        .expect("connect host tunnel");
    stream.write_all(b"ping").await.expect("send ping");
    let mut echoed = [0u8; 4];
    stream
        .read_exact(&mut echoed)
        .await
        .expect("tunnel carries bytes");
    assert_eq!(&echoed, b"ping");

    gate.set(false);
    let mut byte = [0u8; 1];
    let closed = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut byte)).await;
    match closed {
        Ok(Ok(0)) | Ok(Err(_)) => {}
        other => panic!("the tunnel stayed open: {other:?}"),
    }
    drop(stream);
    for _ in 0..100 {
        if tunnel.stats().open == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(tunnel.stats().open, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_bearer_gets_401_without_an_upgrade() {
    let (rpc, connections) = start_echo().await;
    let worker = start_worker(rpc.port(), true).await;
    let (status, headers, body) = raw(
        worker.address,
        "GET",
        COMPUTE_ROUTE,
        Some("Bearer wrong-secret".into()),
        Some(COMPUTE_UPGRADE.into()),
    )
    .await;
    assert_eq!(status, 401, "{body}");
    assert!(
        headers.iter().all(|(name, _)| name != "upgrade"),
        "no upgrade was offered: {headers:?}"
    );
    assert_eq!(body, "{\"error\":\"unauthorized\"}");
    assert_eq!(connections.load(Ordering::SeqCst), 0, "no RPC connection");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forged_pin_is_refused_and_never_reaches_the_rpc_server() {
    let (rpc, connections) = start_echo().await;
    let worker = start_worker(rpc.port(), true).await;
    let mut record = worker.record("windows");
    record.fingerprint = "0".repeat(64);
    let tunnel = HostTunnel::bind(&record).await.expect("host tunnel");
    let mut stream = tokio::net::TcpStream::connect(tunnel.local_addr())
        .await
        .expect("connect host tunnel");
    let _ = stream.write_all(b"hello").await;
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut byte)).await;
    assert!(matches!(read, Ok(Ok(0)) | Ok(Err(_))), "{read:?}");
    for _ in 0..100 {
        if tunnel.stats().refused_total >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(tunnel.stats().refused_total, 1);
    assert_eq!(connections.load(Ordering::SeqCst), 0, "the pin stopped it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zero_byte_upgrade_does_not_hurt_the_worker() {
    let (rpc, _connections) = start_echo().await;
    let worker = start_worker(rpc.port(), true).await;
    let socket = tokio::net::TcpStream::connect(worker.address)
        .await
        .expect("connect worker");
    drop(socket);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let client = pinned_client(&worker.fingerprint, &worker.secret, Duration::from_secs(5))
        .expect("pinned client");
    let hello = client
        .get(format!("https://{}/mesh/v1/hello", worker.address))
        .send()
        .await
        .expect("worker still answers")
        .json::<sail_protocol::mesh::MeshHello>()
        .await
        .expect("hello JSON");
    assert_eq!(hello.version, MESH_VERSION);
    assert!(hello.compute.is_some(), "the compute snapshot is reported");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn post_lend_writes_lend_json_and_returns_the_snapshot() {
    let (rpc, _connections) = start_echo().await;
    let worker = start_worker(rpc.port(), true).await;
    let client = pinned_client(&worker.fingerprint, &worker.secret, Duration::from_secs(5))
        .expect("pinned client");
    let response = client
        .post(format!("https://{}{LEND_ROUTE}", worker.address))
        .json(&serde_json::json!({"mode": {"mode": "pause"}}))
        .send()
        .await
        .expect("post lend");
    assert_eq!(response.status(), 200);
    let snapshot: MeshCompute = response.json().await.expect("compute snapshot");
    assert_eq!(snapshot.mode, LendMode::Pause);

    let saved: LendMode = serde_json::from_slice(
        &std::fs::read(worker.dir.path().join("lend.json")).expect("lend.json"),
    )
    .expect("saved mode");
    assert_eq!(saved, LendMode::Pause);

    let hello = client
        .get(format!("https://{}/mesh/v1/hello", worker.address))
        .send()
        .await
        .expect("hello")
        .json::<sail_protocol::mesh::MeshHello>()
        .await
        .expect("hello JSON");
    assert_eq!(
        hello.compute.expect("compute snapshot").mode,
        LendMode::Pause
    );

    let malformed = client
        .post(format!("https://{}{LEND_ROUTE}", worker.address))
        .body("[]")
        .send()
        .await
        .expect("malformed lend");
    assert_eq!(malformed.status(), 400);
}
