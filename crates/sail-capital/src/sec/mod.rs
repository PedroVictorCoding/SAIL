//! SEC EDGAR client and parsers (Kilo's slice, round 8).
//!
//! Reads only SEC's public JSON. It never sends portfolio data, and the only identifying
//! thing it sends is the owner-declared User-Agent that SEC's fair-access policy requires.
//! The parsers are pure functions, so they can be tested on fixtures without the network.
//!
//! Two choices are visible here:
//! - EDGAR's `acceptanceDateTime` is real UTC: its `Z` means what it says. Checked live on
//!   2026-11-11: the submissions JSON gave `2026-11-06T02:54:06.000Z` for a filing whose
//!   sec.gov index page says "Accepted 2026-11-05 21:54:06" (Eastern, UTC-5).
//! - [`ACCEPT_ENCODING`] is `identity`: the workspace builds `reqwest` without its gzip
//!   feature, so asking for a compressed body would leave it undecoded.
use crate::evidence::*;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// What the client advertises it can decode. The workspace builds `reqwest` without the
/// gzip feature, so a compressed response could not be read; `identity` keeps SEC's body
/// plain. See the round-8 handoff log for the one-line manifest change if gzip is wanted.
const ACCEPT_ENCODING: &str = "identity";

/// Base URL for the ticker map. Only the test constructor overrides it.
const DEFAULT_WWW_BASE: &str = "https://www.sec.gov";

/// Base URL for submissions JSON. Only the test constructor overrides it.
const DEFAULT_DATA_BASE: &str = "https://data.sec.gov";

/// Per-request timeout. SEC answers large submissions files slowly but not this slowly.
const REQUEST_TIMEOUT_SECS: u64 = 20;

/// Redirects followed before giving up, so a redirect loop can't hang a refresh.
const MAX_REDIRECTS: usize = 3;

/// Hard cap on ticker-map rows read from one body, beyond the body-size cap. The real map
/// is roughly 12k rows; this only stops a misbehaving or hostile server.
const MAX_TICKER_ROWS: usize = 100_000;

/// Hard cap on `filings.recent` rows read from one body. Recent blocks hold hundreds of
/// rows; this keeps a hostile body from making the parser slow.
const MAX_RECENT_ROWS: usize = 10_000;

/// Most 8-K item numbers kept for one filing. A real filing lists a handful.
const MAX_ITEMS_PER_FILING: usize = 64;

/// Parses `company_tickers.json`.
///
/// The body is an object of numbered rows; every value must be an object, or the body isn't
/// this file at all ([`SourceError::Malformed`]). A single row with an unusable CIK, an
/// empty ticker or a missing field is skipped and counted, never guessed. Duplicate tickers
/// stay in the index: resolving them is the matcher's job.
pub fn parse_ticker_index(
    body: &[u8],
    fetched_at: DateTime<Utc>,
) -> Result<TickerIndex, SourceError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|error| SourceError::Malformed(format!("the ticker map isn't JSON: {error}")))?;
    let object = value.as_object().ok_or_else(|| {
        SourceError::Malformed("the ticker map isn't a JSON object of numbered rows".to_string())
    })?;

    let mut entries: Vec<TickerEntry> = Vec::new();
    let mut skipped: usize = 0;
    let mut truncated: usize = 0;
    for (read, row) in object.values().enumerate() {
        if read >= MAX_TICKER_ROWS {
            truncated = object.len().saturating_sub(read);
            break;
        }
        let Some(row) = row.as_object() else {
            return Err(SourceError::Malformed(
                "the ticker map has a row that isn't an object".to_string(),
            ));
        };
        let cik = row.get("cik_str").and_then(json_u64).and_then(Cik::new);
        let ticker = row
            .get("ticker")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_uppercase();
        let title = row
            .get("title")
            .and_then(Value::as_str)
            .map(cap_text)
            .unwrap_or_default();
        let Some(cik) = cik else {
            skipped += 1;
            continue;
        };
        if ticker.is_empty() {
            skipped += 1;
            continue;
        }
        entries.push(TickerEntry {
            cik,
            ticker: cap_text(&ticker),
            title,
        });
    }

    entries.sort_by(|left, right| {
        left.ticker
            .cmp(&right.ticker)
            .then_with(|| left.cik.cmp(&right.cik))
    });
    if skipped > 0 {
        tracing::warn!(
            skipped,
            rows = object.len(),
            "skipped unusable rows in the SEC ticker map"
        );
    }
    if truncated > 0 {
        tracing::warn!(
            truncated,
            rows = object.len(),
            "stopped at the SEC ticker-map row cap"
        );
    }
    Ok(TickerIndex {
        fetched_at,
        payload_sha256: crate::sha256_hex(body),
        entries,
    })
}

/// Parses `CIK##########.json` for `cik`.
///
/// Only the `filings.recent` block is read; the older `files` pages are ignored this round.
/// Arrays are parallel, so index `i` is read only when it exists in the three required
/// arrays; a length mismatch is logged and the common prefix is used. A row that can't be
/// validated is skipped and counted. The result keeps the newest
/// [`MAX_FILINGS_PER_ISSUER`] filings by `(published_at desc, accession)`.
pub fn parse_submissions(body: &[u8], cik: Cik) -> Result<IssuerFilings, SourceError> {
    let value: Value = serde_json::from_slice(body).map_err(|error| {
        SourceError::Malformed(format!("the submissions file isn't JSON: {error}"))
    })?;
    let root = value.as_object().ok_or_else(|| {
        SourceError::Malformed("the submissions file isn't a JSON object".to_string())
    })?;

    match root.get("cik").and_then(json_u64).and_then(Cik::new) {
        Some(body_cik) if body_cik == cik => {}
        Some(body_cik) => {
            return Err(SourceError::Malformed(format!(
                "the submissions file is for CIK {}, not {}",
                body_cik.padded(),
                cik.padded()
            )));
        }
        None => {
            return Err(SourceError::Malformed(
                "the submissions file has no usable CIK".to_string(),
            ));
        }
    }

    let name = root
        .get("name")
        .and_then(Value::as_str)
        .map(cap_text)
        .unwrap_or_default();
    let tickers: Vec<String> = root
        .get("tickers")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(|ticker| cap_text(&ticker.trim().to_ascii_uppercase()))
                .filter(|ticker| !ticker.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let recent = root
        .get("filings")
        .and_then(Value::as_object)
        .and_then(|filings| filings.get("recent"))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            SourceError::Malformed("the submissions file has no filings.recent block".to_string())
        })?;
    let accessions = required_array(recent, "accessionNumber")?;
    let filing_dates = required_array(recent, "filingDate")?;
    let forms = required_array(recent, "form")?;

    if accessions.len() != filing_dates.len() || accessions.len() != forms.len() {
        tracing::warn!(
            accessions = accessions.len(),
            filing_dates = filing_dates.len(),
            forms = forms.len(),
            "SEC submissions arrays have different lengths; reading the common prefix"
        );
    }
    let common = accessions
        .len()
        .min(filing_dates.len())
        .min(forms.len())
        .min(MAX_RECENT_ROWS);
    if common < accessions.len() {
        tracing::warn!(
            common,
            available = accessions.len(),
            "SEC submissions recent block was truncated"
        );
    }

    let mut filings: Vec<Filing> = Vec::new();
    let mut skipped: usize = 0;
    for index in 0..common {
        let Some(accession) = accessions[index]
            .as_str()
            .map(str::trim)
            .filter(|text| accession_is_valid(text))
        else {
            skipped += 1;
            continue;
        };
        let Some(filing_date) = filing_dates[index].as_str().and_then(parse_date) else {
            skipped += 1;
            continue;
        };
        let Some(form) = forms[index]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        else {
            skipped += 1;
            continue;
        };

        let report_date = value_at(recent, "reportDate", index)
            .and_then(Value::as_str)
            .and_then(parse_date);
        let published = match value_at(recent, "acceptanceDateTime", index)
            .and_then(Value::as_str)
            .and_then(parse_acceptance)
        {
            Some(at) => Some((at, PublishedBasis::Acceptance)),
            None => {
                end_of_filing_day(filing_date).map(|at| (at, PublishedBasis::FilingDateEndOfDay))
            }
        };
        let Some((published_at, published_basis)) = published else {
            skipped += 1;
            continue;
        };

        let items = value_at(recent, "items", index)
            .and_then(Value::as_str)
            .map(parse_items)
            .unwrap_or_default();
        let primary_document = value_at(recent, "primaryDocument", index)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| document_is_valid(name))
            .map(str::to_string);
        let description = value_at(recent, "primaryDocDescription", index)
            .and_then(Value::as_str)
            .map(cap_text)
            .filter(|text| !text.is_empty());
        let url = filing_url(cik, accession, primary_document.as_deref());

        filings.push(Filing {
            accession: accession.to_string(),
            cik,
            form: cap_text(form),
            filing_date,
            published_at,
            published_basis,
            report_date,
            items,
            primary_document,
            description,
            url,
        });
    }

    filings.sort_by(|left, right| {
        right
            .published_at
            .cmp(&left.published_at)
            .then_with(|| left.accession.cmp(&right.accession))
    });
    filings.truncate(MAX_FILINGS_PER_ISSUER);
    if skipped > 0 {
        tracing::warn!(skipped, read = common, "skipped unusable SEC filing rows");
    }

    Ok(IssuerFilings {
        cik,
        name,
        tickers,
        filings,
        payload_sha256: crate::sha256_hex(body),
    })
}

/// Reads a required parallel array, or reports the body as malformed.
fn required_array<'a>(
    recent: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Result<&'a Vec<Value>, SourceError> {
    recent
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| SourceError::Malformed(format!("the submissions file has no {field} array")))
}

/// A field of one parallel array, when the array exists and has that index.
fn value_at<'a>(
    recent: &'a serde_json::Map<String, Value>,
    field: &str,
    index: usize,
) -> Option<&'a Value> {
    recent
        .get(field)
        .and_then(Value::as_array)
        .and_then(|rows| rows.get(index))
}

/// A CIK as SEC writes it: a number, or digits in a string.
fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

/// Cap untrusted SEC text, dropping control characters. Trims first so surrounding space
/// never consumes the budget.
fn cap_text(text: &str) -> String {
    text.trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_SEC_TEXT_CHARS)
        .collect()
}

/// `^\d{10}-\d{2}-\d{6}$`, checked without pulling in a regex dependency.
fn accession_is_valid(accession: &str) -> bool {
    let bytes = accession.as_bytes();
    bytes.len() == 20
        && bytes[..10].iter().all(u8::is_ascii_digit)
        && bytes[10] == b'-'
        && bytes[11..13].iter().all(u8::is_ascii_digit)
        && bytes[13] == b'-'
        && bytes[14..].iter().all(u8::is_ascii_digit)
}

/// `^[A-Za-z0-9._-]{1,128}$`, so a hostile document name can't escape the filing path or
/// become a script URL.
fn document_is_valid(document: &str) -> bool {
    !document.is_empty()
        && document.len() <= 128
        && document
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Split an 8-K item list and keep only `^\d\.\d{2}$` entries.
fn parse_items(items: &str) -> Vec<String> {
    items
        .split(',')
        .map(str::trim)
        .filter(|item| item_is_valid(item))
        .take(MAX_ITEMS_PER_FILING)
        .map(str::to_string)
        .collect()
}

/// `^\d\.\d{2}$`, e.g. `"2.02"`.
fn item_is_valid(item: &str) -> bool {
    let bytes = item.as_bytes();
    bytes.len() == 4
        && bytes[0].is_ascii_digit()
        && bytes[1] == b'.'
        && bytes[2].is_ascii_digit()
        && bytes[3].is_ascii_digit()
}

/// Parse a `YYYY-MM-DD` date, rejecting empty or malformed text.
fn parse_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").ok()
}

/// Parse `acceptanceDateTime`, an RFC 3339 UTC time such as `2026-11-06T02:54:06.000Z`.
/// Anything else is `None`, and the caller falls back to the end of the filing day in New
/// York, which is never earlier than the real publication.
fn parse_acceptance(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|moment| moment.with_timezone(&Utc))
}

/// The latest instant of `date` in New York, used when only `filingDate` is known. Never
/// earlier than the real publication, so point-in-time replay can't leak it.
fn end_of_filing_day(date: NaiveDate) -> Option<DateTime<Utc>> {
    let naive = date.and_hms_opt(23, 59, 59)?;
    Some(
        New_York
            .from_local_datetime(&naive)
            .earliest()
            .map(|moment| moment.with_timezone(&Utc))
            .unwrap_or_else(|| Utc.from_utc_datetime(&naive)),
    )
}

/// Build the sec.gov link from the CIK, accession and validated document name only. Free
/// text never reaches the URL.
fn filing_url(cik: Cik, accession: &str, primary_document: Option<&str>) -> String {
    let accession_digits = accession.replace('-', "");
    let base = format!(
        "https://www.sec.gov/Archives/edgar/data/{}/{}",
        cik.0, accession_digits
    );
    match primary_document {
        Some(document) => format!("{base}/{document}"),
        None => format!("{base}/"),
    }
}

/// The real EDGAR client: rate-limited, size-capped, and needs the User-Agent.
#[derive(Clone)]
pub struct EdgarClient {
    /// The owner-declared User-Agent, if configured.
    user_agent: Option<String>,
    /// The HTTP client, absent if the TLS backend couldn't be initialised.
    http: Option<reqwest::Client>,
    /// Base that serves `/files/company_tickers.json`.
    www_base: String,
    /// Base that serves `/submissions/CIK##########.json`.
    data_base: String,
    /// Last request instant, shared with clones so the rate limit is global.
    gate: Arc<Mutex<Option<Instant>>>,
}

impl EdgarClient {
    /// Builds a client from `$SAIL_SEC_USER_AGENT`. Without it, every call returns
    /// [`SourceError::NotConfigured`] and no request is made.
    pub fn from_env() -> Self {
        let user_agent = std::env::var("SAIL_SEC_USER_AGENT")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        Self::build(user_agent, DEFAULT_WWW_BASE, DEFAULT_DATA_BASE)
    }

    /// Builds a client against explicit base URLs, for tests. An empty `user_agent` means
    /// unconfigured, so the client behaves exactly like a missing environment variable.
    pub fn with_base(
        user_agent: impl Into<String>,
        www_base: impl Into<String>,
        data_base: impl Into<String>,
    ) -> Self {
        let user_agent = user_agent.into();
        let user_agent = {
            let trimmed = user_agent.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        };
        Self::build(user_agent, &www_base.into(), &data_base.into())
    }

    /// True when a User-Agent is set.
    pub fn is_configured(&self) -> bool {
        self.user_agent.is_some()
    }

    /// Create the shared pieces. A TLS/client build failure is carried as `None` instead of
    /// panicking; the next request reports it as a network error.
    fn build(user_agent: Option<String>, www_base: &str, data_base: &str) -> Self {
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS));
        if let Some(user_agent) = &user_agent {
            builder = builder.user_agent(user_agent);
        }
        Self {
            user_agent,
            http: builder.build().ok(),
            www_base: www_base.trim_end_matches('/').to_string(),
            data_base: data_base.trim_end_matches('/').to_string(),
            gate: Arc::new(Mutex::new(None)),
        }
    }

    /// Wait until the shared rate limit allows another request, then claim the slot. The
    /// mutex is held across the wait so concurrent clones queue instead of bursting.
    async fn wait_for_slot(&self) {
        let per_second = u64::from(SEC_MAX_REQUESTS_PER_SECOND.max(1));
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

    /// Fetch one SEC URL, capped and mapped to owner-safe errors.
    async fn get(&self, url: &str) -> Result<Vec<u8>, SourceError> {
        if self.user_agent.is_none() {
            return Err(SourceError::NotConfigured);
        }
        let Some(http) = &self.http else {
            return Err(SourceError::Network(
                "the HTTP client could not be created".to_string(),
            ));
        };
        self.wait_for_slot().await;
        let mut response = http
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::ACCEPT_ENCODING, ACCEPT_ENCODING)
            .send()
            .await
            .map_err(network_error)?;
        let status = response.status();
        if matches!(status.as_u16(), 403 | 429) {
            return Err(SourceError::Refused(status.as_u16()));
        }
        if !status.is_success() {
            return Err(SourceError::Http(status.as_u16()));
        }

        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(network_error)? {
            if body.len().saturating_add(chunk.len()) > MAX_SEC_RESPONSE_BYTES {
                return Err(SourceError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

/// Keep transport errors short and free of the request URL and the User-Agent.
fn network_error(error: reqwest::Error) -> SourceError {
    let message = if error.is_timeout() {
        "the request timed out"
    } else if error.is_connect() {
        "the connection failed"
    } else {
        "the request failed"
    };
    SourceError::Network(message.to_string())
}

#[async_trait::async_trait]
impl EdgarApi for EdgarClient {
    async fn ticker_index(&self) -> Result<TickerIndex, SourceError> {
        let url = format!("{}/files/company_tickers.json", self.www_base);
        let body = self.get(&url).await?;
        parse_ticker_index(&body, Utc::now())
    }

    async fn issuer_filings(&self, cik: Cik) -> Result<IssuerFilings, SourceError> {
        let url = format!("{}/submissions/CIK{}.json", self.data_base, cik.padded());
        let body = self.get(&url).await?;
        parse_submissions(&body, cik)
    }
}
