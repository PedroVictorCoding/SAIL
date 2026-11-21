//! The compute tunnel: `ggml-rpc-server` bytes carried inside the Mesh TLS channel.
//!
//! Worker side: `GET /mesh/v1/compute` with `Upgrade: sail-rpc/1` returns 101 and then moves
//! raw bytes to `127.0.0.1:<rpc_port>`. Host side: [`HostTunnel`] listens on loopback and
//! forwards each connection to the worker's tunnel over pinned TLS.
use crate::{MeshError, Result, compute::ComputeHandle, registry::WorkerRecord};
use axum::{
    body::Body,
    extract::Request,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use hyper_util::rt::TokioIo;
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, WebPkiSupportedAlgorithms},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use sail_protocol::mesh::{COMPUTE_ROUTE, COMPUTE_UPGRADE};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
    sync::watch,
};

/// Seconds the host is told to wait before retrying a refused tunnel. Fixed a priori: the
/// supervisor re-evaluates lending every 2 s, and the window edge can be up to a minute away.
const LENDING_RETRY_AFTER_SECS: u32 = 60;

/// How long a 101 response waits for the peer to finish the HTTP upgrade.
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the handler waits for the loopback RPC server before answering 502.
const RPC_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the host waits for the worker's TLS handshake and upgrade response.
const HOST_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest upgrade response head accepted from a worker.
const MAX_HEAD_BYTES: usize = 8 * 1024;

/// Lending flag shared by the supervisor and the tunnel handlers.
#[derive(Clone)]
pub struct LendGate {
    sender: Arc<watch::Sender<bool>>,
}

impl LendGate {
    pub fn new() -> Self {
        let (sender, _receiver) = watch::channel(false);
        Self {
            sender: Arc::new(sender),
        }
    }

    pub fn set(&self, lending: bool) {
        // `send` fails when nobody is subscribed yet; the state must still change.
        self.sender.send_replace(lending);
    }

    pub fn lending(&self) -> bool {
        *self.sender.borrow()
    }

    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.sender.subscribe()
    }
}

impl Default for LendGate {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for LendGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LendGate")
            .field("lending", &self.lending())
            .finish()
    }
}

/// Host side: a loopback listener forwarding every connection to one worker's tunnel.
pub struct HostTunnel {
    address: SocketAddr,
    counters: Arc<Counters>,
    stop: Option<watch::Sender<bool>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl HostTunnel {
    /// Listens on 127.0.0.1:0.
    pub async fn bind(record: &WorkerRecord) -> Result<HostTunnel> {
        if record.address.contains(['\r', '\n', ' ']) || record.secret.contains(['\r', '\n']) {
            return Err(MeshError::new(
                "the worker record contains an invalid address or secret",
            ));
        }
        let config = pinned_config(&record.fingerprint)?;
        let server_name = server_name(&record.address)?;
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|e| MeshError::new(format!("Cannot listen for the RPC tunnel: {e}")))?;
        let address = listener
            .local_addr()
            .map_err(|e| MeshError::new(format!("Cannot read the tunnel address: {e}")))?;
        let counters = Arc::new(Counters::default());
        let target = record.address.clone();
        let secret = record.secret.clone();
        let (stop, mut stopped) = watch::channel(false);
        let shared = counters.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stopped.changed() => break,
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        shared.opened_total.fetch_add(1, Ordering::SeqCst);
                        let config = config.clone();
                        let target = target.clone();
                        let secret = secret.clone();
                        let name = server_name.clone();
                        let counters = shared.clone();
                        let stopped = stopped.clone();
                        tokio::spawn(async move {
                            forward(socket, target, secret, name, config, counters, stopped).await;
                        });
                    }
                }
            }
        });
        Ok(HostTunnel {
            address,
            counters,
            stop: Some(stop),
            task: Some(task),
        })
    }

    /// Pass this to llama-server `--rpc`.
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    pub fn stats(&self) -> TunnelStats {
        TunnelStats {
            open: self.counters.open.load(Ordering::SeqCst),
            opened_total: self.counters.opened_total.load(Ordering::SeqCst),
            refused_total: self.counters.refused_total.load(Ordering::SeqCst),
            bytes_up: self.counters.bytes_up.load(Ordering::SeqCst),
            bytes_down: self.counters.bytes_down.load(Ordering::SeqCst),
        }
    }

    pub async fn close(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(true);
        }
        if let Some(task) = self.task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        }
        for _ in 0..20 {
            if self.counters.open.load(Ordering::SeqCst) == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Drop for HostTunnel {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(true);
        }
    }
}

/// Live counters behind [`TunnelStats`].
#[derive(Default)]
struct Counters {
    open: AtomicU32,
    opened_total: AtomicU64,
    refused_total: AtomicU64,
    bytes_up: AtomicU64,
    bytes_down: AtomicU64,
}

/// Count one open tunnel; decrement on drop, including cancellation.
struct OpenGuard(Arc<Counters>);
impl Drop for OpenGuard {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Dial one worker, upgrade, then copy both ways. Every failure counts as refused.
async fn forward(
    client: TcpStream,
    target: String,
    secret: String,
    server_name: ServerName<'static>,
    config: Arc<ClientConfig>,
    counters: Arc<Counters>,
    mut stopped: watch::Receiver<bool>,
) {
    let Ok(connected) = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect(&target),
    )
    .await
    else {
        counters.refused_total.fetch_add(1, Ordering::SeqCst);
        return;
    };
    let Ok(socket) = connected else {
        counters.refused_total.fetch_add(1, Ordering::SeqCst);
        return;
    };
    let Ok(connection) = ClientConnection::new(config, server_name) else {
        counters.refused_total.fetch_add(1, Ordering::SeqCst);
        return;
    };
    let mut tls = TlsStream::new(connection, socket);
    let request = format!(
        "GET {COMPUTE_ROUTE} HTTP/1.1\r\nHost: {target}\r\nAuthorization: Bearer {secret}\r\nConnection: Upgrade\r\nUpgrade: {COMPUTE_UPGRADE}\r\n\r\n"
    );
    let upgraded = tokio::time::timeout(HOST_HANDSHAKE_TIMEOUT, async {
        tls.write_all(request.as_bytes()).await?;
        tls.flush().await?;
        read_head(&mut tls).await?;
        Ok::<(), io::Error>(())
    })
    .await;
    if !matches!(upgraded, Ok(Ok(()))) {
        counters.refused_total.fetch_add(1, Ordering::SeqCst);
        return;
    }
    counters.open.fetch_add(1, Ordering::SeqCst);
    let guard = OpenGuard(counters.clone());
    let mut client = Metered {
        inner: client,
        counters: counters.clone(),
    };
    tokio::select! {
        _ = tokio::io::copy_bidirectional(&mut tls, &mut client) => {}
        _ = wait_stopped(&mut stopped) => {}
    }
    drop(guard);
}

/// Read the HTTP response head, requiring `101 Switching Protocols`.
async fn read_head(tls: &mut TlsStream) -> io::Result<()> {
    let mut head = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while head.len() < MAX_HEAD_BYTES {
        let read = tls.read(&mut byte).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "worker closed before the upgrade completed",
            ));
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            let status = head
                .split(|b| *b == b'\n')
                .next()
                .and_then(|line| line.split(|b| *b == b' ').nth(1))
                .and_then(|code| std::str::from_utf8(code).ok())
                .and_then(|code| code.parse::<u16>().ok());
            return match status {
                Some(101) => Ok(()),
                Some(other) => Err(io::Error::other(format!(
                    "worker refused the tunnel with HTTP {other}"
                ))),
                None => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "worker sent an unreadable upgrade response",
                )),
            };
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "worker upgrade response is too large",
    ))
}

async fn wait_stopped(stopped: &mut watch::Receiver<bool>) {
    loop {
        if *stopped.borrow_and_update() {
            return;
        }
        if stopped.changed().await.is_err() {
            return;
        }
    }
}

/// A client connection whose bytes are counted per direction.
struct Metered {
    inner: TcpStream,
    counters: Arc<Counters>,
}

impl AsyncRead for Metered {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let ready = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &ready {
            let read = buf.filled().len().saturating_sub(before);
            this.counters
                .bytes_up
                .fetch_add(read as u64, Ordering::SeqCst);
        }
        ready
    }
}

impl AsyncWrite for Metered {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let ready = Pin::new(&mut this.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(written)) = &ready {
            this.counters
                .bytes_down
                .fetch_add(*written as u64, Ordering::SeqCst);
        }
        ready
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// A pinned rustls client stream over a tokio socket. `tokio-rustls` is not a workspace
/// dependency, so this carries the small adapter needed for an HTTP upgrade.
struct TlsStream {
    connection: ClientConnection,
    socket: TcpStream,
    out: Vec<u8>,
    out_offset: usize,
}

impl TlsStream {
    fn new(connection: ClientConnection, socket: TcpStream) -> Self {
        Self {
            connection,
            socket,
            out: Vec::new(),
            out_offset: 0,
        }
    }

    /// Write queued TLS records to the socket. `Ready(Ok(()))` means nothing is pending.
    fn poll_flush_tls(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        loop {
            if self.out_offset < self.out.len() {
                match Pin::new(&mut self.socket).poll_write(cx, &self.out[self.out_offset..]) {
                    Poll::Ready(Ok(0)) => {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "the worker closed the tunnel socket",
                        )));
                    }
                    Poll::Ready(Ok(written)) => self.out_offset += written,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending => return Poll::Pending,
                }
                continue;
            }
            self.out.clear();
            self.out_offset = 0;
            if !self.connection.wants_write() {
                return Poll::Ready(Ok(()));
            }
            if let Err(e) = self.connection.write_tls(&mut self.out) {
                return Poll::Ready(Err(e));
            }
            if self.out.is_empty() {
                return Poll::Ready(Err(io::Error::other("the TLS layer made no progress")));
            }
        }
    }

    fn poll_read_plain(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        loop {
            match self.connection.reader().read(buf) {
                Ok(0) => return Poll::Ready(Ok(0)),
                Ok(read) => return Poll::Ready(Ok(read)),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Poll::Ready(Err(e)),
            }
            match self.poll_flush_tls(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) => {}
            }
            let mut chunk = [0u8; 16 * 1024];
            let mut filled = ReadBuf::new(&mut chunk);
            match Pin::new(&mut self.socket).poll_read(cx, &mut filled) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) => {}
            }
            let data = filled.filled();
            if data.is_empty() {
                return Poll::Ready(Ok(0));
            }
            let mut offset = 0;
            while offset < data.len() {
                let read = match self.connection.read_tls(&mut &data[offset..]) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(e) => return Poll::Ready(Err(e)),
                };
                offset += read;
                // Drop the plaintext buffer's used bytes before feeding more: rustls caps
                // its receive deframer at one maximum-size record.
                if let Err(e) = self.connection.process_new_packets() {
                    return Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData, e)));
                }
            }
        }
    }
}

impl AsyncRead for TlsStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let read = {
            let unfilled = buf.initialize_unfilled();
            match this.poll_read_plain(cx, unfilled) {
                Poll::Ready(Ok(read)) => read,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        };
        buf.advance(read);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for TlsStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match this.poll_flush_tls(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) => {}
        }
        let written = match this.connection.writer().write(buf) {
            Ok(0) if !buf.is_empty() => {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "the TLS layer accepted no plaintext",
                )));
            }
            Ok(written) => written,
            Err(e) => return Poll::Ready(Err(e)),
        };
        // If the socket would block, the bytes are already buffered in rustls; the next
        // write or flush drains them.
        if let Poll::Ready(Err(e)) = this.poll_flush_tls(cx) {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(written))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().poll_flush_tls(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.connection.send_close_notify();
        this.poll_flush_tls(cx)
    }
}

/// Accepts exactly the paired certificate and nothing else.
#[derive(Debug)]
struct PinnedFingerprint {
    fingerprint: String,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedFingerprint {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if crate::identity::fingerprint(end_entity.as_ref()) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "the worker certificate does not match the paired fingerprint".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn pinned_config(fingerprint: &str) -> Result<Arc<ClientConfig>> {
    let provider = provider();
    let verifier = PinnedFingerprint {
        fingerprint: fingerprint.to_ascii_lowercase(),
        algorithms: provider.signature_verification_algorithms,
    };
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| MeshError::new(format!("Cannot pin the worker certificate: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn server_name(address: &str) -> Result<ServerName<'static>> {
    let host = if let Some(rest) = address.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        address.rsplit_once(':').map_or(address, |(host, _)| host)
    };
    ServerName::try_from(host.to_owned())
        .map_err(|_| MeshError::new("the worker address has no usable host name"))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelStats {
    pub open: u32,
    pub opened_total: u64,
    pub refused_total: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
}

/// Worker side: turn one authenticated `GET /mesh/v1/compute` into a raw byte pipe to
/// `127.0.0.1:<rpc_port>`. Refuses with 429 unless the supervisor says `Lending`.
pub(crate) async fn accept(handle: &ComputeHandle, mut request: Request) -> Response {
    let upgrade = request
        .headers()
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !upgrade.eq_ignore_ascii_case(COMPUTE_UPGRADE) {
        return error(
            StatusCode::BAD_REQUEST,
            &format!("this route needs Upgrade: {COMPUTE_UPGRADE}"),
        );
    }
    if !handle.gate().lending() {
        let state = handle.snapshot().state;
        let mut response = error(
            StatusCode::TOO_MANY_REQUESTS,
            &format!("worker is not lending ({})", state_name(&state)),
        );
        if let Ok(value) = header::HeaderValue::from_str(&LENDING_RETRY_AFTER_SECS.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        return response;
    }
    let rpc_address = SocketAddr::from(([127, 0, 0, 1], handle.rpc_port()));
    let rpc = match tokio::time::timeout(RPC_CONNECT_TIMEOUT, TcpStream::connect(rpc_address)).await
    {
        Ok(Ok(rpc)) => rpc,
        _ => return error(StatusCode::BAD_GATEWAY, "the RPC server is not running"),
    };
    let on_upgrade = hyper::upgrade::on(&mut request);
    let handle = handle.clone();
    tokio::spawn(async move {
        let Ok(Ok(upgraded)) = tokio::time::timeout(UPGRADE_TIMEOUT, on_upgrade).await else {
            return;
        };
        handle.tunnel_opened();
        let guard = TunnelGuard(handle.clone());
        let mut upgraded = TokioIo::new(upgraded);
        let mut rpc = rpc;
        let mut lending = handle.gate().subscribe();
        tokio::select! {
            _ = tokio::io::copy_bidirectional(&mut upgraded, &mut rpc) => {}
            _ = wait_until_not_lending(&mut lending) => {}
        }
        drop(guard);
    });
    match Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, COMPUTE_UPGRADE)
        .body(Body::empty())
    {
        Ok(response) => response,
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "cannot start the tunnel"),
    }
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(serde_json::json!({"error": message}))).into_response()
}

/// The state in a short plain-words form for the 429 body.
fn state_name(state: &sail_protocol::mesh::LendState) -> &'static str {
    use sail_protocol::mesh::LendState;
    match state {
        LendState::Lending => "lending",
        LendState::Loading => "loading",
        LendState::Paused => "paused",
        LendState::OutsideWindow { .. } => "outside lending hours",
        LendState::Resting { .. } => "resting",
        LendState::Unavailable { .. } => "unavailable",
    }
}

async fn wait_until_not_lending(lending: &mut watch::Receiver<bool>) {
    loop {
        if !*lending.borrow_and_update() {
            return;
        }
        if lending.changed().await.is_err() {
            return;
        }
    }
}

struct TunnelGuard(ComputeHandle);
impl Drop for TunnelGuard {
    fn drop(&mut self) {
        self.0.tunnel_closed();
    }
}
