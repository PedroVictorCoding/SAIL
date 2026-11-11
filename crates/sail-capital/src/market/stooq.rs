//! Stooq daily price client and CSV parser (round 10, Antigravity's slice).
//!
//! Stooq provides historical daily bars via public CSV downloads. Fetching sends the
//! held symbols to the provider, so it is strictly opt-in: disabled unless the owner sets
//! `SAIL_PRICES_SOURCE=stooq`.
//!
//! A neutral User-Agent (`"SAIL-capital/1"`) is used. The owner's email or identity is
//! never sent.
use super::{
    Adjustment, MAX_BARS_PER_SERIES, MAX_PRICE_RESPONSE_BYTES, PRICE_MAX_REQUESTS_PER_SECOND,
    PriceBar, PriceSeries, PriceSource, PriceSourceError,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Base URL for Stooq requests.
const DEFAULT_STOOQ_BASE: &str = "https://stooq.com";

/// User-Agent header sent with every request: neutral, never identifying the owner.
const USER_AGENT: &str = "SAIL-capital/1";

/// Per-request timeout for price requests.
const REQUEST_TIMEOUT_SECS: u64 = 20;

/// Redirects followed before giving up.
const MAX_REDIRECTS: usize = 3;

/// The source's name for a broker symbol, or `None` when it can't be mapped.
/// Lowercases the broker symbol and maps XTB suffixes to Stooq's:
/// - `.US` -> `.us`
/// - `.DE` -> `.de`
/// - `.UK` -> `.uk`
/// - `.FR` -> `.fr`
/// - `.NL` -> `.nl`
/// - `.PT` -> `.pt`
/// - `.ES` -> `.es`
/// - `.IT` -> `.it`
/// - `.PL` -> `.pl`
///
/// Any other suffix, or a symbol with no suffix (CFDs like US500, GOLD), is `None`.
///
/// Note: This mapping is per Stooq's naming conventions and unverified until the live check.
pub fn stooq_symbol(broker_symbol: &str) -> Option<String> {
    let raw = broker_symbol.trim();
    if raw.is_empty() {
        return None;
    }
    let (prefix, suffix) = raw.rsplit_once('.')?;
    if prefix.is_empty() {
        return None;
    }
    let stooq_suffix = match suffix.to_ascii_uppercase().as_str() {
        "US" => "us",
        "DE" => "de",
        "UK" => "uk",
        "FR" => "fr",
        "NL" => "nl",
        "PT" => "pt",
        "ES" => "es",
        "IT" => "it",
        "PL" => "pl",
        _ => return None,
    };
    Some(format!("{}.{}", prefix.to_ascii_lowercase(), stooq_suffix))
}

/// Reads Stooq's daily CSV (`Date,Open,High,Low,Close,Volume`).
pub fn parse_stooq_csv(
    body: &[u8],
    symbol: &str,
    source_symbol: &str,
) -> Result<PriceSeries, PriceSourceError> {
    if body.is_empty() {
        return Err(PriceSourceError::Malformed("empty response body".into()));
    }

    if let Ok(text) = std::str::from_utf8(body) {
        let trimmed = text.trim();
        if trimmed.eq_ignore_ascii_case("no data")
            || trimmed.to_ascii_lowercase().starts_with("no data")
        {
            return Err(PriceSourceError::UnknownSymbol(symbol.to_string()));
        }
    }

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(body);

    let headers = reader
        .headers()
        .map_err(|e| PriceSourceError::Malformed(format!("could not read CSV headers: {e}")))?
        .clone();

    let mut date_col = None;
    let mut open_col = None;
    let mut high_col = None;
    let mut low_col = None;
    let mut close_col = None;
    let mut volume_col = None;

    for (idx, header) in headers.iter().enumerate() {
        match header.trim().to_ascii_lowercase().as_str() {
            "date" => date_col = Some(idx),
            "open" => open_col = Some(idx),
            "high" => high_col = Some(idx),
            "low" => low_col = Some(idx),
            "close" => close_col = Some(idx),
            "volume" | "vol" => volume_col = Some(idx),
            _ => {}
        }
    }

    let date_idx = date_col
        .ok_or_else(|| PriceSourceError::Malformed("missing Date column in Stooq CSV".into()))?;
    let close_idx = close_col
        .ok_or_else(|| PriceSourceError::Malformed("missing Close column in Stooq CSV".into()))?;

    let mut by_session: BTreeMap<NaiveDate, PriceBar> = BTreeMap::new();
    let mut skipped = 0;

    for result in reader.records() {
        let record = match result {
            Ok(r) => r,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let Some(date_str) = record.get(date_idx) else {
            skipped += 1;
            continue;
        };
        let Ok(session) = NaiveDate::parse_from_str(date_str.trim(), "%Y-%m-%d") else {
            skipped += 1;
            continue;
        };

        let Some(close_str) = record.get(close_idx) else {
            skipped += 1;
            continue;
        };
        let Some(close) = crate::contracts::parse_decimal_text(close_str) else {
            skipped += 1;
            continue;
        };
        if close <= Decimal::ZERO {
            skipped += 1;
            continue;
        }

        let open = open_col
            .and_then(|i| record.get(i))
            .and_then(crate::contracts::parse_decimal_text);
        let high = high_col
            .and_then(|i| record.get(i))
            .and_then(crate::contracts::parse_decimal_text);
        let low = low_col
            .and_then(|i| record.get(i))
            .and_then(crate::contracts::parse_decimal_text);
        let volume = volume_col
            .and_then(|i| record.get(i))
            .and_then(crate::contracts::parse_decimal_text);

        let bar = PriceBar {
            session,
            open,
            high,
            low,
            close,
            volume,
        };

        if by_session.insert(session, bar).is_some() {
            tracing::warn!(%session, "duplicate session in Stooq CSV; keeping last occurrence");
        }
    }

    if skipped > 0 {
        tracing::warn!(
            skipped,
            "skipped malformed or non-positive close rows in Stooq CSV"
        );
    }

    if by_session.is_empty() {
        return Err(PriceSourceError::Malformed(
            "no valid price bars in Stooq CSV".into(),
        ));
    }

    let mut bars: Vec<PriceBar> = by_session.into_values().collect();
    if bars.len() > MAX_BARS_PER_SERIES {
        let excess = bars.len() - MAX_BARS_PER_SERIES;
        bars.drain(0..excess);
    }

    Ok(PriceSeries {
        source_id: "stooq".to_string(),
        symbol: symbol.to_string(),
        source_symbol: source_symbol.to_string(),
        currency: None,
        adjustment: Adjustment::Unknown,
        bars,
        payload_sha256: crate::sha256_hex(body),
    })
}

/// Short network error messages with no URL.
fn network_error(error: reqwest::Error) -> PriceSourceError {
    let message = if error.is_timeout() {
        "the request timed out"
    } else if error.is_connect() {
        "the connection failed"
    } else {
        "the request failed"
    };
    PriceSourceError::Network(message.to_string())
}

/// The Stooq daily prices client.
#[derive(Clone)]
pub struct StooqClient {
    enabled: bool,
    base: String,
    http: Option<reqwest::Client>,
    gate: Arc<Mutex<Option<Instant>>>,
}

impl StooqClient {
    /// Reads `SAIL_PRICES_SOURCE`. Only `"stooq"` enables it; otherwise every call
    /// returns `NotConfigured` before making any request.
    pub fn from_env() -> Self {
        let enabled = std::env::var("SAIL_PRICES_SOURCE")
            .ok()
            .map(|v| v.trim().eq_ignore_ascii_case("stooq"))
            .unwrap_or(false);
        Self::build(enabled, DEFAULT_STOOQ_BASE)
    }

    /// Builds a client with an explicit base URL for testing. Tests are always enabled.
    pub fn with_base(base: impl Into<String>) -> Self {
        Self::build(true, &base.into())
    }

    /// True when configured and enabled.
    pub fn is_configured(&self) -> bool {
        self.enabled
    }

    fn build(enabled: bool, base: &str) -> Self {
        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
            .user_agent(USER_AGENT);

        Self {
            enabled,
            base: base.trim_end_matches('/').to_string(),
            http: builder.build().ok(),
            gate: Arc::new(Mutex::new(None)),
        }
    }

    async fn wait_for_slot(&self) {
        let per_second = u64::from(PRICE_MAX_REQUESTS_PER_SECOND.max(1));
        let interval = Duration::from_millis(1_000 / per_second);
        let mut last = self.gate.lock().await;
        if let Some(previous) = *last {
            let elapsed = previous.elapsed();
            if elapsed < interval {
                tokio::time::sleep(interval - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }
}

#[async_trait::async_trait]
impl PriceSource for StooqClient {
    fn source_id(&self) -> &str {
        "stooq"
    }

    async fn daily(&self, symbol: &str) -> Result<PriceSeries, PriceSourceError> {
        if !self.enabled {
            return Err(PriceSourceError::NotConfigured);
        }

        let stooq_sym = stooq_symbol(symbol)
            .ok_or_else(|| PriceSourceError::UnknownSymbol(symbol.to_string()))?;

        let Some(http) = &self.http else {
            return Err(PriceSourceError::Network(
                "the HTTP client could not be created".to_string(),
            ));
        };

        self.wait_for_slot().await;

        let url = format!("{}/q/d/l/?s={}&i=d", self.base, stooq_sym);
        let mut response = http.get(&url).send().await.map_err(network_error)?;

        let status = response.status();
        if matches!(status.as_u16(), 403 | 429) {
            return Err(PriceSourceError::Refused(status.as_u16()));
        }
        if !status.is_success() {
            return Err(PriceSourceError::Http(status.as_u16()));
        }

        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(network_error)? {
            if body.len().saturating_add(chunk.len()) > MAX_PRICE_RESPONSE_BYTES {
                return Err(PriceSourceError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }

        parse_stooq_csv(&body, symbol, &stooq_sym)
    }
}
