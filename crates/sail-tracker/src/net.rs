//! Outbound HTTP for public feeds: one client, short timeouts, capped bodies, and a guard
//! that keeps owner-supplied feed URLs off loopback and private networks.
use futures_util::StreamExt;
use std::net::IpAddr;
use std::time::Duration;

pub const USER_AGENT: &str = concat!(
    "SAIL-tracker/",
    env!("CARGO_PKG_VERSION"),
    " (local-first personal dashboard)"
);

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("request failed: {0}")]
    Http(String),
    #[error("HTTP {0}")]
    Status(u16),
    #[error("response larger than {0} bytes")]
    TooLarge(usize),
    #[error("blocked address: {0}")]
    Blocked(String),
    #[error("unreadable response: {0}")]
    Parse(String),
}

impl From<reqwest::Error> for FetchError {
    fn from(e: reqwest::Error) -> Self {
        FetchError::Http(e.without_url().to_string())
    }
}

#[derive(Clone)]
pub struct Net {
    /// For the fixed, well-known API hosts.
    api: reqwest::Client,
    /// For owner-supplied feed URLs: redirects are followed manually so each hop is checked.
    guarded: reqwest::Client,
}

impl Default for Net {
    fn default() -> Self {
        Self::new()
    }
}

impl Net {
    pub fn new() -> Self {
        let build = |redirect| {
            reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(Duration::from_secs(15))
                .connect_timeout(Duration::from_secs(6))
                .redirect(redirect)
                .build()
                .expect("tracker HTTP client")
        };
        Self {
            api: build(reqwest::redirect::Policy::limited(3)),
            guarded: build(reqwest::redirect::Policy::none()),
        }
    }

    pub async fn get_bytes(&self, url: &str, max: usize) -> Result<Vec<u8>, FetchError> {
        let response = self.api.get(url).send().await?;
        read_capped(response, max).await
    }

    pub async fn get_json(&self, url: &str, max: usize) -> Result<serde_json::Value, FetchError> {
        let bytes = self.get_bytes(url, max).await?;
        serde_json::from_slice(&bytes).map_err(|e| FetchError::Parse(e.to_string()))
    }

    /// Fetches an owner-supplied URL, refusing hosts that resolve to non-public addresses.
    pub async fn get_guarded(&self, url: &str, max: usize) -> Result<Vec<u8>, FetchError> {
        let mut current = url::Url::parse(url).map_err(|e| FetchError::Http(e.to_string()))?;
        for _ in 0..4 {
            guard(&current).await?;
            let response = self.guarded.get(current.clone()).send().await?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or(FetchError::Status(response.status().as_u16()))?;
                current = current
                    .join(location)
                    .map_err(|e| FetchError::Http(e.to_string()))?;
                continue;
            }
            return read_capped(response, max).await;
        }
        Err(FetchError::Http("too many redirects".into()))
    }
}

async fn read_capped(response: reqwest::Response, max: usize) -> Result<Vec<u8>, FetchError> {
    if !response.status().is_success() {
        return Err(FetchError::Status(response.status().as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|len| len as usize > max)
    {
        return Err(FetchError::TooLarge(max));
    }
    let mut out = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if out.len() + chunk.len() > max {
            return Err(FetchError::TooLarge(max));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

async fn guard(url: &url::Url) -> Result<(), FetchError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FetchError::Blocked(url.scheme().into()));
    }
    let host = url
        .host_str()
        .ok_or_else(|| FetchError::Blocked("missing host".into()))?;
    let port = url.port_or_known_default().unwrap_or(443);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| FetchError::Http(format!("cannot resolve {host}: {e}")))?;
    let mut any = false;
    for addr in addrs {
        any = true;
        if !is_public(addr.ip()) {
            return Err(FetchError::Blocked(host.into()));
        }
    }
    if !any {
        return Err(FetchError::Http(format!("cannot resolve {host}")));
    }
    Ok(())
}

pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || a == 0
                || (a == 100 && (64..128).contains(&b))
                || (a == 198 && (b == 18 || b == 19))
                || a >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_addresses() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.1",
            "172.16.0.9",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in ["1.1.1.1", "151.101.0.81", "2606:4700::1111"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[tokio::test]
    async fn guarded_fetch_refuses_loopback() {
        let err = Net::new()
            .get_guarded("http://127.0.0.1:9/feed", 1000)
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Blocked(_)), "{err}");
    }
}
