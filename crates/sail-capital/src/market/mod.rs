//! Market data (round 10, CAP-15): daily closing prices for owned instruments, as the
//! EPIC's `MarketObservation`. A price is an observation with a source, a session date and
//! the time SAIL first had it, never a prediction.
//!
//! The adapters (Antigravity's slice) are:
//! - an opt-in daily-bar client. Fetching sends the held symbols to the provider, so it
//!   is off unless the owner sets `SAIL_PRICES_SOURCE`.
//! - a parser for price files the owner downloads and uploads.
//!
//! Prices are exact `Decimal`s in the instrument's trading currency, never converted.
use crate::contracts::Currency;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Largest response body read from a price source.
pub const MAX_PRICE_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Most daily bars kept per series (about 40 years).
pub const MAX_BARS_PER_SERIES: usize = 10_000;

/// Most requests per second sent to a price source.
pub const PRICE_MAX_REQUESTS_PER_SECOND: u32 = 1;

/// Most instruments one refresh asks about.
pub const MAX_INSTRUMENTS_PER_REFRESH: usize = 100;

/// Largest owner-uploaded price file.
pub const MAX_PRICE_FILE_BYTES: usize = 4 * 1024 * 1024;

/// How the source adjusted its prices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Adjustment {
    /// Raw traded closes.
    Unadjusted,
    /// Adjusted for splits only.
    SplitAdjusted,
    /// Adjusted for splits and distributions (a total-return series).
    TotalReturn,
    /// The source doesn't say. Treated as split-adjusted for returns, with a warning.
    Unknown,
}

/// One daily bar, for the exchange's session date.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceBar {
    /// Session date (exchange-local calendar date).
    pub session: NaiveDate,
    /// Open, if given.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub open: Option<Decimal>,
    /// High, if given.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub high: Option<Decimal>,
    /// Low, if given.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub low: Option<Decimal>,
    /// Close, positive.
    #[serde(with = "rust_decimal::serde::str")]
    pub close: Decimal,
    /// Volume, if given.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub volume: Option<Decimal>,
}

/// A series as one fetch or one file delivered it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceSeries {
    /// Stable source id, e.g. `"stooq"` or `"owner-file"`.
    pub source_id: String,
    /// The broker symbol it is for, e.g. `"AAPL.US"`.
    pub symbol: String,
    /// The symbol as the source names it, e.g. `"aapl.us"`.
    pub source_symbol: String,
    /// Trading currency, when known.
    pub currency: Option<Currency>,
    /// How the closes are adjusted.
    pub adjustment: Adjustment,
    /// Bars, oldest first, sessions strictly increasing, at most [`MAX_BARS_PER_SERIES`].
    pub bars: Vec<PriceBar>,
    /// sha256 of the source bytes.
    pub payload_sha256: String,
}

/// The EPIC's `MarketObservation`: one stored close.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketObservation {
    /// Ledger instrument key.
    pub instrument_key: String,
    /// Broker symbol.
    pub symbol: String,
    /// Session date.
    pub session: NaiveDate,
    /// The close.
    #[serde(with = "rust_decimal::serde::str")]
    pub close: Decimal,
    /// Trading currency, when known.
    pub currency: Option<Currency>,
    /// Always `"close"` this round.
    pub price_kind: String,
    /// Adjustment of the series it came from.
    pub adjustment: Adjustment,
    /// Source id.
    pub source_id: String,
    /// When SAIL first stored this session's close from this source. Point-in-time
    /// questions use it as the bound.
    pub first_available_at: DateTime<Utc>,
    /// When SAIL stored this version.
    pub ingested_at: DateTime<Utc>,
}

/// Why a price request failed. Messages are safe to show the owner.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PriceSourceError {
    /// The owner hasn't opted in (`SAIL_PRICES_SOURCE` unset).
    #[error(
        "Price fetching is off. It sends held symbols to the provider; set SAIL_PRICES_SOURCE=stooq to allow it, or upload price files instead"
    )]
    NotConfigured,
    /// The source has no such symbol, or SAIL can't map the broker symbol to it.
    #[error("the price source has no data for {0}")]
    UnknownSymbol(String),
    /// HTTP 429 or 403.
    #[error("the price source refused the request (HTTP {0}); SAIL will back off")]
    Refused(u16),
    /// Any other non-success status.
    #[error("the price source returned HTTP {0}")]
    Http(u16),
    /// Body over [`MAX_PRICE_RESPONSE_BYTES`].
    #[error("the price response was too large")]
    TooLarge,
    /// The body wasn't the expected CSV.
    #[error("the price data could not be read: {0}")]
    Malformed(String),
    /// Connection or timeout.
    #[error("the price source could not be reached: {0}")]
    Network(String),
}

/// A daily price source. The real one lives in `crate::market::stooq`; tests use fakes.
#[async_trait::async_trait]
pub trait PriceSource: Send + Sync {
    /// Stable id stored with every bar.
    fn source_id(&self) -> &str;
    /// The full daily history for a broker symbol.
    async fn daily(&self, symbol: &str) -> Result<PriceSeries, PriceSourceError>;
}

/// The source's name for a broker symbol, or `None` when it can't be mapped. XTB writes
/// `TICKER.SUFFIX` with suffixes such as `US`, `DE`, `UK`, `FR`, `NL`, `PT`.
pub fn stooq_symbol(broker_symbol: &str) -> Option<String> {
    // Antigravity implements this; the stub keeps the crate building.
    let _ = broker_symbol;
    None
}

/// Reads Stooq's daily CSV (`Date,Open,High,Low,Close,Volume`).
pub fn parse_stooq_csv(
    body: &[u8],
    symbol: &str,
    source_symbol: &str,
) -> Result<PriceSeries, PriceSourceError> {
    let _ = (body, symbol, source_symbol);
    Err(PriceSourceError::Malformed("not implemented".into()))
}

/// Reads an owner-uploaded price file: CSV with a header row naming at least a date and
/// a close column (English or Portuguese), comma or semicolon separated. `currency` and
/// `adjustment` are what the owner declares for the file.
pub fn parse_price_file(
    bytes: &[u8],
    symbol: &str,
    currency: Option<Currency>,
    adjustment: Adjustment,
) -> Result<PriceSeries, PriceSourceError> {
    let _ = (bytes, symbol, currency, adjustment);
    Err(PriceSourceError::Malformed("not implemented".into()))
}

/// A source that never fetches: what SAIL uses until the owner opts in.
pub struct DisabledSource;

#[async_trait::async_trait]
impl PriceSource for DisabledSource {
    fn source_id(&self) -> &str {
        "none"
    }

    async fn daily(&self, _symbol: &str) -> Result<PriceSeries, PriceSourceError> {
        Err(PriceSourceError::NotConfigured)
    }
}

/// The source the owner chose with `SAIL_PRICES_SOURCE`, and whether fetching is on.
/// Until the Stooq client lands this is always the disabled source.
pub fn source_from_env() -> (std::sync::Arc<dyn PriceSource>, bool) {
    (std::sync::Arc::new(DisabledSource), false)
}
