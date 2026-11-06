//! Unix-domain HTTP CONNECT proxy. The proxy is a pure tunnel: it does not terminate TLS, so it
//! cannot inspect encrypted paths, methods, headers or redirects. Its guarantees stop at
//! destination authorization, address validation and byte/time ceilings.
use crate::network::{NetworkBroker, NetworkError, ProxyProtocol};
use base64::Engine;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio::sync::Semaphore;

pub const MAX_HEADER_BYTES: usize = 8 * 1024;
pub const MAX_CONNECTIONS: usize = 256;

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// The dedicated runner UID. The daemon must never share it.
    pub allowed_peer_uid: u32,
    pub max_header_bytes: usize,
    pub handshake_timeout: Duration,
    pub max_connections: usize,
    #[cfg(test)]
    pub(crate) allow_same_uid: bool,
}

impl ProxyConfig {
    pub fn new(allowed_peer_uid: u32) -> Self {
        Self {
            allowed_peer_uid,
            max_header_bytes: MAX_HEADER_BYTES,
            handshake_timeout: Duration::from_secs(5),
            max_connections: 64,
            #[cfg(test)]
            allow_same_uid: false,
        }
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.allowed_peer_uid == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "proxy requires a dedicated non-root runner UID",
            ));
        }
        #[cfg(not(test))]
        if self.allowed_peer_uid == unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "proxy peer UID must differ from the daemon UID",
            ));
        }
        #[cfg(test)]
        if !self.allow_same_uid && self.allowed_peer_uid == unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "proxy peer UID must differ from the daemon UID",
            ));
        }
        if !(1024..=MAX_HEADER_BYTES).contains(&self.max_header_bytes)
            || self.handshake_timeout < Duration::from_secs(1)
            || self.handshake_timeout > Duration::from_secs(30)
            || !(1..=MAX_CONNECTIONS).contains(&self.max_connections)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "proxy configuration is outside service ceilings",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyRequest {
    pub host: String,
    pub port: u16,
    pub binding: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyReject {
    BadRequest(&'static str),
    AuthenticationRequired,
    Forbidden,
    MethodNotAllowed,
    UpstreamUnavailable,
    TooManyConnections,
}

impl ProxyReject {
    fn response(&self) -> &'static [u8] {
        match self {
            Self::BadRequest(_) => {
                b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
            }
            Self::AuthenticationRequired => {
                b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Bearer realm=\"sail\"\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
            }
            Self::Forbidden => {
                b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
            }
            Self::MethodNotAllowed => {
                b"HTTP/1.1 405 Method Not Allowed\r\nAllow: CONNECT\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
            }
            Self::UpstreamUnavailable => {
                b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
            }
            Self::TooManyConnections => {
                b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
            }
        }
    }
}

/// Serve CONNECT only. Every accepted peer must be the dedicated runner UID, and every request
/// must carry a job-bound binding token issued by the broker.
pub async fn serve(
    listener: UnixListener,
    broker: Arc<NetworkBroker>,
    config: ProxyConfig,
) -> io::Result<()> {
    config.validate()?;
    let slots = Arc::new(Semaphore::new(config.max_connections));
    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            reject(stream, ProxyReject::TooManyConnections).await;
            continue;
        };
        let broker = broker.clone();
        let config = config.clone();
        tokio::spawn(async move {
            let _slot = slot;
            if let Err(error) = handle_stream(stream, broker, config).await {
                tracing::debug!(%error, "network proxy connection closed");
            }
        });
    }
}

async fn reject(mut stream: UnixStream, reject: ProxyReject) {
    let _ = stream.write_all(reject.response()).await;
    let _ = stream.shutdown().await;
}

async fn handle_stream(
    mut stream: UnixStream,
    broker: Arc<NetworkBroker>,
    config: ProxyConfig,
) -> io::Result<()> {
    let credentials = stream.peer_cred()?;
    if credentials.uid() != config.allowed_peer_uid {
        reject(stream, ProxyReject::Forbidden).await;
        return Ok(());
    }
    let headers = match tokio::time::timeout(
        config.handshake_timeout,
        read_headers(&mut stream, config.max_header_bytes),
    )
    .await
    {
        Ok(Ok(headers)) => headers,
        Ok(Err(problem)) => {
            reject(stream, problem).await;
            return Ok(());
        }
        Err(_) => {
            reject(stream, ProxyReject::BadRequest("handshake timeout")).await;
            return Ok(());
        }
    };
    let request = match parse_connect(&headers) {
        Ok(request) => request,
        Err(problem) => {
            reject(stream, problem).await;
            return Ok(());
        }
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    let grant = match broker.grant_for(&request.binding, now_ms) {
        Ok(grant) => grant,
        Err(NetworkError::GrantNotFound) => {
            reject(stream, ProxyReject::AuthenticationRequired).await;
            return Ok(());
        }
        Err(_) => {
            reject(stream, ProxyReject::Forbidden).await;
            return Ok(());
        }
    };
    // Register before resolution/connect so revocation cancels this attempt even while DNS or
    // the upstream handshake is in flight. The guard removes the slot on every return path.
    let (connection_id, cancel) = broker.register_connection(grant.id);
    let _registration = ConnectionGuard {
        broker: broker.clone(),
        grant_id: grant.id,
        connection_id,
    };
    let limit = broker
        .policy_max_connections(&grant.policy.policy_id, grant.policy.policy_version)
        .unwrap_or(1) as usize;
    if broker.active_connections(grant.id) > limit {
        reject(stream, ProxyReject::TooManyConnections).await;
        return Ok(());
    }
    let destination = match broker
        .authorize(
            &request.binding,
            &request.host,
            request.port,
            ProxyProtocol::Https,
            now_ms,
        )
        .await
    {
        Ok(destination) => destination,
        Err(error) => {
            tracing::warn!(
                job_id = %grant.job_id,
                host = %request.host,
                port = request.port,
                reason = %error,
                "network destination denied"
            );
            reject(stream, ProxyReject::Forbidden).await;
            return Ok(());
        }
    };
    if *cancel.borrow() {
        reject(stream, ProxyReject::Forbidden).await;
        return Ok(());
    }
    let limits = broker.policy_limits(&destination.policy_id, destination.policy_version);
    let connect_timeout = limits.map(|(_, _, _, connect)| connect).unwrap_or(10_000);
    let upstream = match connect_upstream(&destination, connect_timeout).await {
        Ok(upstream) => upstream,
        Err(_) => {
            reject(stream, ProxyReject::UpstreamUnavailable).await;
            return Ok(());
        }
    };
    if grant.is_revoked() || *cancel.borrow() {
        reject(stream, ProxyReject::Forbidden).await;
        return Ok(());
    }
    tracing::info!(
        job_id = %grant.job_id,
        host = %destination.host,
        port = destination.port,
        policy = %destination.policy_id,
        rule = %destination.matched_rule,
        "network destination allowed"
    );
    stream
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    pipeline(stream, upstream, cancel, limits).await
}

struct ConnectionGuard {
    broker: Arc<NetworkBroker>,
    grant_id: uuid::Uuid,
    connection_id: u64,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.broker
            .unregister_connection(self.grant_id, self.connection_id);
    }
}

async fn connect_upstream(
    destination: &crate::network::AllowedDestination,
    connect_timeout_ms: u64,
) -> io::Result<TcpStream> {
    for ip in &destination.ips {
        let target = std::net::SocketAddr::new(*ip, destination.port);
        if let Ok(Ok(stream)) = tokio::time::timeout(
            Duration::from_millis(connect_timeout_ms),
            TcpStream::connect(target),
        )
        .await
        {
            return Ok(stream);
        }
    }
    Err(io::Error::other("all validated destinations failed"))
}

async fn pipeline(
    client: UnixStream,
    upstream: TcpStream,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    limits: Option<(u64, u64, u64, u64)>,
) -> io::Result<()> {
    let (max_bytes, max_bps, idle_ms, _) =
        limits.unwrap_or((64 * 1024 * 1024, 16 * 1024 * 1024, 30_000, 10_000));
    let total = Arc::new(AtomicU64::new(0));
    let started = Instant::now();
    let (client_read, client_write) = tokio::io::split(client);
    let (upstream_read, upstream_write) = tokio::io::split(upstream);
    let cancellation = async {
        loop {
            if *cancel.borrow() || cancel.changed().await.is_err() {
                return;
            }
        }
    };
    let client_to_upstream = pump(
        client_read,
        upstream_write,
        total.clone(),
        started,
        max_bytes,
        max_bps,
        idle_ms,
    );
    let upstream_to_client = pump(
        upstream_read,
        client_write,
        total,
        started,
        max_bytes,
        max_bps,
        idle_ms,
    );
    tokio::select! {
        _ = client_to_upstream => {}
        _ = upstream_to_client => {}
        _ = cancellation => {}
    }
    Ok(())
}

async fn pump<R, W>(
    mut reader: R,
    mut writer: W,
    total: Arc<AtomicU64>,
    started: Instant,
    max_bytes: u64,
    max_bps: u64,
    idle_ms: u64,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = [0u8; 8 * 1024];
    loop {
        let read = tokio::time::timeout(Duration::from_millis(idle_ms), reader.read(&mut buffer))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "proxy idle timeout"))??;
        if read == 0 {
            return Ok(());
        }
        let before = total.fetch_add(read as u64, Ordering::AcqRel);
        if before + read as u64 > max_bytes {
            return Err(io::Error::other("proxy connection byte ceiling"));
        }
        writer.write_all(&buffer[..read]).await?;
        let expected = Duration::from_millis((before + read as u64).saturating_mul(1000) / max_bps);
        let elapsed = started.elapsed();
        if expected > elapsed {
            tokio::time::sleep(expected - elapsed).await;
        }
    }
}

/// Read one bounded HTTP header block. Folding, NULs and oversized headers are rejected.
async fn read_headers(stream: &mut UnixStream, limit: usize) -> Result<Vec<u8>, ProxyReject> {
    let mut data = Vec::with_capacity(1024);
    let mut buffer = [0u8; 1024];
    loop {
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(|_| ProxyReject::BadRequest("read"))?;
        if read == 0 {
            return Err(ProxyReject::BadRequest("closed"));
        }
        data.extend_from_slice(&buffer[..read]);
        if data.len() > limit {
            return Err(ProxyReject::BadRequest("header limit"));
        }
        if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            if end + 4 != data.len() {
                return Err(ProxyReject::BadRequest("pipelined data before 200"));
            }
            return Ok(data);
        }
        if data.contains(&0) {
            return Err(ProxyReject::BadRequest("nul"));
        }
    }
}

/// Parse exactly one `CONNECT host:port HTTP/1.1` request. The Host header is ignored: the
/// authority in the request line is the only destination.
fn parse_connect(headers: &[u8]) -> Result<ProxyRequest, ProxyReject> {
    let text = std::str::from_utf8(headers).map_err(|_| ProxyReject::BadRequest("encoding"))?;
    let head = text
        .split("\r\n\r\n")
        .next()
        .ok_or(ProxyReject::BadRequest("empty"))?;
    let mut lines = head.split("\r\n");
    let request_line = lines
        .next()
        .ok_or(ProxyReject::BadRequest("request line"))?;
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    if parts.next().is_some() || version != "HTTP/1.1" {
        return Err(ProxyReject::BadRequest("request line"));
    }
    if !method.eq_ignore_ascii_case("CONNECT") {
        return Err(ProxyReject::MethodNotAllowed);
    }
    if target.contains('/') || target.contains('@') || target.contains('\0') {
        return Err(ProxyReject::BadRequest("authority"));
    }
    let (host, port) = split_authority(target)?;
    if host.is_empty() || port == 0 {
        return Err(ProxyReject::BadRequest("authority"));
    }
    let mut auth: Option<&str> = None;
    let mut count = 0usize;
    for line in lines {
        count += 1;
        if count > 64 || line.starts_with(' ') || line.starts_with('\t') {
            return Err(ProxyReject::BadRequest("headers"));
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(ProxyReject::BadRequest("header"));
        };
        if name.eq_ignore_ascii_case("proxy-authorization") {
            if auth.is_some() {
                return Err(ProxyReject::BadRequest("duplicate authorization"));
            }
            auth = Some(value.trim());
        }
    }
    let binding = parse_authorization(auth.ok_or(ProxyReject::AuthenticationRequired)?)?;
    Ok(ProxyRequest {
        host,
        port,
        binding,
    })
}

fn split_authority(target: &str) -> Result<(String, u16), ProxyReject> {
    if let Some(rest) = target.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or(ProxyReject::BadRequest("ipv6"))?;
        let port = tail
            .strip_prefix(':')
            .ok_or(ProxyReject::BadRequest("port"))?
            .parse()
            .map_err(|_| ProxyReject::BadRequest("port"))?;
        return Ok((host.into(), port));
    }
    let (host, port) = target
        .rsplit_once(':')
        .ok_or(ProxyReject::BadRequest("port"))?;
    if host.contains(':') {
        return Err(ProxyReject::BadRequest("authority"));
    }
    let port = port.parse().map_err(|_| ProxyReject::BadRequest("port"))?;
    Ok((host.into(), port))
}

fn parse_authorization(value: &str) -> Result<String, ProxyReject> {
    let (scheme, rest) = value
        .split_once(' ')
        .ok_or(ProxyReject::AuthenticationRequired)?;
    if scheme.eq_ignore_ascii_case("bearer") {
        return valid_binding(rest.trim()).ok_or(ProxyReject::AuthenticationRequired);
    }
    if scheme.eq_ignore_ascii_case("basic") {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(rest.trim())
            .map_err(|_| ProxyReject::AuthenticationRequired)?;
        let decoded =
            String::from_utf8(decoded).map_err(|_| ProxyReject::AuthenticationRequired)?;
        let (_, token) = decoded
            .split_once(':')
            .ok_or(ProxyReject::AuthenticationRequired)?;
        return valid_binding(token).ok_or(ProxyReject::AuthenticationRequired);
    }
    Err(ProxyReject::AuthenticationRequired)
}

fn valid_binding(token: &str) -> Option<String> {
    let hex = token.strip_prefix("v1.")?;
    if hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Some(token.to_owned())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::{
        Effect, HostPattern, NetworkLimits, NetworkPolicy, NetworkRule, Resolver,
    };
    use async_trait::async_trait;
    use std::net::IpAddr;
    use tokio::net::TcpListener;

    struct LoopbackResolver;

    #[async_trait]
    impl Resolver for LoopbackResolver {
        async fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<IpAddr>, NetworkError> {
            Ok(vec!["127.0.0.1".parse().unwrap()])
        }
    }

    async fn setup() -> (
        Arc<NetworkBroker>,
        std::path::PathBuf,
        u16,
        tokio::task::JoinHandle<io::Result<()>>,
        tokio::task::JoinHandle<()>,
    ) {
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = echo.local_addr().unwrap().port();
        let echo_task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = echo.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let (mut read, mut write) = stream.split();
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                });
            }
        });
        let id = format!("test-{}", uuid::Uuid::new_v4());
        let path = std::env::temp_dir().join(format!("{id}.sock"));
        let listener = UnixListener::bind(&path).unwrap();
        let broker = Arc::new(NetworkBroker::new(Arc::new(LoopbackResolver)));
        let mut policy = NetworkPolicy::new(
            "test",
            1,
            vec![NetworkRule {
                effect: Effect::Allow,
                host: HostPattern::Exact("localhost".into()),
                ports: vec![port],
                protocols: vec![ProxyProtocol::Https],
                reason: "test".into(),
            }],
            NetworkLimits::default(),
        );
        policy.allow_loopback = true;
        broker.install_policy(policy).unwrap();
        let mut config = ProxyConfig::new(unsafe { libc::geteuid() });
        config.allow_same_uid = true;
        let task = tokio::spawn(serve(listener, broker.clone(), config));
        (broker, path, port, task, echo_task)
    }

    fn connect_request(port: u16, token: &str) -> String {
        format!("CONNECT localhost:{port} HTTP/1.1\r\nProxy-Authorization: Bearer {token}\r\n\r\n")
    }

    #[tokio::test]
    async fn an_allowed_connect_tunnels_bytes() {
        let (broker, path, port, _serve, _echo) = setup().await;
        let (grant, token) = broker
            .issue_grant(
                sail_common::types::SubsystemId::Forge,
                uuid::Uuid::new_v4(),
                "test",
                60_000,
                chrono::Utc::now().timestamp_millis(),
            )
            .unwrap();
        let mut client = UnixStream::connect(&path).await.unwrap();
        client
            .write_all(connect_request(port, token.expose_secret()).as_bytes())
            .await
            .unwrap();
        let response = read_response(&mut client).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        client.write_all(b"ping").await.unwrap();
        let mut echoed = [0u8; 4];
        tokio::time::timeout(Duration::from_secs(5), client.read_exact(&mut echoed))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&echoed, b"ping");
        assert!(broker.revoke(grant.id));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn missing_or_unknown_credentials_are_rejected() {
        let (_broker, path, port, _serve, _echo) = setup().await;
        let mut client = UnixStream::connect(&path).await.unwrap();
        client
            .write_all(format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        assert!(read_response(&mut client).await.starts_with("HTTP/1.1 407"));

        let mut client = UnixStream::connect(&path).await.unwrap();
        let fake = format!("v1.{}", "a".repeat(64));
        client
            .write_all(connect_request(port, &fake).as_bytes())
            .await
            .unwrap();
        assert!(read_response(&mut client).await.starts_with("HTTP/1.1 407"));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn a_port_outside_the_policy_is_forbidden() {
        let (broker, path, port, _serve, _echo) = setup().await;
        let (_grant, token) = broker
            .issue_grant(
                sail_common::types::SubsystemId::Forge,
                uuid::Uuid::new_v4(),
                "test",
                60_000,
                chrono::Utc::now().timestamp_millis(),
            )
            .unwrap();
        let mut client = UnixStream::connect(&path).await.unwrap();
        client
            .write_all(connect_request(port.saturating_add(1), token.expose_secret()).as_bytes())
            .await
            .unwrap();
        assert!(read_response(&mut client).await.starts_with("HTTP/1.1 403"));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn revocation_terminates_an_active_tunnel_and_blocks_new_connections() {
        let (broker, path, port, _serve, _echo) = setup().await;
        let (grant, token) = broker
            .issue_grant(
                sail_common::types::SubsystemId::Forge,
                uuid::Uuid::new_v4(),
                "test",
                60_000,
                chrono::Utc::now().timestamp_millis(),
            )
            .unwrap();
        let mut active = UnixStream::connect(&path).await.unwrap();
        active
            .write_all(connect_request(port, token.expose_secret()).as_bytes())
            .await
            .unwrap();
        assert!(read_response(&mut active).await.starts_with("HTTP/1.1 200"));
        assert!(broker.revoke(grant.id));

        // The active tunnel must end, not keep forwarding bytes after revocation.
        let mut buffer = [0u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(3), active.read(&mut buffer)).await;
        assert!(
            matches!(read, Ok(Ok(0))),
            "active tunnel did not close: {read:?}"
        );

        // A new CONNECT with the revoked token is refused.
        let mut fresh = UnixStream::connect(&path).await.unwrap();
        fresh
            .write_all(connect_request(port, token.expose_secret()).as_bytes())
            .await
            .unwrap();
        assert!(read_response(&mut fresh).await.starts_with("HTTP/1.1 403"));
        let _ = std::fs::remove_file(path);
    }

    async fn read_response(stream: &mut UnixStream) -> String {
        let mut data = Vec::new();
        let mut buffer = [0u8; 256];
        loop {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            data.extend_from_slice(&buffer[..read]);
            if data.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&data).into_owned()
    }
}
