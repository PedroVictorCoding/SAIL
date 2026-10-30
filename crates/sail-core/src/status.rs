//! Read the running daemon's status rather than inventing zero metrics in a new service.
pub async fn read() -> Result<sail_mind::protocol::MindStatus, Box<dyn std::error::Error>> {
    let port = std::env::var("SAIL_VOICE_BIND")
        .ok()
        .and_then(|v| v.parse::<std::net::SocketAddr>().ok())
        .map(|a| a.port())
        .unwrap_or(8088);
    let tls = std::env::var("SAIL_TLS_CERT").ok();
    let url = std::env::var("SAIL_MIND_STATUS_URL").unwrap_or_else(|_| {
        format!(
            "{}://127.0.0.1:{port}/api/mind/status",
            if tls.is_some() { "https" } else { "http" }
        )
    });
    let url = reqwest::Url::parse(&url)?;
    let local = url.host_str().is_some_and(|h| {
        h == "localhost"
            || h.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if !local
        || !matches!(url.scheme(), "http" | "https")
        || url.path() != "/api/mind/status"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Mind status must use the local daemon's /api/mind/status endpoint".into());
    }
    let mut client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(15));
    if let Some(cert) = tls {
        client =
            client.add_root_certificate(reqwest::Certificate::from_pem(&std::fs::read(cert)?)?);
    }
    if url.host_str() == Some("localhost") {
        client = client.resolve(
            "localhost",
            std::net::SocketAddr::from(([127, 0, 0, 1], url.port_or_known_default().unwrap())),
        );
    }
    let status = client
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json::<sail_mind::protocol::MindStatus>()
        .await?;
    if status.version != sail_mind::protocol::VERSION {
        return Err("Unsupported daemon status protocol version".into());
    }
    Ok(status)
}
