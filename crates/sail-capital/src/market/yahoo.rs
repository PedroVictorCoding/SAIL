//! Yahoo daily prices through the `yfinance` Python library (round 10).
//!
//! SAIL runs `scripts/prices/yfinance_daily.py` as a child process, one symbol per run,
//! and reads one JSON object from its stdout. It is opt-in: only used when the owner sets
//! `SAIL_PRICES_SOURCE=yfinance`. Only the symbol leaves the machine.
//!
//! - `SAIL_YFINANCE_PYTHON`: the interpreter (default [`DEFAULT_PYTHON`], made by
//!   `scripts/prices/setup.sh`).
//! - `SAIL_YFINANCE_SCRIPT`: the helper (default [`DEFAULT_SCRIPT`], relative to SAIL's
//!   working directory, like the voice worker).
//! - `SAIL_YFINANCE_CACHE`: yfinance's small cache directory (default [`DEFAULT_CACHE`]).
//! - `SAIL_YFINANCE_SEARCH_SCRIPT`: the symbol-search helper (default
//!   [`DEFAULT_SEARCH_SCRIPT`]).
use super::{
    Adjustment, MAX_BARS_PER_SERIES, MAX_PRICE_RESPONSE_BYTES, PRICE_MAX_REQUESTS_PER_SECOND,
    PriceBar, PriceSeries, PriceSource, PriceSourceError, SymbolMatch,
};
use crate::contracts::Currency;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Default interpreter, inside the environment `scripts/prices/setup.sh` makes.
pub const DEFAULT_PYTHON: &str = "/srv/sail-data/venvs/prices/bin/python";

/// Default helper script, relative to SAIL's working directory.
pub const DEFAULT_SCRIPT: &str = "scripts/prices/yfinance_daily.py";

/// Default symbol-search helper script, relative to SAIL's working directory.
pub const DEFAULT_SEARCH_SCRIPT: &str = "scripts/prices/search.py";

/// Most search results kept from one helper run.
const MAX_SEARCH_RESULTS: usize = 10;

/// Longest owner query sent to the search helper.
const MAX_SEARCH_QUERY_CHARS: usize = 64;

/// Default cache directory for yfinance.
pub const DEFAULT_CACHE: &str = "/srv/sail-data/capital/yfinance-cache";

/// Longest one helper run may take.
const RUN_TIMEOUT: Duration = Duration::from_secs(90);

/// Environment variables passed through to the helper; everything else is cleared.
const PASS_ENV: [&str; 8] = [
    "PATH",
    "HOME",
    "LANG",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "HTTPS_PROXY",
    "https_proxy",
    "NO_PROXY",
];

/// Yahoo's name for a broker symbol, or `None` when it can't be mapped. XTB writes
/// `TICKER.SUFFIX`. US listings have no Yahoo suffix and use `-` for share classes
/// (`BRK.B.US` → `BRK-B`). CFDs and other suffix-less symbols are `None`.
pub fn yahoo_symbol(broker_symbol: &str) -> Option<String> {
    let raw = broker_symbol.trim();
    let (prefix, suffix) = raw.rsplit_once('.')?;
    if prefix.is_empty()
        || !prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return None;
    }
    let prefix = prefix.to_ascii_uppercase();
    let yahoo_suffix = match suffix.to_ascii_uppercase().as_str() {
        "US" => return Some(prefix.replace('.', "-")),
        "PT" => "LS",
        "NL" => "AS",
        "DE" => "DE",
        "UK" => "L",
        "FR" => "PA",
        "ES" => "MC",
        "IT" => "MI",
        "PL" => "WA",
        "BE" => "BR",
        "CH" => "SW",
        "DK" => "CO",
        "SE" => "ST",
        "NO" => "OL",
        "FI" => "HE",
        "AT" => "VI",
        "IE" => "IR",
        _ => return None,
    };
    Some(format!("{prefix}.{yahoo_suffix}"))
}

/// `[date, open, high, low, close, volume]`, numbers as strings.
type HelperBar = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[derive(Deserialize)]
struct HelperOutput {
    error: Option<String>,
    detail: Option<String>,
    currency: Option<String>,
    adjustment: Option<Adjustment>,
    #[serde(default)]
    bars: Vec<HelperBar>,
}

/// Reads the helper's JSON into a series.
pub fn parse_yfinance_json(
    body: &[u8],
    symbol: &str,
    source_symbol: &str,
) -> Result<PriceSeries, PriceSourceError> {
    let out: HelperOutput = serde_json::from_slice(body).map_err(|_| {
        PriceSourceError::Malformed("the yfinance helper's output wasn't JSON".into())
    })?;
    if let Some(error) = out.error.as_deref() {
        return Err(match error {
            "no_data" | "bad_symbol" => PriceSourceError::UnknownSymbol(symbol.to_string()),
            "rate_limited" => PriceSourceError::Refused(429),
            _ => PriceSourceError::Network(format!(
                "the yfinance helper failed ({})",
                out.detail.as_deref().unwrap_or("no detail")
            )),
        });
    }
    let dec = |v: &Option<String>| v.as_deref().and_then(super::machine_decimal);
    let mut by_session: BTreeMap<NaiveDate, PriceBar> = BTreeMap::new();
    let mut skipped = 0usize;
    for (date, open, high, low, close, volume) in &out.bars {
        let (Ok(session), Some(close)) = (
            NaiveDate::parse_from_str(date, "%Y-%m-%d"),
            dec(close).filter(|c| *c > Decimal::ZERO),
        ) else {
            skipped += 1;
            continue;
        };
        by_session.insert(
            session,
            PriceBar {
                session,
                open: dec(open),
                high: dec(high),
                low: dec(low),
                close,
                volume: dec(volume),
            },
        );
    }
    if skipped > 0 {
        tracing::warn!(skipped, "skipped unreadable rows from the yfinance helper");
    }
    if by_session.is_empty() {
        return Err(PriceSourceError::UnknownSymbol(symbol.to_string()));
    }
    let mut bars: Vec<PriceBar> = by_session.into_values().collect();
    if bars.len() > MAX_BARS_PER_SERIES {
        bars.drain(0..bars.len() - MAX_BARS_PER_SERIES);
    }
    Ok(PriceSeries {
        source_id: "yfinance".to_string(),
        symbol: symbol.to_string(),
        source_symbol: source_symbol.to_string(),
        currency: out.currency.as_deref().and_then(Currency::parse),
        adjustment: out.adjustment.unwrap_or(Adjustment::Unknown),
        bars,
        payload_sha256: crate::sha256_hex(body),
    })
}

/// Whether a symbol is safe to pass to a Yahoo helper.
fn plausible_symbol(symbol: &str) -> bool {
    !symbol.is_empty()
        && symbol.len() <= 24
        && symbol
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-=^".contains(&b))
        && (symbol.as_bytes()[0].is_ascii_alphanumeric() || symbol.as_bytes()[0] == b'^')
}

#[derive(Deserialize)]
struct SearchOutput {
    error: Option<String>,
    detail: Option<String>,
    #[serde(default)]
    results: Vec<SearchQuote>,
}

#[derive(Deserialize)]
struct SearchQuote {
    symbol: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    exchange: String,
    #[serde(default)]
    quote_type: String,
}

/// Reads the search helper's JSON into usable matches. Entries with a symbol the daily
/// helper couldn't fetch are dropped, as are duplicates.
pub fn parse_search_json(body: &[u8]) -> Result<Vec<SymbolMatch>, PriceSourceError> {
    let out: SearchOutput = serde_json::from_slice(body).map_err(|_| {
        PriceSourceError::Malformed("the search helper's output wasn't JSON".into())
    })?;
    if let Some(error) = out.error.as_deref() {
        return Err(match error {
            "bad_query" => {
                PriceSourceError::Malformed("the search query was not valid".to_string())
            }
            "rate_limited" => PriceSourceError::Refused(429),
            _ => PriceSourceError::Network(format!(
                "the search helper failed ({})",
                out.detail.as_deref().unwrap_or("no detail")
            )),
        });
    }

    let mut seen = std::collections::BTreeSet::new();
    let mut results = Vec::new();
    for quote in out.results {
        if !plausible_symbol(&quote.symbol) || !seen.insert(quote.symbol.clone()) {
            continue;
        }
        let name = quote.name.trim();
        let name = if name.is_empty() {
            quote.symbol.clone()
        } else {
            name.chars().take(120).collect()
        };
        results.push(SymbolMatch {
            symbol: quote.symbol,
            name,
            exchange: quote.exchange.trim().chars().take(80).collect(),
            quote_type: quote.quote_type.trim().to_ascii_uppercase(),
        });
        if results.len() >= MAX_SEARCH_RESULTS {
            break;
        }
    }
    Ok(results)
}

/// Runs the yfinance helper.
#[derive(Clone)]
pub struct YahooSource {
    python: String,
    script: String,
    search_script: String,
    cache: Option<String>,
    gate: Arc<Mutex<Option<Instant>>>,
}

impl YahooSource {
    /// Reads `SAIL_YFINANCE_PYTHON`, `SAIL_YFINANCE_SCRIPT`, `SAIL_YFINANCE_SEARCH_SCRIPT`
    /// and `SAIL_YFINANCE_CACHE`.
    pub fn from_env() -> Self {
        let var = |name: &str, default: &str| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        Self {
            python: var("SAIL_YFINANCE_PYTHON", DEFAULT_PYTHON),
            script: var("SAIL_YFINANCE_SCRIPT", DEFAULT_SCRIPT),
            search_script: var("SAIL_YFINANCE_SEARCH_SCRIPT", DEFAULT_SEARCH_SCRIPT),
            cache: Some(var("SAIL_YFINANCE_CACHE", DEFAULT_CACHE)),
            gate: Arc::new(Mutex::new(None)),
        }
    }

    /// An explicit interpreter and script, for tests. The script also serves symbol
    /// search, so a fake helper can answer both.
    pub fn with_command(python: impl Into<String>, script: impl Into<String>) -> Self {
        let script = script.into();
        Self {
            python: python.into(),
            script: script.clone(),
            search_script: script,
            cache: None,
            gate: Arc::new(Mutex::new(None)),
        }
    }

    async fn wait_for_slot(&self) {
        let interval =
            Duration::from_millis(1_000 / u64::from(PRICE_MAX_REQUESTS_PER_SECOND.max(1)));
        let mut last = self.gate.lock().await;
        if let Some(previous) = *last {
            let elapsed = previous.elapsed();
            if elapsed < interval {
                tokio::time::sleep(interval - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }

    async fn run(&self, yahoo: &str) -> Result<Vec<u8>, PriceSourceError> {
        self.run_helper(&self.script, yahoo).await
    }

    async fn search_helper(&self, query: &str) -> Result<Vec<u8>, PriceSourceError> {
        self.run_helper(&self.search_script, query).await
    }

    async fn run_helper(&self, script: &str, argument: &str) -> Result<Vec<u8>, PriceSourceError> {
        let mut command = tokio::process::Command::new(&self.python);
        command
            .arg("-I")
            .arg(script)
            .arg(argument)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for name in PASS_ENV {
            if let Ok(value) = std::env::var(name) {
                command.env(name, value);
            }
        }
        if let Some(cache) = &self.cache
            && std::fs::create_dir_all(cache).is_ok()
        {
            command.env("XDG_CACHE_HOME", cache);
        }
        let mut child = command.spawn().map_err(|_| {
            PriceSourceError::Network(
                "the yfinance helper could not start; run scripts/prices/setup.sh".into(),
            )
        })?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| PriceSourceError::Network("the yfinance helper had no output".into()))?;
        let read = async {
            let mut body = Vec::new();
            (&mut stdout)
                .take(MAX_PRICE_RESPONSE_BYTES as u64 + 1)
                .read_to_end(&mut body)
                .await
                .map_err(|_| {
                    PriceSourceError::Network("reading the yfinance helper failed".into())
                })?;
            if body.len() > MAX_PRICE_RESPONSE_BYTES {
                return Err(PriceSourceError::TooLarge);
            }
            let _ = child.wait().await;
            Ok(body)
        };
        match tokio::time::timeout(RUN_TIMEOUT, read).await {
            Ok(result) => result,
            Err(_) => Err(PriceSourceError::Network("the request timed out".into())),
        }
    }
}

#[async_trait::async_trait]
impl PriceSource for YahooSource {
    fn source_id(&self) -> &str {
        "yfinance"
    }

    async fn daily_as(
        &self,
        symbol: &str,
        source_symbol: &str,
    ) -> Result<PriceSeries, PriceSourceError> {
        if !plausible_symbol(source_symbol) {
            return Err(PriceSourceError::UnknownSymbol(symbol.to_string()));
        }
        self.wait_for_slot().await;
        let body = self.run(source_symbol).await?;
        parse_yfinance_json(&body, symbol, source_symbol)
    }

    async fn daily(&self, symbol: &str) -> Result<PriceSeries, PriceSourceError> {
        let yahoo = yahoo_symbol(symbol)
            .ok_or_else(|| PriceSourceError::UnknownSymbol(symbol.to_string()))?;
        self.wait_for_slot().await;
        let body = self.run(&yahoo).await?;
        parse_yfinance_json(&body, symbol, &yahoo)
    }

    async fn search(&self, query: &str) -> Result<Vec<SymbolMatch>, PriceSourceError> {
        let query = query.trim();
        if query.is_empty()
            || query.chars().count() > MAX_SEARCH_QUERY_CHARS
            || query.chars().any(char::is_control)
        {
            return Err(PriceSourceError::Malformed(
                "the search query is empty or too long".into(),
            ));
        }
        self.wait_for_slot().await;
        let body = self.search_helper(query).await?;
        parse_search_json(&body)
    }
}
