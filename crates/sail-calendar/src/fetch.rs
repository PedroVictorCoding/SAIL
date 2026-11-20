//! K4: feed URL safety, manual redirects, conditional GET and size limits.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{
    CONTENT_TYPE, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, LOCATION,
};
use url::Url;

use crate::contracts::CalendarError;

/// Feed body cap (invariant 4).
pub(crate) const MAX_FEED_BYTES: usize = 5 * 1024 * 1024;
/// Fetch timeout (invariant 4).
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// Redirects are followed manually, at most three (invariant 4).
const MAX_REDIRECTS: usize = 3;

#[async_trait]
pub trait FeedFetcher: Send + Sync {
    async fn fetch(
        &self,
        url: &Url,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<FetchOutcome, CalendarError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    NotModified,
    Body {
        text: String,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

/// HTTP fetcher. Redirects, DNS addresses and body size are checked by hand.
pub struct ReqwestFetcher {
    client: reqwest::Client,
}

impl ReqwestFetcher {
    pub fn new() -> Result<Self, CalendarError> {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| CalendarError::FetchFailed(error.to_string()))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl FeedFetcher for ReqwestFetcher {
    async fn fetch(
        &self,
        url: &Url,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<FetchOutcome, CalendarError> {
        let mut current = url.clone();
        for _ in 0..=MAX_REDIRECTS {
            check_url(&current).await?;
            let mut request = self.client.get(current.clone());
            if let Some(etag) = etag {
                request = request.header(IF_NONE_MATCH, etag);
            }
            if let Some(last_modified) = last_modified {
                request = request.header(IF_MODIFIED_SINCE, last_modified);
            }
            let response = request
                .send()
                .await
                .map_err(|_| CalendarError::FetchFailed("the feed did not respond".into()))?;
            if response.status() == reqwest::StatusCode::NOT_MODIFIED {
                return Ok(FetchOutcome::NotModified);
            }
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| CalendarError::FetchFailed("a redirect had no target".into()))?;
                current = current.join(location).map_err(|_| {
                    CalendarError::FetchFailed("a redirect target was invalid".into())
                })?;
                continue;
            }
            if !response.status().is_success() {
                return Err(CalendarError::FetchFailed(format!(
                    "the feed returned HTTP {}",
                    response.status().as_u16()
                )));
            }
            if let Some(length) = response.content_length()
                && length as usize > MAX_FEED_BYTES
            {
                return Err(CalendarError::TooLarge);
            }
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            let etag = response
                .headers()
                .get(ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let last_modified = response
                .headers()
                .get(LAST_MODIFIED)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk
                    .map_err(|_| CalendarError::FetchFailed("the feed stopped early".into()))?;
                if body.len() + chunk.len() > MAX_FEED_BYTES {
                    return Err(CalendarError::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            let text = String::from_utf8_lossy(&body).to_string();
            let looks_like_calendar = text.trim_start().starts_with("BEGIN:VCALENDAR");
            if !content_type.contains("text/calendar") && !looks_like_calendar {
                return Err(CalendarError::NotCalendar);
            }
            return Ok(FetchOutcome::Body {
                text,
                etag,
                last_modified,
            });
        }
        Err(CalendarError::FetchFailed("too many redirects".into()))
    }
}

/// Normalize `webcal://` to `https://`; refuse everything except https.
pub fn normalize_feed_url(raw: &str) -> Result<Url, CalendarError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CalendarError::Invalid("the feed URL is empty".into()));
    }
    let rewritten = trimmed
        .strip_prefix("webcal://")
        .map(|rest| format!("https://{rest}"))
        .unwrap_or_else(|| trimmed.to_string());
    let url = Url::parse(&rewritten)
        .map_err(|_| CalendarError::Invalid("the feed URL is not valid".into()))?;
    if url.scheme() != "https" {
        return Err(CalendarError::Invalid(
            "only https calendar addresses are allowed".into(),
        ));
    }
    if url.host_str().is_none() {
        return Err(CalendarError::Invalid("the feed URL has no host".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(CalendarError::Invalid(
            "the feed URL must not contain a username or password".into(),
        ));
    }
    Ok(url)
}

/// Scheme, host and `/…`; never the secret path or query.
pub fn url_hint(url: &Url) -> String {
    let host = url.host_str().unwrap_or("");
    format!("{}://{host}/…", url.scheme())
}

/// Refuse addresses that are not public unicast.
pub fn check_addr(addr: IpAddr) -> Result<(), CalendarError> {
    match addr {
        IpAddr::V4(addr) => check_v4(addr),
        IpAddr::V6(addr) => check_v6(addr),
    }
}

fn check_v4(addr: Ipv4Addr) -> Result<(), CalendarError> {
    let octets = addr.octets();
    let cgnat = octets[0] == 100 && (64..=127).contains(&octets[1]);
    let reserved = octets[0] == 0;
    if addr.is_loopback()
        || addr.is_private()
        || addr.is_link_local()
        || addr.is_multicast()
        || addr.is_unspecified()
        || addr.is_broadcast()
        || cgnat
        || reserved
    {
        return Err(CalendarError::BlockedAddress);
    }
    Ok(())
}

fn check_v6(addr: Ipv6Addr) -> Result<(), CalendarError> {
    if let Some(v4) = addr.to_ipv4_mapped() {
        return check_v4(v4);
    }
    let segments = addr.segments();
    let unique_local = segments[0] & 0xfe00 == 0xfc00;
    let link_local = segments[0] & 0xffc0 == 0xfe80;
    if addr.is_loopback()
        || addr.is_unspecified()
        || addr.is_multicast()
        || unique_local
        || link_local
    {
        return Err(CalendarError::BlockedAddress);
    }
    Ok(())
}

async fn check_url(url: &Url) -> Result<(), CalendarError> {
    if url.scheme() != "https" {
        return Err(CalendarError::Invalid(
            "only https calendar addresses are allowed".into(),
        ));
    }
    let Some(host) = url.host_str() else {
        return Err(CalendarError::Invalid("the feed URL has no host".into()));
    };
    let port = url.port_or_known_default().unwrap_or(443);
    let lookup = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| CalendarError::FetchFailed("the calendar host did not resolve".into()))?;
    let mut seen = false;
    for address in lookup {
        seen = true;
        check_addr(address.ip())?;
    }
    if !seen {
        return Err(CalendarError::FetchFailed(
            "the calendar host did not resolve".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizer_rewrites_webcal_and_refuses_http() {
        assert_eq!(
            normalize_feed_url("webcal://calendar.google.com/x")
                .expect("webcal")
                .as_str(),
            "https://calendar.google.com/x"
        );
        assert!(matches!(
            normalize_feed_url("http://example.test/x"),
            Err(CalendarError::Invalid(_))
        ));
        assert!(matches!(
            normalize_feed_url("ftp://example.test/x"),
            Err(CalendarError::Invalid(_))
        ));
        assert!(normalize_feed_url("not a url").is_err());
    }

    #[test]
    fn hints_never_include_the_path_or_query() {
        let url = Url::parse("https://calendar.google.com/secret/address?token=1").expect("url");
        assert_eq!(url_hint(&url), "https://calendar.google.com/…");
    }

    #[test]
    fn private_and_special_addresses_are_blocked() {
        for addr in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.5.4",
            "192.168.1.10",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.254",
            "0.0.0.0",
            "224.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::",
            "ff02::1",
            "::ffff:127.0.0.1",
        ] {
            let ip: IpAddr = addr.parse().expect("ip");
            assert_eq!(check_addr(ip), Err(CalendarError::BlockedAddress), "{addr}");
        }
        for addr in ["8.8.8.8", "1.1.1.1", "2001:4860:4860::8888"] {
            let ip: IpAddr = addr.parse().expect("ip");
            assert_eq!(check_addr(ip), Ok(()), "{addr}");
        }
    }
}
