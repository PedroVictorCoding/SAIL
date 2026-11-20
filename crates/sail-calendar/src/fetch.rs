//! K4: feed URL normalization, address guard and HTTP fetching.

use std::net::IpAddr;

use async_trait::async_trait;
use url::Url;

use crate::contracts::CalendarError;

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

/// Normalize webcal:// to https:// and allow only https. Stub until K4.
pub fn normalize_feed_url(raw: &str) -> Result<Url, CalendarError> {
    Err(CalendarError::Invalid(format!(
        "feed URL `{raw}` is not valid"
    )))
}

/// Scheme, host and `/…`; never the secret path. Stub until K4.
pub fn url_hint(url: &Url) -> String {
    let _ = url;
    String::new()
}

/// Refuse non-public addresses. Stub until K4.
pub fn check_addr(addr: IpAddr) -> Result<(), CalendarError> {
    let _ = addr;
    Err(CalendarError::BlockedAddress)
}
