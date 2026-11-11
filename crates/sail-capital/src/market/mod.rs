//! Market data (round 10, CAP-15): daily closing prices for owned instruments, as the
//! EPIC's `MarketObservation`. A price is an observation with a source, a session date and
//! the time SAIL first had it, never a prediction.
//!
//! The adapters (Antigravity's slice) are:
//! - opt-in daily-bar clients (`yahoo`, through the yfinance helper; `stooq`). Fetching sends the held symbols to the provider, so it
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

/// Column indexes read from a CSV header, shared by the price adapters.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PriceColumns {
    /// Date column.
    pub(crate) date: usize,
    /// Close column.
    pub(crate) close: usize,
    /// Open column, if present.
    pub(crate) open: Option<usize>,
    /// High column, if present.
    pub(crate) high: Option<usize>,
    /// Low column, if present.
    pub(crate) low: Option<usize>,
    /// Volume column, if present.
    pub(crate) volume: Option<usize>,
}

/// Reads bars from a CSV reader with the policy both adapters share: skip malformed rows
/// and non-positive closes, keep the last duplicate session with a warning, and cap at
/// [`MAX_BARS_PER_SERIES`] from the newest end. `parse_number` reads prices: machine
/// sources pass [`machine_decimal`], owner files their delimiter-aware reader. `label` names the source in warnings and
/// the empty-result error.
pub(crate) fn read_price_bars<R: std::io::Read>(
    reader: &mut csv::Reader<R>,
    columns: &PriceColumns,
    parse_date: impl Fn(&str) -> Option<NaiveDate>,
    parse_number: impl Fn(&str) -> Option<Decimal>,
    label: &str,
) -> Result<Vec<PriceBar>, PriceSourceError> {
    let mut by_session: std::collections::BTreeMap<NaiveDate, PriceBar> = Default::default();
    let mut skipped = 0usize;
    for result in reader.records() {
        let record = match result {
            Ok(record) => record,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let Some(session) = record.get(columns.date).and_then(&parse_date) else {
            skipped += 1;
            continue;
        };
        let Some(close) = record.get(columns.close).and_then(&parse_number) else {
            skipped += 1;
            continue;
        };
        if close <= Decimal::ZERO {
            skipped += 1;
            continue;
        }
        let number = |column: Option<usize>| {
            column
                .and_then(|index| record.get(index))
                .and_then(&parse_number)
        };
        let bar = PriceBar {
            session,
            open: number(columns.open),
            high: number(columns.high),
            low: number(columns.low),
            close,
            volume: number(columns.volume),
        };
        if by_session.insert(session, bar).is_some() {
            tracing::warn!(
                %session,
                label,
                "duplicate session in price data; keeping last occurrence"
            );
        }
    }
    if skipped > 0 {
        tracing::warn!(
            skipped,
            label,
            "skipped malformed or non-positive close rows"
        );
    }
    if by_session.is_empty() {
        return Err(PriceSourceError::Malformed(format!(
            "no valid price bars in {label}"
        )));
    }
    let mut bars: Vec<PriceBar> = by_session.into_values().collect();
    if bars.len() > MAX_BARS_PER_SERIES {
        let excess = bars.len() - MAX_BARS_PER_SERIES;
        bars.drain(0..excess);
    }
    Ok(bars)
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

/// Open, high and low of one session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ohl {
    /// Open.
    #[serde(with = "rust_decimal::serde::str")]
    pub open: Decimal,
    /// High.
    #[serde(with = "rust_decimal::serde::str")]
    pub high: Decimal,
    /// Low.
    #[serde(with = "rust_decimal::serde::str")]
    pub low: Decimal,
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
    /// Open, high and low of the session, when the source gave them (round 12; rows
    /// stored earlier have none). Chart-pattern models need them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ohl: Option<Ohl>,
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
        "Price fetching is off. It sends held symbols to the provider; set SAIL_PRICES_SOURCE=yfinance to allow it, or upload price files instead"
    )]
    NotConfigured,
    /// The source has no such symbol, or SAIL can't map the broker symbol to it.
    #[error("the price source has no data for {0}")]
    UnknownSymbol(String),
    /// HTTP 429 or 403.
    #[error("the price source refused the request (HTTP {0}); SAIL will back off")]
    Refused(u16),
    /// The source answered with a web page (a bot check) instead of data. SAIL does not
    /// try to get past such checks.
    #[error(
        "the price source asked for a browser check instead of returning prices; SAIL won't get around that, so upload price files instead"
    )]
    Blocked,
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

/// A number from a machine-written source (a dot decimal, optional exponent), never a
/// locale guess: `54.123` is fifty-four, not fifty-four thousand.
pub(crate) fn machine_decimal(text: &str) -> Option<Decimal> {
    let text = text.trim();
    if text.is_empty() || text.len() > 40 {
        return None;
    }
    use std::str::FromStr;
    Decimal::from_str(text)
        .ok()
        .or_else(|| Decimal::from_scientific(text).ok())
}

/// A daily price source. The real one lives in `crate::market::stooq`; tests use fakes.
#[async_trait::async_trait]
pub trait PriceSource: Send + Sync {
    /// Stable id stored with every bar.
    fn source_id(&self) -> &str;
    /// The full daily history for a broker symbol.
    async fn daily(&self, symbol: &str) -> Result<PriceSeries, PriceSourceError>;
}

pub mod file;
pub mod stooq;
pub mod yahoo;

pub use file::parse_price_file;
pub use stooq::{StooqClient, parse_stooq_csv, stooq_symbol};
pub use yahoo::{YahooSource, parse_yfinance_json, yahoo_symbol};

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

/// The source the owner chose with `SAIL_PRICES_SOURCE`, and whether fetching is on:
/// `yfinance` runs the yfinance helper, `stooq` the Stooq client (which Stooq's browser
/// check now blocks), anything else leaves fetching off.
pub fn source_from_env() -> (std::sync::Arc<dyn PriceSource>, bool) {
    let chosen = std::env::var("SAIL_PRICES_SOURCE")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match chosen.as_str() {
        "yfinance" | "yahoo" => (std::sync::Arc::new(yahoo::YahooSource::from_env()), true),
        "stooq" => (std::sync::Arc::new(stooq::StooqClient::from_env()), true),
        _ => (std::sync::Arc::new(DisabledSource), false),
    }
}
