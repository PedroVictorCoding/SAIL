//! Shared types for SAIL Capital (M0). Every slice builds against these; change them only
//! through the lead (see `docs/SAIL_CAPITAL.md` and the round's handoff file).
//!
//! The data flow of the first vertical slice is:
//!
//! 1. The owner exports official reports from the broker (XTB web platform → History →
//!    Export) and uploads the file. SAIL never signs in to the broker and never asks for a
//!    password, session or 2FA secret.
//! 2. An importer (`import::xtb`) turns the file into a [`ParsedReport`]: typed
//!    [`ParsedEntry`] rows, an optional [`BrokerSnapshot`] of what the broker itself says the
//!    account holds, and [`ImportIssue`]s for anything it could not read. It never guesses:
//!    an unknown row becomes [`EntryKind::Unsupported`], not a silent drop.
//! 3. The store appends the entries to the private ledger. Entries are never overwritten:
//!    the same [`ParsedEntry::natural_key`] seen again with the same content is a duplicate,
//!    with different content it is recorded as a conflict for the owner to resolve.
//! 4. The ledger (`ledger::project`) replays the entries known at a point in time into
//!    positions, cash, dividends, fees and realized results, and reconciles them against the
//!    broker's own snapshot. Variances are reported, never smoothed over.
//!
//! Money is always [`Decimal`], never `f64`. Every amount carries its currency. Everything
//! read from a report is untrusted data: cells are length-capped and never interpreted as
//! instructions or markup.
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Version of the ledger contract. Stored with every batch so a later reader knows how the
/// rows were produced.
pub const CONTRACT_VERSION: &str = "capital-ledger/1";

/// Largest report file accepted, in bytes. Years of XTB history are well under a megabyte;
/// anything near this is the wrong file.
pub const MAX_REPORT_BYTES: usize = 32 * 1024 * 1024;

/// Most data rows read from one report across all sheets. Stops a runaway file.
pub const MAX_REPORT_ROWS: usize = 200_000;

/// Most sheets read from one workbook.
pub const MAX_SHEETS: usize = 32;

/// Longest text kept from one cell, in characters. Longer cells are cut and flagged.
pub const MAX_CELL_CHARS: usize = 512;

/// Most issues kept per import. Further issues are counted, not stored.
pub const MAX_ISSUES: usize = 2_000;

// ---------------------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------------------

/// The broker a report came from. Each broker gets its own importer; `Manual` is for
/// entries the owner types in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Broker {
    /// XTB (xStation). Its public API was discontinued on 2025-03-14, so official report
    /// exports are the supported path.
    Xtb,
    /// Entries the owner records by hand.
    Manual,
}

impl Broker {
    /// Stable lowercase name, used in ids and natural keys.
    pub fn as_str(self) -> &'static str {
        match self {
            Broker::Xtb => "xtb",
            Broker::Manual => "manual",
        }
    }
}

/// A private account id, `"<broker>:<label>"`. The label is a masked account number (last
/// four digits) or an owner-chosen name, never the full account number.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(String);

impl AccountId {
    /// Build an id from a broker and a raw account number or label. Digits-only labels
    /// longer than four characters are masked to their last four, so the full account
    /// number never reaches storage, logs or the UI.
    pub fn new(broker: Broker, label: &str) -> Self {
        let clean: String = label
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(32)
            .collect();
        let label = if clean.len() > 4 && clean.chars().all(|c| c.is_ascii_digit()) {
            format!("…{}", &clean[clean.len() - 4..])
        } else if clean.is_empty() {
            "default".to_string()
        } else {
            clean
        };
        Self(format!("{}:{label}", broker.as_str()))
    }

    /// Wrap a stored id as-is (store and HTTP layers only).
    pub fn from_stored(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The id text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An ISO 4217 currency code. Constructed only through [`Currency::parse`], so a value of
/// this type is always three uppercase ASCII letters.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Currency(String);

impl Currency {
    /// Parse a code, ignoring surrounding space and case. Returns `None` for anything that
    /// isn't three letters; the caller turns that into an [`ImportIssue`].
    pub fn parse(code: &str) -> Option<Self> {
        let code = code.trim();
        (code.len() == 3 && code.bytes().all(|b| b.is_ascii_alphabetic()))
            .then(|| Self(code.to_ascii_uppercase()))
    }

    /// The euro, the owner's reporting currency.
    pub fn eur() -> Self {
        Self("EUR".to_string())
    }

    /// The code text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Currency {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Currency::parse(&value).ok_or_else(|| format!("not a currency code: {value:?}"))
    }
}

impl From<Currency> for String {
    fn from(value: Currency) -> Self {
        value.0
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An exact amount in one currency. Serialized as `{"amount":"12.34","currency":"EUR"}`,
/// with the amount as a string so no JSON reader turns it into a float.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Money {
    /// Exact amount. Sign meaning is documented on each field that holds a `Money`.
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Decimal,
    /// Currency of `amount`.
    pub currency: Currency,
}

impl Money {
    /// Pair an amount with its currency.
    pub fn new(amount: Decimal, currency: Currency) -> Self {
        Self { amount, currency }
    }
}

/// What kind of instrument a position or trade is in. CFDs are kept apart from owned
/// securities everywhere: they are leveraged contracts, not holdings, and must never be
/// summed into the same position or exposure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentKind {
    /// A share owned outright.
    Stock,
    /// An exchange-traded fund owned outright.
    Etf,
    /// A contract for difference (leveraged, not owned).
    Cfd,
    /// The report didn't say, and the importer couldn't tell from documented fields.
    Unknown,
}

/// A security as the report names it. Identity across reports is [`Instrument::key`]: the
/// ISIN when known, else the broker symbol. A later security master (CAP-02) maps these to
/// issuers and listings; this round only records what the report says.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Instrument {
    /// Broker symbol as written, e.g. `"AAPL.US"`, trimmed and capped.
    pub symbol: String,
    /// ISIN if the report gives one and it passes [`isin_is_valid`].
    pub isin: Option<String>,
    /// Instrument name as written, if given.
    pub name: Option<String>,
    /// Trading currency, if given.
    pub currency: Option<Currency>,
    /// Owned security, CFD or unknown.
    pub kind: InstrumentKind,
}

impl Instrument {
    /// The grouping key: `"isin:<ISIN>"` when known, else `"sym:<SYMBOL>"`, with the kind
    /// appended for CFDs so a CFD and a share on the same name never merge.
    pub fn key(&self) -> String {
        let base = match &self.isin {
            Some(isin) => format!("isin:{isin}"),
            None => format!("sym:{}", self.symbol.to_ascii_uppercase()),
        };
        match self.kind {
            InstrumentKind::Cfd => format!("{base}#cfd"),
            _ => base,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------------------

/// How precisely a report states when something happened. A date-only row must not be
/// ordered against same-day timed rows as if it had a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimePrecision {
    /// Date and time of day, converted to UTC from the report's stated or configured zone.
    Exact,
    /// Only the date is known; the time is set to 00:00 in the report's zone.
    DateOnly,
}

/// When something happened, as the report states it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventTime {
    /// The instant, in UTC.
    pub at: DateTime<Utc>,
    /// Whether the time of day is real.
    pub precision: TimePrecision,
}

// ---------------------------------------------------------------------------------------
// Parsed report (importer output)
// ---------------------------------------------------------------------------------------

/// Which part of the broker's export a sheet or file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SheetKind {
    /// Cash operations: deposits, withdrawals, purchases, sales, dividends, taxes, fees.
    CashOperations,
    /// Closed positions with open and close prices and results.
    ClosedPositions,
    /// Open positions at export time.
    OpenPositions,
    /// Order history.
    Orders,
    /// A sheet the importer doesn't recognise. Listed so the owner sees it was skipped.
    Unknown,
}

/// Where a row came from inside the uploaded file.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RowRef {
    /// Sheet name (`None` for CSV).
    pub sheet: Option<String>,
    /// Which part of the export the sheet holds.
    pub sheet_kind: SheetKind,
    /// 1-based row number as a spreadsheet shows it.
    pub row: u32,
}

/// Buy or sell side of a trade.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// Opening or adding to a long position.
    Buy,
    /// Reducing or closing a long position.
    Sell,
}

/// What a ledger entry records. Amount signs are explained on [`ParsedEntry::cash_effect`];
/// the amounts inside each kind are **unsigned magnitudes** as the report states them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntryKind {
    /// Money paid in to the account.
    Deposit,
    /// Money taken out of the account.
    Withdrawal,
    /// Money moved between the owner's own accounts or sub-accounts.
    Transfer,
    /// A purchase or sale of an owned security. `quantity` and `price` are present only
    /// when the report states them; the importer never derives a quantity from an amount.
    Trade {
        /// The security.
        instrument: Instrument,
        /// Buy or sell.
        side: Side,
        /// Units traded, positive, if stated.
        #[serde(default, with = "rust_decimal::serde::str_option")]
        quantity: Option<Decimal>,
        /// Price per unit in the instrument's currency, if stated.
        price: Option<Money>,
        /// Broker position or order id linking this cash row to a position row, if stated.
        position_ref: Option<String>,
    },
    /// A cash dividend or distribution credited, before any tax row.
    Dividend {
        /// The paying security, if the row names it.
        instrument: Option<Instrument>,
    },
    /// Tax withheld at source or charged by the broker.
    Tax {
        /// Withholding on a distribution, tax on interest, or other.
        kind: TaxKind,
        /// The security it relates to, if the row names it.
        instrument: Option<Instrument>,
    },
    /// Interest credited on free cash.
    Interest,
    /// A fee or charge.
    Fee {
        /// Commission, regulatory fee, swap, currency conversion, custody, or other.
        kind: FeeKind,
        /// The security it relates to, if the row names it.
        instrument: Option<Instrument>,
    },
    /// Realized result of a closed CFD or other derivative, booked as cash.
    DerivativeResult {
        /// The contract.
        instrument: Instrument,
        /// Broker position id, if stated.
        position_ref: Option<String>,
    },
    /// A row the importer read but can't classify. Kept, shown to the owner, and excluded
    /// from every projection except the raw cash balance (which uses `cash_effect`).
    Unsupported {
        /// The broker's type text, capped at [`MAX_CELL_CHARS`].
        raw_type: String,
    },
}

/// Kinds of tax rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaxKind {
    /// Withholding at source on a dividend or distribution.
    Withholding,
    /// Tax on interest.
    OnInterest,
    /// Any other tax row; see the entry comment.
    Other,
}

/// Kinds of fee rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeeKind {
    /// Trading commission.
    Commission,
    /// Regulatory transaction fee (e.g. the US SEC fee).
    Regulatory,
    /// Overnight financing (swap) on a leveraged position.
    Swap,
    /// Currency conversion charge.
    FxConversion,
    /// Custody or account fee.
    Custody,
    /// Any other fee; see the entry comment.
    Other,
}

/// One row of a report, read and typed. Produced by an importer; stored unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedEntry {
    /// Idempotency key, unique per account: `"<broker>:<sheet_kind>:<broker id>"` when the
    /// row has a broker id, else `"<broker>:<sheet_kind>:h:<sha256 of the normalised row>"`.
    /// Must not depend on the row number, the file name or the import time, so the same
    /// operation in a later, longer export maps to the same key.
    pub natural_key: String,
    /// Where the row is in the file.
    pub source: RowRef,
    /// The broker's own id for the row (operation or position id), if any.
    pub broker_ref: Option<String>,
    /// When it happened.
    pub occurred: EventTime,
    /// What it records.
    pub kind: EntryKind,
    /// Signed change to the account's cash, as the broker reports it: positive adds cash,
    /// negative removes it. `None` for rows that carry no cash amount (e.g. a position row).
    pub cash_effect: Option<Money>,
    /// The broker's comment text, capped at [`MAX_CELL_CHARS`]. Data only.
    pub comment: Option<String>,
}

/// One open position as the broker reports it at export time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerPosition {
    /// The security or contract.
    pub instrument: Instrument,
    /// Broker position id, if stated.
    pub position_ref: Option<String>,
    /// Units held (positive for long). CFD volumes are in lots as the broker states them.
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Open price per unit, if stated.
    pub open_price: Option<Money>,
    /// When the position was opened, if stated.
    pub opened: Option<EventTime>,
    /// Broker's market price per unit at export, if stated.
    pub market_price: Option<Money>,
    /// Broker's market value at export, in account currency, if stated.
    pub market_value: Option<Money>,
    /// Broker's unrealized result at export, in account currency, if stated.
    pub unrealized: Option<Money>,
    /// Where the row is in the file.
    pub source: RowRef,
}

/// One closed position as the broker reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerClosedPosition {
    /// The security or contract.
    pub instrument: Instrument,
    /// Broker position id, if stated.
    pub position_ref: Option<String>,
    /// Units closed, positive.
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Open price per unit, if stated.
    pub open_price: Option<Money>,
    /// Close price per unit, if stated.
    pub close_price: Option<Money>,
    /// When the position was opened, if stated.
    pub opened: Option<EventTime>,
    /// When it was closed.
    pub closed: EventTime,
    /// Broker's realized result in account currency, if stated (gross of tax).
    pub realized: Option<Money>,
    /// Where the row is in the file.
    pub source: RowRef,
}

/// What the broker itself says the account held when the report was exported. The ledger
/// reconciles against this; it is evidence, not truth.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerSnapshot {
    /// The report's own "as of" time, if it states one.
    pub as_of: Option<DateTime<Utc>>,
    /// Open positions at that time.
    pub open_positions: Vec<BrokerPosition>,
    /// Closed positions listed in the report.
    pub closed_positions: Vec<BrokerClosedPosition>,
    /// Cash balance as reported, if stated.
    pub balance: Option<Money>,
    /// Equity (cash plus positions at market) as reported, if stated.
    pub equity: Option<Money>,
}

/// How bad an import issue is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Worth knowing; nothing was lost.
    Info,
    /// A row or field was skipped or cut; totals may be affected.
    Warning,
    /// The file or a sheet couldn't be read; the import is incomplete.
    Error,
}

/// Something the importer couldn't read cleanly. Never silently dropped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportIssue {
    /// How bad it is.
    pub severity: Severity,
    /// Stable machine code, e.g. `"unknown_header"`, `"bad_number"`, `"unknown_type"`.
    pub code: String,
    /// Plain-language explanation for the owner, capped at [`MAX_CELL_CHARS`].
    pub message: String,
    /// The row it's about, if any.
    pub source: Option<RowRef>,
}

/// Counts for one sheet, so the owner can see nothing went missing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetSummary {
    /// Sheet name (`None` for CSV).
    pub name: Option<String>,
    /// What the importer took it to be.
    pub kind: SheetKind,
    /// Data rows seen (header and blank rows excluded).
    pub rows: u32,
    /// Rows turned into entries or snapshot positions.
    pub accepted: u32,
    /// Rows skipped with an issue.
    pub rejected: u32,
}

/// The whole result of reading one report file. Pure data: no ids, no import time. The
/// store adds those.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedReport {
    /// The broker.
    pub broker: Broker,
    /// Account label from the file (masked by [`AccountId::new`]), if the file states one.
    pub account_label: Option<String>,
    /// Account currency from the file, if stated.
    pub account_currency: Option<Currency>,
    /// Ledger rows.
    pub entries: Vec<ParsedEntry>,
    /// What the broker says the account held.
    pub snapshot: BrokerSnapshot,
    /// Per-sheet counts.
    pub sheets: Vec<SheetSummary>,
    /// Problems found, at most [`MAX_ISSUES`].
    pub issues: Vec<ImportIssue>,
    /// Issues found beyond [`MAX_ISSUES`].
    pub issues_dropped: u32,
}

/// Settings an importer needs that the file may not state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportOptions {
    /// Time zone the report's local times are in. Default `Europe/Lisbon`; XTB shows times
    /// in the zone of the platform session, which the owner confirms on upload.
    pub report_tz: chrono_tz::Tz,
    /// Account currency to assume when the file doesn't state it. `None` means rows with
    /// no currency are rejected with an issue.
    pub default_currency: Option<Currency>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            report_tz: chrono_tz::Europe::Lisbon,
            default_currency: None,
        }
    }
}

/// Why a whole file couldn't be read. Row-level trouble is an [`ImportIssue`] instead.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImportError {
    /// Bigger than [`MAX_REPORT_BYTES`].
    #[error("the report is too large ({0} bytes)")]
    TooLarge(usize),
    /// Not an XLSX or CSV the importer can open.
    #[error("the file isn't a readable report: {0}")]
    Unreadable(String),
    /// Opened, but no sheet looks like a known export.
    #[error("no known report sheet was found")]
    NoKnownSheet,
}

// ---------------------------------------------------------------------------------------
// Stored ledger (store output, ledger input)
// ---------------------------------------------------------------------------------------

/// A stored entry: the parsed row plus where and when SAIL got it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerRecord {
    /// Stable id: the first 16 hex characters of sha256(account id + natural key).
    pub id: String,
    /// The account.
    pub account: AccountId,
    /// The import batch that first brought it in.
    pub batch_id: String,
    /// When SAIL first stored it. Point-in-time queries use this as "known since".
    pub ingested_at: DateTime<Utc>,
    /// The row.
    pub entry: ParsedEntry,
}

/// A stored broker snapshot, from one import.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSnapshot {
    /// The account.
    pub account: AccountId,
    /// The import batch it came from.
    pub batch_id: String,
    /// When SAIL stored it.
    pub ingested_at: DateTime<Utc>,
    /// What the broker said.
    pub snapshot: BrokerSnapshot,
}

/// Which records a projection may see. Both bounds apply: an entry must have happened by
/// `occurred_by` **and** have been known to SAIL by `known_by`. Historical questions set
/// `known_by` so later imports can't leak into an earlier answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsOf {
    /// Latest event time included.
    pub occurred_by: DateTime<Utc>,
    /// Latest ingest time included.
    pub known_by: DateTime<Utc>,
}

impl AsOf {
    /// Everything known now.
    pub fn now() -> Self {
        let now = Utc::now();
        Self {
            occurred_by: now,
            known_by: now,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Helpers every slice uses the same way
// ---------------------------------------------------------------------------------------

/// Turn a spreadsheet's numeric cell into an exact decimal. Uses the shortest text that
/// round-trips the `f64` (Rust's `Display`), so `0.1` becomes `0.1`, not
/// `0.1000000000000000055…`. Returns `None` for NaN and infinities.
pub fn decimal_from_cell_f64(value: f64) -> Option<Decimal> {
    if !value.is_finite() {
        return None;
    }
    let text = format!("{value}");
    Decimal::from_str(&text)
        .or_else(|_| Decimal::from_scientific(&text))
        .ok()
}

/// Parse a number written as text in a report. Accepts `1234.56`, `1,234.56`, `1 234,56`,
/// `1.234,56`, `-12,5`, `(12.50)` for negatives, and a trailing currency code or `%` is
/// rejected (the caller strips known suffixes first). The decimal separator is the last
/// `.` or `,` that is followed by one to eight digits and nothing else; every other `.`,
/// `,`, space, NBSP or apostrophe is a grouping mark. Returns `None` when ambiguous or
/// malformed, never a guess.
pub fn parse_decimal_text(text: &str) -> Option<Decimal> {
    let mut t = text.trim().to_string();
    if t.is_empty() || t.len() > 64 {
        return None;
    }
    let mut negative = false;
    if t.starts_with('(') && t.ends_with(')') {
        negative = true;
        t = t[1..t.len() - 1].trim().to_string();
    }
    if let Some(rest) = t.strip_prefix('-').or_else(|| t.strip_prefix('\u{2212}')) {
        negative = !negative;
        t = rest.trim().to_string();
    } else if let Some(rest) = t.strip_prefix('+') {
        t = rest.trim().to_string();
    }
    let t: String = t
        .chars()
        .filter(|c| !matches!(c, ' ' | '\u{a0}' | '\u{202f}' | '\''))
        .collect();
    if t.is_empty()
        || !t
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
    {
        return None;
    }
    let seps: Vec<(usize, char)> = t
        .char_indices()
        .filter(|(_, c)| *c == '.' || *c == ',')
        .collect();
    let decimal_at = match seps.last() {
        None => None,
        Some(&(i, c)) => {
            let tail = &t[i + 1..];
            let same_kind = seps.iter().filter(|(_, s)| *s == c).count();
            if tail.is_empty() {
                return None;
            }
            // "1,234" or "1.234" alone: a single separator followed by exactly three
            // digits is ambiguous (thousands or decimals), so refuse it.
            if seps.len() == 1 && tail.len() == 3 && t[..i].len() <= 3 && t[..i] != *"0" {
                return None;
            }
            if same_kind > 1 {
                // "1.234.567": every mark is grouping.
                None
            } else if tail.len() <= 8 {
                Some(i)
            } else {
                return None;
            }
        }
    };
    let mut digits = String::with_capacity(t.len());
    for (i, c) in t.char_indices() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else if Some(i) == decimal_at {
            digits.push('.');
        }
    }
    let value = Decimal::from_str(&digits).ok()?;
    Some(if negative { -value } else { value })
}

/// Check an ISIN's shape and Luhn check digit. Invalid ISINs are dropped to `None` with an
/// issue rather than used as identity.
pub fn isin_is_valid(isin: &str) -> bool {
    let b = isin.as_bytes();
    if b.len() != 12
        || !b[..2].iter().all(u8::is_ascii_uppercase)
        || !b[2..11].iter().all(u8::is_ascii_alphanumeric)
        || !b[11].is_ascii_digit()
    {
        return false;
    }
    let mut digits = Vec::with_capacity(24);
    for &c in b {
        if c.is_ascii_digit() {
            digits.push(c - b'0');
        } else {
            let v = c - b'A' + 10;
            digits.push(v / 10);
            digits.push(v % 10);
        }
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            let d = u32::from(d);
            if i % 2 == 1 {
                let x = d * 2;
                x / 10 + x % 10
            } else {
                d
            }
        })
        .sum();
    sum % 10 == 0
}

/// Cap untrusted cell text at [`MAX_CELL_CHARS`], drop control characters, and trim.
/// Returns the text and whether it was cut.
pub fn clean_cell_text(text: &str) -> (String, bool) {
    let mut out = String::new();
    let mut cut = false;
    for (n, c) in text.trim().chars().filter(|c| !c.is_control()).enumerate() {
        if n >= MAX_CELL_CHARS {
            cut = true;
            break;
        }
        out.push(c);
    }
    (out, cut)
}

/// Hex sha256 of some bytes. Used for file hashes and content-derived keys.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// A convenience for date-only rows: midnight of `date` in `tz`, in UTC. Falls back to
/// UTC midnight on the rare DST gap where local midnight doesn't exist.
pub fn date_only(date: NaiveDate, tz: chrono_tz::Tz) -> EventTime {
    use chrono::TimeZone;
    let naive = date.and_hms_opt(0, 0, 0).unwrap_or_default();
    let at = tz
        .from_local_datetime(&naive)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&naive));
    EventTime {
        at,
        precision: TimePrecision::DateOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn decimal_text_accepts_common_layouts() {
        assert_eq!(parse_decimal_text("1234.56"), Some(d("1234.56")));
        assert_eq!(parse_decimal_text("1,234.56"), Some(d("1234.56")));
        assert_eq!(parse_decimal_text("1 234,56"), Some(d("1234.56")));
        assert_eq!(parse_decimal_text("1.234,56"), Some(d("1234.56")));
        assert_eq!(parse_decimal_text("-12,5"), Some(d("-12.5")));
        assert_eq!(parse_decimal_text("(12.50)"), Some(d("-12.50")));
        assert_eq!(parse_decimal_text("1.234.567"), Some(d("1234567")));
        assert_eq!(parse_decimal_text("0.125"), Some(d("0.125")));
        assert_eq!(parse_decimal_text("12"), Some(d("12")));
    }

    #[test]
    fn decimal_text_refuses_ambiguity_and_junk() {
        assert_eq!(parse_decimal_text("1,234"), None);
        assert_eq!(parse_decimal_text("1.234"), None);
        assert_eq!(parse_decimal_text("12 EUR"), None);
        assert_eq!(parse_decimal_text(""), None);
        assert_eq!(parse_decimal_text("1e5"), None);
        assert_eq!(parse_decimal_text("12,"), None);
    }

    #[test]
    fn cell_floats_become_short_decimals() {
        assert_eq!(decimal_from_cell_f64(0.1), Some(d("0.1")));
        assert_eq!(decimal_from_cell_f64(-1234.5), Some(d("-1234.5")));
        assert_eq!(decimal_from_cell_f64(f64::NAN), None);
    }

    #[test]
    fn isin_check_digit() {
        assert!(isin_is_valid("US0378331005"));
        assert!(isin_is_valid("IE00B4L5Y983"));
        assert!(!isin_is_valid("US0378331006"));
        assert!(!isin_is_valid("us0378331005"));
    }

    #[test]
    fn account_numbers_are_masked() {
        assert_eq!(
            AccountId::new(Broker::Xtb, "51234567").as_str(),
            "xtb:…4567"
        );
        assert_eq!(AccountId::new(Broker::Xtb, "main").as_str(), "xtb:main");
        assert_eq!(AccountId::new(Broker::Xtb, "").as_str(), "xtb:default");
    }

    #[test]
    fn cfd_and_share_never_share_a_key() {
        let share = Instrument {
            symbol: "NVDA.US".into(),
            isin: Some("US67066G1040".into()),
            name: None,
            currency: Currency::parse("usd"),
            kind: InstrumentKind::Stock,
        };
        let cfd = Instrument {
            kind: InstrumentKind::Cfd,
            ..share.clone()
        };
        assert_ne!(share.key(), cfd.key());
    }

    #[test]
    fn money_serializes_amount_as_string() {
        let m = Money::new(d("12.30"), Currency::eur());
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["amount"], "12.30");
        assert_eq!(v["currency"], "EUR");
    }
}
