//! SEC EDGAR client and parsers (Kilo's slice, round 8).
//!
//! Reads only SEC's public JSON. It never sends portfolio data, and the only identifying
//! thing it sends is the owner-declared User-Agent that SEC's fair-access policy requires.
//! The parsers are pure functions, so they can be tested on fixtures without the network.
use crate::evidence::*;
use chrono::{DateTime, Utc};

/// Parses `company_tickers.json`. M0 stub; Kilo replaces the body.
pub fn parse_ticker_index(
    body: &[u8],
    fetched_at: DateTime<Utc>,
) -> Result<TickerIndex, SourceError> {
    let _ = (body, fetched_at);
    Err(SourceError::Malformed(
        "not implemented yet (M0 stub)".into(),
    ))
}

/// Parses `CIK##########.json` for `cik`. M0 stub; Kilo replaces the body.
pub fn parse_submissions(body: &[u8], cik: Cik) -> Result<IssuerFilings, SourceError> {
    let _ = (body, cik);
    Err(SourceError::Malformed(
        "not implemented yet (M0 stub)".into(),
    ))
}

/// The real EDGAR client: rate-limited, size-capped, and needs the User-Agent.
pub struct EdgarClient {
    user_agent: Option<String>,
}

impl EdgarClient {
    /// Builds a client from `$SAIL_SEC_USER_AGENT`. Without it, every call returns
    /// [`SourceError::NotConfigured`].
    pub fn from_env() -> Self {
        Self {
            user_agent: std::env::var("SAIL_SEC_USER_AGENT")
                .ok()
                .filter(|v| !v.trim().is_empty()),
        }
    }

    /// True when a User-Agent is set.
    pub fn is_configured(&self) -> bool {
        self.user_agent.is_some()
    }
}

#[async_trait::async_trait]
impl EdgarApi for EdgarClient {
    async fn ticker_index(&self) -> Result<TickerIndex, SourceError> {
        Err(SourceError::NotConfigured)
    }

    async fn issuer_filings(&self, cik: Cik) -> Result<IssuerFilings, SourceError> {
        let _ = cik;
        Err(SourceError::NotConfigured)
    }
}
