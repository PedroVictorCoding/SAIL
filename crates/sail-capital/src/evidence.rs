//! Point-in-time research evidence (round 8 M0): public SEC filings, the issuers they belong
//! to, and the deterministic link from a filing to the owner's holdings. Every slice builds
//! against these types; change them only through the lead.
//!
//! The flow:
//!
//! 1. The `sec` slice reads SEC EDGAR's public JSON with [`EdgarApi`]: the ticker → CIK map
//!    and each held issuer's recent filings. It never sends portfolio data anywhere. The
//!    only thing that leaves the host is "give me CIK X's filings", with the owner-declared
//!    User-Agent SEC requires.
//! 2. The store appends each filing as an [`EvidenceRecord`] that carries four separate
//!    times: when the reported period ended, when SEC published it, when SAIL first saw it,
//!    and when SAIL stored it. A historical question asked with an [`crate::AsOf`] sees only
//!    evidence SAIL had observed by `known_by`.
//! 3. The `exposure` slice maps the owner's instruments to issuers and turns visible
//!    filings into [`WatchItem`]s. This is a deterministic rule with no model, and its
//!    outcome is **watch** or **research proposal**, never a trade.
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable source id for SEC EDGAR evidence.
pub const SEC_SOURCE: &str = "sec-edgar";

/// Largest JSON body read from SEC in one response. The ticker map is about 1 MB, and a
/// large issuer's submissions file a few MB. Anything near this cap is a misbehaving
/// server.
pub const MAX_SEC_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Most requests per second sent to SEC. Their fair-access limit is 10, and we stay well
/// under it.
pub const SEC_MAX_REQUESTS_PER_SECOND: u32 = 2;

/// Most issuers one refresh asks SEC about. The owner holds tens at most; this only stops
/// a runaway.
pub const MAX_ISSUERS_PER_REFRESH: usize = 200;

/// Most filings kept per issuer per refresh. Only the "recent" block of the submissions
/// file is read, and only the newest filings within it.
pub const MAX_FILINGS_PER_ISSUER: usize = 200;

/// Longest free-text field kept from SEC (company names, document descriptions), in
/// characters.
pub const MAX_SEC_TEXT_CHARS: usize = 300;

/// A SEC Central Index Key. Displayed zero-padded to ten digits, as SEC's URLs use it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cik(pub u64);

impl Cik {
    /// The largest CIK SEC's ten-digit format can hold.
    pub const MAX: u64 = 9_999_999_999;

    /// Validates a CIK, which must be non-zero and fit in ten digits.
    pub fn new(value: u64) -> Option<Self> {
        (value > 0 && value <= Self::MAX).then_some(Self(value))
    }

    /// The ten-digit, zero-padded form used in `CIK##########.json`.
    pub fn padded(self) -> String {
        format!("{:010}", self.0)
    }
}

impl fmt::Display for Cik {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One row of SEC's ticker map (`company_tickers.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickerEntry {
    /// The issuer.
    pub cik: Cik,
    /// Exchange ticker, uppercase, as SEC lists it. For example `"BRK-B"`.
    pub ticker: String,
    /// Company name as SEC lists it, capped at [`MAX_SEC_TEXT_CHARS`].
    pub title: String,
}

/// The ticker map as fetched at one moment. Identifier mapping is point-in-time too: a
/// ticker can move to another issuer, so every match records which map version it used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickerIndex {
    /// When SAIL fetched it.
    pub fetched_at: DateTime<Utc>,
    /// sha256 of the response body.
    pub payload_sha256: String,
    /// The rows.
    pub entries: Vec<TickerEntry>,
}

/// How a published time is known. Matters for replay: a date-only filing is treated as
/// published at the end of that day in New York, never earlier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishedBasis {
    /// EDGAR's acceptance date-time for the filing.
    Acceptance,
    /// Only the filing date is known, so the time is set to 23:59:59 America/New_York on
    /// that date.
    FilingDateEndOfDay,
}

/// One filing as listed in an issuer's EDGAR submissions file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    /// Accession number with dashes, for example `"0000320193-24-000123"`. Unique per
    /// filing.
    pub accession: String,
    /// The issuer whose submissions file listed it.
    pub cik: Cik,
    /// Form type as SEC writes it, e.g. `"10-K"`, `"8-K"`, `"4"`, `"10-Q/A"`.
    pub form: String,
    /// Filing date (America/New_York calendar date).
    pub filing_date: NaiveDate,
    /// When SEC made it public, in UTC.
    pub published_at: DateTime<Utc>,
    /// How `published_at` is known.
    pub published_basis: PublishedBasis,
    /// Period of report (period end), if listed.
    pub report_date: Option<NaiveDate>,
    /// 8-K item numbers as listed, e.g. `["2.02", "9.01"]`. Empty for other forms.
    pub items: Vec<String>,
    /// Primary document file name, if listed.
    pub primary_document: Option<String>,
    /// Primary document description, if listed, capped.
    pub description: Option<String>,
    /// Link to the filing on sec.gov: the primary document when known, else the filing
    /// index. Built from CIK and accession only, never from free text.
    pub url: String,
}

impl Filing {
    /// True for amendments (`"…/A"`).
    pub fn is_amendment(&self) -> bool {
        self.form.ends_with("/A")
    }
}

/// An issuer's recent filings, as one fetch returned them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuerFilings {
    /// The issuer.
    pub cik: Cik,
    /// Its name per SEC, capped.
    pub name: String,
    /// Its tickers per SEC.
    pub tickers: Vec<String>,
    /// Newest first, at most [`MAX_FILINGS_PER_ISSUER`].
    pub filings: Vec<Filing>,
    /// sha256 of the response body.
    pub payload_sha256: String,
}

/// Why a SEC request failed. Messages are safe to show the owner.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// SEC requires a User-Agent that declares who is asking, with a contact address. The
    /// owner sets `SAIL_SEC_USER_AGENT`; without it, SAIL makes no SEC requests.
    #[error("SEC access isn't configured: set SAIL_SEC_USER_AGENT to \"<name> <contact email>\"")]
    NotConfigured,
    /// SEC answered 429 or 403 (rate limited or blocked).
    #[error("SEC refused the request (HTTP {0}); SAIL will back off")]
    Refused(u16),
    /// Any other non-success status.
    #[error("SEC returned HTTP {0}")]
    Http(u16),
    /// Body over [`MAX_SEC_RESPONSE_BYTES`].
    #[error("the SEC response was too large")]
    TooLarge,
    /// The body wasn't the expected JSON.
    #[error("the SEC response could not be read: {0}")]
    Malformed(String),
    /// Connection or timeout.
    #[error("SEC could not be reached: {0}")]
    Network(String),
}

/// Read-only access to SEC EDGAR's public JSON. The real client lives in `crate::sec`;
/// tests use fakes.
#[async_trait::async_trait]
pub trait EdgarApi: Send + Sync {
    /// Fetches `https://www.sec.gov/files/company_tickers.json`.
    async fn ticker_index(&self) -> Result<TickerIndex, SourceError>;
    /// Fetches `https://data.sec.gov/submissions/CIK##########.json` for `cik`.
    async fn issuer_filings(&self, cik: Cik) -> Result<IssuerFilings, SourceError>;
    /// Round 24: fetches `https://data.sec.gov/api/xbrl/companyfacts/CIK##########.json`
    /// and reads it with [`crate::sec::companyfacts::parse`]. Fakes that don't serve it
    /// answer [`SourceError::NotConfigured`].
    async fn company_facts(
        &self,
        cik: Cik,
    ) -> Result<crate::sec::companyfacts::CompanyFacts, SourceError> {
        let _ = cik;
        Err(SourceError::NotConfigured)
    }
    /// Round 25: fetches the raw XML of a Form 4 (`4` or `4/A`) `filing` from
    /// `https://www.sec.gov/Archives/edgar/data/{cik}/{accession without dashes}/{raw
    /// document}` and reads it with [`crate::sec::form4::parse`]. Fakes that don't serve
    /// it answer [`SourceError::NotConfigured`].
    async fn form4(&self, filing: &Filing) -> Result<crate::sec::form4::Form4, SourceError> {
        let _ = filing;
        Err(SourceError::NotConfigured)
    }
}

/// A filing as stored: the filing plus SAIL's own observation of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    /// Stable id: the first 16 hex characters of sha256(natural key + payload hash).
    pub id: String,
    /// `"sec:<accession>"`.
    pub natural_key: String,
    /// [`SEC_SOURCE`].
    pub source: String,
    /// When the reported period ended, if any. This is the "event" time.
    pub period_end: Option<NaiveDate>,
    /// When the source made it public.
    pub published_at: DateTime<Utc>,
    /// When SAIL first saw it in a fetch. Point-in-time queries use this as the bound.
    pub first_observed_at: DateTime<Utc>,
    /// When SAIL stored this version.
    pub ingested_at: DateTime<Utc>,
    /// sha256 of this filing's canonical JSON. A changed hash for the same accession is a
    /// vendor revision, kept as a new record rather than overwriting the old one.
    pub payload_sha256: String,
    /// Issuer name at observation time.
    pub issuer_name: String,
    /// The filing.
    pub filing: Filing,
}

/// How an owned instrument was linked to a SEC issuer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IssuerMatch {
    /// Exactly one issuer has this ticker in the map version used.
    Matched {
        /// The issuer.
        cik: Cik,
        /// The ticker that matched.
        ticker: String,
        /// The issuer's name per the map.
        title: String,
        /// Which map version was used (its fetch time).
        map_fetched_at: DateTime<Utc>,
    },
    /// The symbol doesn't name a US listing (no `.US` suffix), so SEC doesn't apply.
    NotUsListed,
    /// The ticker isn't in the map, or more than one issuer has it. Never guessed.
    Unresolved {
        /// Why, in plain words.
        reason: String,
    },
}

/// What SAIL suggests about a filing. There is deliberately no "buy" or "sell".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchAction {
    /// Worth knowing about; nothing to do yet.
    Watch,
    /// Material enough that the owner should read it, or a later research round should
    /// build a dossier.
    ResearchProposal,
}

/// The owner's exposure to an issuer at the time of the question, from the ledger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exposure {
    /// Instrument key in the ledger.
    pub instrument_key: String,
    /// Units held.
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Cost basis, if the ledger knows it.
    pub cost_basis: Option<crate::Money>,
    /// The broker's value at its last snapshot, if any.
    pub broker_market_value: Option<crate::Money>,
    /// This position's share of all owned positions' broker value. Present only when every
    /// owned position has a value in one currency. A fraction between 0 and 1.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub share_of_owned_value: Option<Decimal>,
}

/// One filing that touches something the owner holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchItem {
    /// The evidence it rests on.
    pub evidence_id: String,
    /// The filing.
    pub filing: Filing,
    /// Issuer name.
    pub issuer_name: String,
    /// The holding it touches.
    pub exposure: Exposure,
    /// Watch or research proposal.
    pub action: ResearchAction,
    /// Why, in plain words, quoting only the form type and item numbers.
    pub reason: String,
    /// When SEC published it.
    pub published_at: DateTime<Utc>,
    /// When SAIL first saw it.
    pub first_observed_at: DateTime<Utc>,
}
