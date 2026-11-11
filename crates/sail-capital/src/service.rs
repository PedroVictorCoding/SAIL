//! Wires the importers, the store and the ledger together for the HTTP layer. Every method
//! is blocking (SQLite, spreadsheet parsing), so async callers run them on a blocking
//! thread.
use crate::contracts::*;
use crate::evidence::{Cik, EdgarApi, IssuerMatch, SourceError, WatchItem};
use crate::ledger::{self, DividendView, Projection};
use crate::store::EvidenceOutcome;
use crate::store::{
    AccountSummary, BatchStatus, CapitalStore, ConflictView, GrantView, RetractOutcome, StoreError,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Most import batches `/sync/status` returns.
pub const MAX_BATCHES_REPLY: u32 = 100;

/// Most ledger records one `/cashflows` reply carries.
pub const MAX_RECORDS_REPLY: usize = 5_000;

/// A stored ticker map younger than this is reused instead of fetched again.
pub const TICKER_MAP_MAX_AGE_HOURS: i64 = 24;

/// Default hours between background SEC refreshes (`$SAIL_SEC_REFRESH_HOURS`, 1–168).
pub const DEFAULT_SEC_REFRESH_HOURS: u64 = 12;

/// What the last SEC refresh did, for `/sec/status`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecStatus {
    /// Whether `SAIL_SEC_USER_AGENT` is set. Without it SAIL makes no SEC requests.
    pub configured: bool,
    /// When the last refresh finished.
    pub last_refresh_at: Option<DateTime<Utc>>,
    /// The last refresh's error, owner-safe, if any.
    pub last_error: Option<String>,
    /// Issuers asked about in the last refresh.
    pub issuers_checked: u32,
    /// Filings listed in the responses.
    pub filings_seen: u32,
    /// New evidence stored (including revisions).
    pub new_evidence: u32,
    /// When the ticker map in use was fetched.
    pub map_fetched_at: Option<DateTime<Utc>>,
}

/// One owned position and how it links to SEC.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchView {
    /// Ledger instrument key.
    pub instrument_key: String,
    /// Broker symbol.
    pub symbol: String,
    /// The link.
    #[serde(rename = "match")]
    pub matched: IssuerMatch,
}

/// The `/filings` reply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilingsView {
    /// Filings that touch a holding, newest first.
    pub items: Vec<WatchItem>,
    /// How each owned position links to SEC.
    pub matches: Vec<MatchView>,
    /// The bounds used.
    pub as_of: AsOf,
}

/// Why a request failed. `BadRequest` messages are safe to show the owner as they are.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// The owner's input can't be used.
    #[error("{0}")]
    BadRequest(String),
    /// No such account, or there's no account yet and none was named.
    #[error("{0}")]
    NotFound(String),
    /// The file couldn't be read as a report.
    #[error(transparent)]
    Import(#[from] ImportError),
    /// Storage failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The Capital service: one store, used through the importers, the ledger and the
/// research sources.
pub struct CapitalService {
    store: std::sync::Arc<CapitalStore>,
    edgar: std::sync::Arc<dyn EdgarApi>,
    sec_configured: bool,
    sec_status: std::sync::Mutex<SecStatus>,
    sec_refreshing: tokio::sync::Mutex<()>,
    prices: std::sync::Arc<dyn crate::market::PriceSource>,
    prices_configured: bool,
    price_status: std::sync::Mutex<PriceStatus>,
    price_refreshing: tokio::sync::Mutex<()>,
    /// Model signals the owner turned on (`SAIL_MODEL_SIGNALS`).
    model_specs: Vec<crate::models::ModelSpec>,
    /// One model refresh at a time.
    model_refreshing: tokio::sync::Mutex<()>,
    /// The last model refresh, per model name.
    model_runs: std::sync::Mutex<std::collections::BTreeMap<String, ModelRun>>,
    evaluation_cache: std::sync::Mutex<Option<(String, crate::forecast::EvalReport)>>,
    /// The signal report, cached like the evaluation.
    signals_cache: std::sync::Mutex<Option<(String, crate::signals::SignalReport)>>,
}

/// Where the ledger lives: `$SAIL_CAPITAL_DIR`, or else SAIL's data dir `capital/`.
pub fn default_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SAIL_CAPITAL_DIR") {
        return dir.into();
    }
    sail_common::storage::data_dir("capital")
}

impl CapitalService {
    /// Opens the store in `dir`, with the real SEC client (configured from
    /// `$SAIL_SEC_USER_AGENT`).
    pub fn open(dir: &Path) -> Result<Self, ServiceError> {
        let client = crate::sec::EdgarClient::from_env();
        let configured = client.is_configured();
        let mut service = Self::with_edgar(dir, std::sync::Arc::new(client), configured)?;
        let (prices, prices_configured) = crate::market::source_from_env();
        service.prices = prices;
        service.prices_configured = prices_configured;
        service.model_specs = crate::models::enabled_from_env();
        service.price_status = std::sync::Mutex::new(PriceStatus {
            configured: prices_configured,
            ..PriceStatus::default()
        });
        Ok(service)
    }

    /// Replaces the price source (tests pass a fake).
    pub fn with_prices(
        mut self,
        prices: std::sync::Arc<dyn crate::market::PriceSource>,
        configured: bool,
    ) -> Self {
        self.prices = prices;
        self.prices_configured = configured;
        self.price_status = std::sync::Mutex::new(PriceStatus {
            configured,
            ..PriceStatus::default()
        });
        self
    }

    /// Opens the store in `dir` with a given SEC source (tests pass a fake).
    pub fn with_edgar(
        dir: &Path,
        edgar: std::sync::Arc<dyn EdgarApi>,
        sec_configured: bool,
    ) -> Result<Self, ServiceError> {
        Ok(Self {
            store: std::sync::Arc::new(CapitalStore::open(dir)?),
            edgar,
            sec_configured,
            sec_status: std::sync::Mutex::new(SecStatus {
                configured: sec_configured,
                ..SecStatus::default()
            }),
            sec_refreshing: tokio::sync::Mutex::new(()),
            prices: std::sync::Arc::new(crate::market::DisabledSource),
            prices_configured: false,
            price_status: std::sync::Mutex::new(PriceStatus::default()),
            price_refreshing: tokio::sync::Mutex::new(()),
            model_specs: Vec::new(),
            model_refreshing: tokio::sync::Mutex::new(()),
            model_runs: std::sync::Mutex::new(Default::default()),
            evaluation_cache: std::sync::Mutex::new(None),
            signals_cache: std::sync::Mutex::new(None),
        })
    }

    /// Imports one XTB report.
    ///
    /// - `account_label` names the account for files that don't name one themselves.
    /// - `tz` is an IANA zone name for the report's local times (default Europe/Lisbon).
    /// - `currency` is the account currency to use when the file doesn't state one. The
    ///   owner confirms it; it is never assumed.
    pub fn import_xtb(
        &self,
        account_label: Option<&str>,
        file_name: &str,
        bytes: &[u8],
        tz: Option<&str>,
        currency: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<BatchStatus, ServiceError> {
        if bytes.is_empty() {
            return Err(ServiceError::BadRequest("The file is empty.".into()));
        }
        let lower = file_name.to_ascii_lowercase();
        if !(lower.ends_with(".xlsx") || lower.ends_with(".csv")) {
            return Err(ServiceError::BadRequest(
                "Upload the XTB export as .xlsx or .csv.".into(),
            ));
        }
        let mut options = ImportOptions::default();
        if let Some(tz) = tz.filter(|t| !t.is_empty()) {
            options.report_tz = tz
                .parse()
                .map_err(|_| ServiceError::BadRequest(format!("Unknown time zone {tz:?}.")))?;
        }
        if let Some(code) = currency.filter(|c| !c.is_empty()) {
            options.default_currency = Some(Currency::parse(code).ok_or_else(|| {
                ServiceError::BadRequest(format!("Unknown currency code {code:?}."))
            })?);
        }
        let report = crate::import::parse(Broker::Xtb, bytes, file_name, &options)?;
        // The account the file names always wins, so a report can't be booked to the
        // wrong account by a stale selection. `account_label` (the page's selected
        // account) only fills in for files that don't name one.
        let label = report
            .account_label
            .as_deref()
            .filter(|l| !l.trim().is_empty())
            .or(account_label.filter(|l| !l.trim().is_empty()))
            .unwrap_or("default");
        // A stored account id passed back as-is ("xtb:…4567") names that account; anything
        // else is a label that gets masked.
        let account = match self
            .store
            .accounts()?
            .into_iter()
            .map(|a| a.account)
            .find(|a| a.as_str() == label)
        {
            Some(existing) => existing,
            None => AccountId::new(Broker::Xtb, label),
        };
        Ok(self
            .store
            .record_import(&account, file_name, bytes, &report, now)?)
    }

    /// Every known account.
    pub fn accounts(&self) -> Result<Vec<AccountSummary>, ServiceError> {
        Ok(self.store.accounts()?)
    }

    /// Resolves the `account` query value. With none given and exactly one account
    /// stored, that one is used.
    pub fn resolve_account(&self, account: Option<&str>) -> Result<AccountId, ServiceError> {
        let accounts = self.store.accounts()?;
        match account.filter(|a| !a.is_empty()) {
            Some(id) => accounts
                .into_iter()
                .map(|a| a.account)
                .find(|a| a.as_str() == id)
                .ok_or_else(|| ServiceError::NotFound(format!("No account {id:?}."))),
            None => match accounts.as_slice() {
                [only] => Ok(only.account.clone()),
                [] => Err(ServiceError::NotFound(
                    "No portfolio yet. Import an XTB report first.".into(),
                )),
                _ => Err(ServiceError::BadRequest(
                    "More than one account exists; name one with ?account=.".into(),
                )),
            },
        }
    }

    /// The ledger as seen at `as_of`.
    pub fn portfolio(&self, account: &AccountId, as_of: AsOf) -> Result<Projection, ServiceError> {
        let records = self.ledger_records(account, as_of.known_by)?;
        let snapshots = self.store.snapshots(account)?;
        Ok(ledger::project(account, &records, &snapshots, as_of))
    }

    /// Imported records plus the owner's grants, as SAIL knew them at `known_by`: a grant
    /// retracted by then is left out. The ledger applies the rest of the `AsOf` bound.
    fn ledger_records(
        &self,
        account: &AccountId,
        known_by: DateTime<Utc>,
    ) -> Result<Vec<LedgerRecord>, ServiceError> {
        let mut records = self.store.records(account)?;
        for grant in self.store.grants(account)? {
            if grant.retracted_at.is_some_and(|at| at <= known_by) {
                continue;
            }
            records.push(grant_record(account, &grant));
        }
        Ok(records)
    }

    /// Portfolio insights at `as_of`, with `now` for freshness and trailing windows.
    pub fn insights(
        &self,
        account: &AccountId,
        as_of: AsOf,
        now: DateTime<Utc>,
    ) -> Result<crate::insights::Insights, ServiceError> {
        let records = self.ledger_records(account, as_of.known_by)?;
        let snapshots = self.store.snapshots(account)?;
        let projection = ledger::project(account, &records, &snapshots, as_of);
        let visible: Vec<LedgerRecord> = records
            .into_iter()
            .filter(|r| r.entry.occurred.at <= as_of.occurred_by)
            .filter(|r| r.ingested_at <= as_of.known_by)
            .collect();
        Ok(crate::insights::insights(&projection, &visible, now))
    }

    /// The owner's grants, newest first, retracted ones included.
    pub fn grants(&self, account: &AccountId) -> Result<Vec<GrantView>, ServiceError> {
        Ok(self.store.grants(account)?)
    }

    /// Records units the owner received without buying them.
    pub fn record_grant(
        &self,
        account: &AccountId,
        input: GrantInput,
        now: DateTime<Utc>,
    ) -> Result<GrantView, ServiceError> {
        let symbol = input.symbol.trim().to_ascii_uppercase();
        let symbol_ok = !symbol.is_empty()
            && symbol.len() <= MAX_GRANT_SYMBOL_CHARS
            && symbol
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        if !symbol_ok {
            return Err(ServiceError::BadRequest(
                "The symbol must be the broker's ticker, e.g. GALP.PT.".into(),
            ));
        }
        let kind = match input.kind.as_deref().unwrap_or("stock") {
            "stock" => InstrumentKind::Stock,
            "etf" => InstrumentKind::Etf,
            _ => {
                return Err(ServiceError::BadRequest(
                    "The kind must be \"stock\" or \"etf\".".into(),
                ));
            }
        };
        let quantity = parse_decimal_text(&input.quantity)
            .filter(|q| *q > rust_decimal::Decimal::ZERO)
            .ok_or_else(|| ServiceError::BadRequest("The quantity must be positive.".into()))?;
        let received_at = parse_received_at(&input.received_at).ok_or_else(|| {
            ServiceError::BadRequest(
                "The date received must be YYYY-MM-DD or an RFC 3339 time.".into(),
            )
        })?;
        if received_at > now {
            return Err(ServiceError::BadRequest(
                "The date received can't be in the future.".into(),
            ));
        }
        let cost = match input.cost {
            None => None,
            Some(cost) => {
                let amount = parse_decimal_text(&cost.amount)
                    .filter(|a| *a >= rust_decimal::Decimal::ZERO)
                    .ok_or_else(|| {
                        ServiceError::BadRequest("The cost must be zero or more.".into())
                    })?;
                let currency = Currency::parse(&cost.currency).ok_or_else(|| {
                    ServiceError::BadRequest("The cost needs a three-letter currency.".into())
                })?;
                Some(Money::new(amount, currency))
            }
        };
        let note = input
            .note
            .map(|n| clean_cell_text(&n).0)
            .map(|n| n.chars().take(MAX_GRANT_NOTE_CHARS).collect::<String>())
            .filter(|n| !n.is_empty());
        let id = sha256_hex(
            format!(
                "{}\n{symbol}\n{quantity}\n{}\n{}",
                account.as_str(),
                received_at.to_rfc3339(),
                now.to_rfc3339()
            )
            .as_bytes(),
        )[..16]
            .to_string();
        let grant = GrantView {
            id,
            symbol,
            kind,
            quantity,
            received_at,
            cost,
            note,
            recorded_at: now,
            retracted_at: None,
        };
        self.store.record_grant(account, &grant)?;
        Ok(grant)
    }

    /// Retracts a grant. Retracting twice is refused.
    pub fn retract_grant(
        &self,
        account: &AccountId,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<GrantView, ServiceError> {
        match self.store.retract_grant(account, id, now)? {
            RetractOutcome::Retracted(grant) => Ok(grant),
            RetractOutcome::AlreadyRetracted(_) => Err(ServiceError::BadRequest(
                "This grant was already retracted.".into(),
            )),
            RetractOutcome::NotFound => Err(ServiceError::NotFound(format!("No grant {id:?}."))),
        }
    }

    /// Ledger records with `occurred.at` in `[from, to)`, oldest first, capped at
    /// [`MAX_RECORDS_REPLY`].
    pub fn cashflows(
        &self,
        account: &AccountId,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<Vec<LedgerRecord>, ServiceError> {
        let mut records: Vec<LedgerRecord> = self
            .store
            .records(account)?
            .into_iter()
            .filter(|r| from.is_none_or(|f| r.entry.occurred.at >= f))
            .filter(|r| to.is_none_or(|t| r.entry.occurred.at < t))
            .collect();
        records.sort_by(|a, b| {
            (a.entry.occurred.at, &a.entry.natural_key)
                .cmp(&(b.entry.occurred.at, &b.entry.natural_key))
        });
        records.truncate(MAX_RECORDS_REPLY);
        Ok(records)
    }

    /// Dividends paid in `[from, to)`, taken from the current projection.
    pub fn dividends(
        &self,
        account: &AccountId,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> Result<Vec<DividendView>, ServiceError> {
        let projection = self.portfolio(account, AsOf::now())?;
        Ok(projection
            .dividends
            .into_iter()
            .filter(|d| from.is_none_or(|f| d.paid.at >= f))
            .filter(|d| to.is_none_or(|t| d.paid.at < t))
            .collect())
    }

    /// Import history, newest first.
    pub fn batches(&self, account: Option<&AccountId>) -> Result<Vec<BatchStatus>, ServiceError> {
        Ok(self.store.batches(account, MAX_BATCHES_REPLY)?)
    }

    /// Open conflicts for `account`.
    pub fn conflicts(&self, account: &AccountId) -> Result<Vec<ConflictView>, ServiceError> {
        Ok(self.store.conflicts(account)?)
    }

    /// How each owned position links to an issuer, using the ticker map SAIL had at
    /// `known_by`.
    fn matches(
        &self,
        projection: &Projection,
        known_by: DateTime<Utc>,
    ) -> Result<Vec<MatchView>, ServiceError> {
        let index = self.store.ticker_index(known_by)?;
        Ok(projection
            .positions
            .iter()
            .map(|p| MatchView {
                instrument_key: p.instrument.key(),
                symbol: p.instrument.symbol.clone(),
                matched: match &index {
                    Some(index) => crate::exposure::match_issuer(&p.instrument, index),
                    None => IssuerMatch::Unresolved {
                        reason: "SEC's ticker map hasn't been fetched yet".to_string(),
                    },
                },
            })
            .collect())
    }

    /// The filings that touch `account`'s holdings, as known at `as_of`.
    pub fn filings(&self, account: &AccountId, as_of: AsOf) -> Result<FilingsView, ServiceError> {
        let projection = self.portfolio(account, as_of)?;
        let matches = self.matches(&projection, as_of.known_by)?;
        let mut ciks: Vec<Cik> = matches
            .iter()
            .filter_map(|m| match &m.matched {
                IssuerMatch::Matched { cik, .. } => Some(*cik),
                _ => None,
            })
            .collect();
        ciks.sort();
        ciks.dedup();
        let evidence = self.store.evidence_for(&ciks)?;
        let pairs: Vec<(String, IssuerMatch)> = matches
            .iter()
            .map(|m| (m.instrument_key.clone(), m.matched.clone()))
            .collect();
        let items = crate::exposure::watch_items(&projection, &pairs, &evidence, as_of);
        Ok(FilingsView {
            items,
            matches,
            as_of,
        })
    }

    /// The last refresh's status.
    pub fn sec_status(&self) -> SecStatus {
        self.sec_status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Asks SEC about the issuers `account` holds now (every account when `None`) and
    /// appends what it lists. Only one refresh runs at a time; a second caller waits and
    /// then runs its own.
    pub async fn refresh_sec(&self, account: Option<&AccountId>) -> SecStatus {
        let _running = self.sec_refreshing.lock().await;
        let now = Utc::now();
        let mut status = SecStatus {
            configured: self.sec_configured,
            map_fetched_at: self.sec_status().map_fetched_at,
            ..SecStatus::default()
        };
        if !self.sec_configured {
            status.last_error = Some(SourceError::NotConfigured.to_string());
            return self.finish(status, now);
        }
        match self.refresh_inner(account, now, &mut status).await {
            Ok(()) => {}
            Err(message) => status.last_error = Some(message),
        }
        self.finish(status, now)
    }

    fn finish(&self, mut status: SecStatus, now: DateTime<Utc>) -> SecStatus {
        status.last_refresh_at = Some(now);
        if let Ok(mut slot) = self.sec_status.lock() {
            *slot = status.clone();
        }
        status
    }

    async fn refresh_inner(
        &self,
        account: Option<&AccountId>,
        now: DateTime<Utc>,
        status: &mut SecStatus,
    ) -> Result<(), String> {
        // 1. The ticker map, reused while it is fresh. A fetched map is used as fetched: its
        // `fetched_at` is stamped during the request, after `now`.
        let stored = self.store.ticker_index(now).map_err(|e| e.to_string())?;
        let index = match stored {
            Some(i) if now - i.fetched_at < Duration::hours(TICKER_MAP_MAX_AGE_HOURS) => i,
            _ => {
                let index = self.edgar.ticker_index().await.map_err(|e| e.to_string())?;
                self.store
                    .record_ticker_index(&index)
                    .map_err(|e| e.to_string())?;
                index
            }
        };
        status.map_fetched_at = Some(index.fetched_at);

        // 2. The issuers held now.
        let accounts = match account {
            Some(a) => vec![a.clone()],
            None => self
                .accounts()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|a| a.account)
                .collect(),
        };
        let mut ciks: Vec<Cik> = Vec::new();
        for account in &accounts {
            let projection = self
                .portfolio(account, AsOf::now())
                .map_err(|e| e.to_string())?;
            for position in &projection.positions {
                if let IssuerMatch::Matched { cik, .. } =
                    crate::exposure::match_issuer(&position.instrument, &index)
                {
                    ciks.push(cik);
                }
            }
        }
        ciks.sort();
        ciks.dedup();
        ciks.truncate(crate::evidence::MAX_ISSUERS_PER_REFRESH);

        // 3. Each issuer's filings. A refusal stops the run so SAIL backs off; other
        //    errors are recorded and the next issuer is tried.
        let mut last_error = None;
        for cik in ciks {
            status.issuers_checked += 1;
            match self.edgar.issuer_filings(cik).await {
                Ok(filings) => {
                    let EvidenceOutcome { seen, added, .. } = self
                        .store
                        .record_filings(&filings, Utc::now())
                        .map_err(|e| e.to_string())?;
                    status.filings_seen += seen;
                    status.new_evidence += added;
                }
                Err(SourceError::Refused(code)) => {
                    return Err(SourceError::Refused(code).to_string());
                }
                Err(e) => last_error = Some(format!("CIK {cik}: {e}")),
            }
        }
        match last_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Starts the background SEC refresh when SEC is configured: every
    /// `$SAIL_SEC_REFRESH_HOURS` (1–168, default 12), first run a minute after start.
    pub fn start(self: &std::sync::Arc<Self>) {
        if !self.sec_configured {
            tracing::info!("SEC refresh off: SAIL_SEC_USER_AGENT is not set");
            return;
        }
        let hours = std::env::var("SAIL_SEC_REFRESH_HOURS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|h| (1..=168).contains(h))
            .unwrap_or(DEFAULT_SEC_REFRESH_HOURS);
        let service = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            loop {
                let status = service.refresh_sec(None).await;
                tracing::info!(
                    issuers = status.issuers_checked,
                    new = status.new_evidence,
                    error = status.last_error.as_deref().unwrap_or(""),
                    "SEC refresh finished"
                );
                tokio::time::sleep(std::time::Duration::from_secs(hours * 3600)).await;
            }
        });
    }
}

/// Longest symbol a grant accepts.
const MAX_GRANT_SYMBOL_CHARS: usize = 32;

/// Longest note kept on a grant, in characters.
const MAX_GRANT_NOTE_CHARS: usize = 200;

/// What the owner sends to record a grant (`POST /grants`).
#[derive(Clone, Debug, Deserialize)]
pub struct GrantInput {
    /// Broker symbol, e.g. `"GALP.PT"`.
    pub symbol: String,
    /// `"stock"` (default) or `"etf"`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Units received, as a decimal string.
    pub quantity: String,
    /// `YYYY-MM-DD` (start of that day, UTC) or an RFC 3339 time.
    pub received_at: String,
    /// Cost basis; absent or null means zero.
    #[serde(default)]
    pub cost: Option<GrantCostInput>,
    /// A note, e.g. "promotional campaign".
    #[serde(default)]
    pub note: Option<String>,
}

/// A stated cost, with amounts as strings like everywhere else.
#[derive(Clone, Debug, Deserialize)]
pub struct GrantCostInput {
    /// Decimal amount.
    pub amount: String,
    /// Three-letter currency.
    pub currency: String,
}

/// Reads `YYYY-MM-DD` as the start of that UTC day, or an RFC 3339 time.
fn parse_received_at(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim();
    if let Ok(date) = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return date.and_hms_opt(0, 0, 0).map(|t| t.and_utc());
    }
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// The ledger row a grant stands for. Its natural key sorts before XTB's on the same
/// instant, so units received at a time are held before a sale at that time.
fn grant_record(account: &AccountId, grant: &GrantView) -> LedgerRecord {
    let date_only = grant.received_at.time() == chrono::NaiveTime::MIN;
    LedgerRecord {
        id: format!("grant-{}", grant.id),
        account: account.clone(),
        batch_id: "owner-grants".to_string(),
        ingested_at: grant.recorded_at,
        entry: ParsedEntry {
            natural_key: format!("owner:grant:{}", grant.id),
            source: RowRef {
                sheet: None,
                sheet_kind: SheetKind::Unknown,
                row: 0,
            },
            broker_ref: None,
            occurred: EventTime {
                at: grant.received_at,
                precision: if date_only {
                    TimePrecision::DateOnly
                } else {
                    TimePrecision::Exact
                },
            },
            kind: EntryKind::Grant {
                instrument: Instrument {
                    symbol: grant.symbol.clone(),
                    isin: None,
                    name: None,
                    currency: None,
                    kind: grant.kind,
                },
                quantity: grant.quantity,
                cost: grant.cost.clone(),
            },
            cash_effect: None,
            comment: grant.note.clone(),
        },
    }
}

// ---------------------------------------------------------------------------------------
// Prices and forecasts (round 10)
// ---------------------------------------------------------------------------------------

/// Most observations one `/prices` reply returns.
pub const MAX_PRICES_REPLY: usize = 10_000;

/// The price refresh state, for `/prices/status`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceStatus {
    /// True when the owner opted in to fetching.
    pub configured: bool,
    /// Source id when configured.
    pub source: Option<String>,
    /// When the last refresh ran.
    pub last_refresh_at: Option<DateTime<Utc>>,
    /// Why the last refresh stopped, if it did.
    pub last_error: Option<String>,
    /// Instruments with stored prices.
    pub instruments: u32,
    /// Distinct stored sessions over all instruments.
    pub observations: u32,
    /// Per instrument.
    pub per_instrument: Vec<crate::store::PriceCoverage>,
    /// Held instruments the source can't name (CFDs, unmapped listings), by symbol.
    pub unmapped: Vec<String>,
}

/// What importing one price file did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceImport {
    /// Broker symbol.
    pub symbol: String,
    /// Ledger instrument key the prices were stored under.
    pub instrument_key: String,
    /// Bars read.
    pub sessions_seen: u32,
    /// New sessions.
    pub added: u32,
    /// Changed sessions.
    pub revisions: u32,
    /// First and last session in the file.
    pub first: Option<chrono::NaiveDate>,
    /// Last session in the file.
    pub last: Option<chrono::NaiveDate>,
}

/// Forecasts for the held instruments at one horizon and model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForecastsView {
    /// When SAIL computed them.
    pub as_of: DateTime<Utc>,
    /// Horizon in sessions.
    pub horizon_sessions: u32,
    /// The model.
    pub model: crate::forecast::ModelId,
    /// One per held owned instrument with prices.
    pub forecasts: Vec<crate::forecast::ForecastDistribution>,
    /// Held owned instruments with no stored prices, by symbol.
    pub without_prices: Vec<String>,
}

impl CapitalService {
    /// Held owned instruments: (instrument key, symbol). CFDs are never forecast.
    fn held_instruments(&self, account: &AccountId) -> Result<Vec<(String, String)>, ServiceError> {
        let projection = self.portfolio(account, AsOf::now())?;
        let mut held: Vec<(String, String)> = projection
            .positions
            .iter()
            .map(|p| (p.instrument.key(), p.instrument.symbol.clone()))
            .collect();
        held.sort();
        held.dedup();
        Ok(held)
    }

    /// The current price status.
    pub fn price_status(&self) -> Result<PriceStatus, ServiceError> {
        let mut status = self
            .price_status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default();
        status.configured = self.prices_configured;
        status.source = self
            .prices_configured
            .then(|| self.prices.source_id().to_string());
        status.per_instrument = self.store.price_coverage()?;
        status.instruments = status.per_instrument.len() as u32;
        status.observations = status.per_instrument.iter().map(|c| c.sessions).sum();
        Ok(status)
    }

    /// Fetches daily prices for every held owned instrument the source can name. Makes
    /// no request unless the owner opted in; stops at the first refusal.
    pub async fn refresh_prices(
        &self,
        account: Option<&AccountId>,
    ) -> Result<PriceStatus, ServiceError> {
        let _running = self.price_refreshing.lock().await;
        let now = Utc::now();
        let mut last_error = None;
        let mut unmapped = Vec::new();
        if !self.prices_configured {
            last_error = Some(crate::market::PriceSourceError::NotConfigured.to_string());
        } else {
            let accounts = match account {
                Some(a) => vec![a.clone()],
                None => self.accounts()?.into_iter().map(|a| a.account).collect(),
            };
            let mut held: Vec<(String, String)> = Vec::new();
            for a in &accounts {
                held.extend(self.held_instruments(a)?);
            }
            held.sort();
            held.dedup();
            held.truncate(crate::market::MAX_INSTRUMENTS_PER_REFRESH);
            for (key, symbol) in held {
                match self.prices.daily(&symbol).await {
                    Ok(series) => {
                        let store = self.store.clone();
                        tokio::task::spawn_blocking(move || {
                            store.record_prices(&key, &series, Utc::now())
                        })
                        .await
                        .map_err(|_| ServiceError::Store(crate::store::StoreError::Poisoned))?
                        .map_err(ServiceError::from)?;
                    }
                    Err(crate::market::PriceSourceError::UnknownSymbol(_)) => unmapped.push(symbol),
                    Err(
                        e @ (crate::market::PriceSourceError::Refused(_)
                        | crate::market::PriceSourceError::Blocked),
                    ) => {
                        last_error = Some(e.to_string());
                        break;
                    }
                    Err(e) => last_error = Some(format!("{symbol}: {e}")),
                }
            }
        }
        if let Ok(mut status) = self.price_status.lock() {
            status.last_refresh_at = Some(now);
            status.last_error = last_error;
            status.unmapped = unmapped;
        }
        self.price_status()
    }

    /// Stores an owner-uploaded price file for `symbol`. The prices go under the held
    /// instrument with that symbol, or `sym:<SYMBOL>` when none is held.
    pub fn import_price_file(
        &self,
        account: Option<&AccountId>,
        symbol: &str,
        bytes: &[u8],
        currency: Option<&str>,
        adjustment: crate::market::Adjustment,
        now: DateTime<Utc>,
    ) -> Result<PriceImport, ServiceError> {
        let symbol = symbol.trim().to_ascii_uppercase();
        if symbol.is_empty() || symbol.len() > 32 {
            return Err(ServiceError::BadRequest(
                "Name the symbol the prices are for.".into(),
            ));
        }
        if bytes.len() > crate::market::MAX_PRICE_FILE_BYTES {
            return Err(ServiceError::BadRequest(
                "The price file is too large.".into(),
            ));
        }
        let currency = match currency.filter(|c| !c.is_empty()) {
            Some(c) => Some(Currency::parse(c).ok_or_else(|| {
                ServiceError::BadRequest("The currency must be a three-letter code.".into())
            })?),
            None => None,
        };
        let series = crate::market::parse_price_file(bytes, &symbol, currency, adjustment)
            .map_err(|e| ServiceError::BadRequest(e.to_string()))?;
        let held = match account {
            Some(a) => self.held_instruments(a)?,
            None => Vec::new(),
        };
        let key = held
            .into_iter()
            .find(|(_, s)| s.eq_ignore_ascii_case(&symbol))
            .map(|(k, _)| k)
            .unwrap_or_else(|| format!("sym:{symbol}"));
        let outcome = self.store.record_prices(&key, &series, now)?;
        Ok(PriceImport {
            symbol,
            instrument_key: key,
            sessions_seen: outcome.seen,
            added: outcome.added,
            revisions: outcome.revisions,
            first: series.bars.first().map(|b| b.session),
            last: series.bars.last().map(|b| b.session),
        })
    }

    /// Stored closes for `symbol` in `[from, to]`, as known now, at most
    /// [`MAX_PRICES_REPLY`].
    pub fn prices(
        &self,
        symbol: &str,
        from: Option<chrono::NaiveDate>,
        to: Option<chrono::NaiveDate>,
    ) -> Result<Vec<crate::market::MarketObservation>, ServiceError> {
        let symbol = symbol.trim();
        let Some(coverage) = self
            .store
            .price_coverage()?
            .into_iter()
            .find(|c| c.symbol.eq_ignore_ascii_case(symbol))
        else {
            return Ok(Vec::new());
        };
        let mut rows =
            self.store
                .prices_in_range(&coverage.instrument_key, Utc::now(), from, to)?;
        if rows.len() > MAX_PRICES_REPLY {
            rows.drain(..rows.len() - MAX_PRICES_REPLY);
        }
        Ok(rows)
    }

    /// Closes per held instrument as known at `known_by`: (key, symbol, closes).
    #[allow(clippy::type_complexity)]
    fn held_closes(
        &self,
        account: &AccountId,
        known_by: DateTime<Utc>,
    ) -> Result<
        Vec<(
            String,
            String,
            Vec<(chrono::NaiveDate, rust_decimal::Decimal)>,
        )>,
        ServiceError,
    > {
        let held = self.held_instruments(account)?;
        self.closes_for(&held, known_by)
    }

    /// Closes for an already resolved held set, so callers that need the set for a cache
    /// key don't project the portfolio twice.
    #[allow(clippy::type_complexity)]
    fn closes_for(
        &self,
        held: &[(String, String)],
        known_by: DateTime<Utc>,
    ) -> Result<
        Vec<(
            String,
            String,
            Vec<(chrono::NaiveDate, rust_decimal::Decimal)>,
        )>,
        ServiceError,
    > {
        let mut out = Vec::new();
        for (key, symbol) in held {
            let closes: Vec<_> = self
                .store
                .prices(key, known_by)?
                .into_iter()
                .map(|o| (o.session, o.close))
                .collect();
            out.push((key.clone(), symbol.clone(), closes));
        }
        Ok(out)
    }

    /// The walk-forward evaluation over held instruments, cached until prices or the held
    /// set change.
    pub fn evaluation(
        &self,
        account: &AccountId,
    ) -> Result<crate::forecast::EvalReport, ServiceError> {
        let held = self.held_instruments(account)?;
        let mut held_keys: Vec<&str> = held.iter().map(|(key, _)| key.as_str()).collect();
        held_keys.sort_unstable();
        let version = format!(
            "{}|{}|{}",
            account.as_str(),
            self.store.price_version()?,
            held_keys.join("|")
        );
        if let Ok(cache) = self.evaluation_cache.lock()
            && let Some((cached_version, report)) = cache.as_ref()
            && *cached_version == version
        {
            return Ok(report.clone());
        }
        let data = self.closes_for(&held, Utc::now())?;
        let series: Vec<crate::forecast::CloseSeries<'_>> = data
            .iter()
            .filter(|(_, _, closes)| !closes.is_empty())
            .map(|(key, symbol, closes)| crate::forecast::CloseSeries {
                instrument_key: key,
                symbol,
                closes,
            })
            .collect();
        let report = crate::forecast::evaluate(&series, &crate::forecast::EvalConfig::default());
        if let Ok(mut cache) = self.evaluation_cache.lock() {
            *cache = Some((version, report.clone()));
        }
        Ok(report)
    }

    /// Forecasts at `horizon` with `model` for every held owned instrument with prices.
    /// Calibration comes from the latest evaluation.
    pub fn forecasts(
        &self,
        account: &AccountId,
        horizon: u32,
        model: crate::forecast::ModelId,
    ) -> Result<ForecastsView, ServiceError> {
        if !crate::forecast::HORIZONS.contains(&horizon) {
            return Err(ServiceError::BadRequest(format!(
                "The horizon must be one of {:?} sessions.",
                crate::forecast::HORIZONS
            )));
        }
        let evaluation = self.evaluation(account)?;
        let calibration = evaluation
            .scores
            .iter()
            .find(|s| s.model == model && s.horizon_sessions == horizon)
            .map(|s| crate::forecast::CalibrationState {
                verified: s.calibrated,
                cohort: evaluation
                    .origins
                    .map(|(a, b)| format!("walk-forward {a}..{b}, {} forecasts", s.forecasts)),
            })
            .unwrap_or(crate::forecast::CalibrationState {
                verified: false,
                cohort: None,
            });
        let mut forecasts = Vec::new();
        let mut without_prices = Vec::new();
        for (key, symbol, closes) in self.held_closes(account, Utc::now())? {
            if closes.is_empty() {
                without_prices.push(symbol);
                continue;
            }
            let series = crate::forecast::CloseSeries {
                instrument_key: &key,
                symbol: &symbol,
                closes: &closes,
            };
            forecasts.push(crate::forecast::forecast(
                &series,
                model,
                horizon,
                calibration.clone(),
            ));
        }
        Ok(ForecastsView {
            as_of: Utc::now(),
            horizon_sessions: horizon,
            model,
            forecasts,
            without_prices,
        })
    }
}

/// `GET /policy`: the policy in force and every stored version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyView {
    /// The latest stored version, or the built-in draft (version 0).
    pub current: crate::policy::RiskPolicy,
    /// Stored versions, newest first.
    pub history: Vec<crate::policy::RiskPolicy>,
}

/// Horizon, in sessions, that ideas are formed at: about a quarter, so trading costs
/// weigh less against the expected gain.
pub const IDEA_HORIZON: u32 = 60;

/// XTB suffixes whose listings trade outside the euro.
const NON_EURO_SUFFIXES: [&str; 7] = ["US", "UK", "CH", "DK", "SE", "NO", "PL"];

impl CapitalService {
    /// The policy in force and its history.
    pub fn policy(&self, account: &AccountId) -> Result<PolicyView, ServiceError> {
        let history = self.store.policies(account)?;
        let current = history
            .first()
            .cloned()
            .unwrap_or_else(crate::policy::RiskPolicy::builtin_draft);
        Ok(PolicyView { current, history })
    }

    /// Stores a new policy version, as a draft or confirmed.
    pub fn save_policy(
        &self,
        account: &AccountId,
        limits: &crate::policy::RiskLimits,
        confirm: bool,
        now: DateTime<Utc>,
    ) -> Result<crate::policy::RiskPolicy, ServiceError> {
        limits.validate().map_err(ServiceError::BadRequest)?;
        let status = if confirm {
            crate::policy::PolicyStatus::Confirmed
        } else {
            crate::policy::PolicyStatus::Draft
        };
        Ok(self.store.save_policy(account, limits, status, now)?)
    }

    /// Records a dismissal or "done" for a suggestion.
    pub fn suggestion_action(
        &self,
        account: &AccountId,
        id: &str,
        action: crate::suggest::Action,
        now: DateTime<Utc>,
    ) -> Result<crate::suggest::ActionRecord, ServiceError> {
        if id.len() != 16 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ServiceError::BadRequest("Unknown suggestion id.".into()));
        }
        let record = crate::suggest::ActionRecord {
            suggestion_id: id.to_string(),
            action,
            at: now,
        };
        self.store.record_suggestion_action(account, &record)?;
        Ok(record)
    }

    /// The signal report over held instruments, cached until prices or holdings change.
    pub fn signals(
        &self,
        account: &AccountId,
    ) -> Result<crate::signals::SignalReport, ServiceError> {
        let held = self.held_instruments(account)?;
        let mut held_keys: Vec<&str> = held.iter().map(|(key, _)| key.as_str()).collect();
        held_keys.sort_unstable();
        let model_version: u64 = self
            .store
            .signal_value_coverage()?
            .iter()
            .map(|c| u64::from(c.values))
            .sum();
        let version = format!(
            "{}|{}|{}|{}",
            account.as_str(),
            self.store.price_version()?,
            model_version,
            held_keys.join("|")
        );
        if let Ok(cache) = self.signals_cache.lock()
            && let Some((cached, report)) = cache.as_ref()
            && *cached == version
        {
            return Ok(report.clone());
        }
        let data = self.closes_for(&held, Utc::now())?;
        let series: Vec<crate::forecast::CloseSeries<'_>> = data
            .iter()
            .filter(|(_, _, closes)| !closes.is_empty())
            .map(|(key, symbol, closes)| crate::forecast::CloseSeries {
                instrument_key: key,
                symbol,
                closes,
            })
            .collect();
        let sigma = |key: &str, horizon: u32| -> Option<f64> {
            let s = series.iter().find(|s| s.instrument_key == key)?;
            ewma_sigma(s, horizon)
        };
        let external = self.external_scores(&held_keys)?;
        let report = crate::signals::evaluate_and_combine(
            &series,
            &external,
            &sigma,
            &crate::signals::SignalEvalConfig::default(),
        );
        if let Ok(mut cache) = self.signals_cache.lock() {
            *cache = Some((version, report.clone()));
        }
        Ok(report)
    }

    /// Everything the Today view shows.
    pub fn today(
        &self,
        account: &AccountId,
        now: DateTime<Utc>,
    ) -> Result<crate::suggest::Today, ServiceError> {
        use rust_decimal::prelude::ToPrimitive;
        let policy = self.policy(account)?.current;
        let handled = self.store.handled_suggestions(account)?;
        let insights = self.insights(account, AsOf::now(), now)?;
        let projection = self.portfolio(account, AsOf::now())?;
        let prices_configured = self.price_status()?.configured;

        let held = self.held_instruments(account)?;
        let data = self.closes_for(&held, now)?;
        let mut without_prices = Vec::new();
        let mut stale_prices = Vec::new();
        for (_, symbol, closes) in &data {
            match closes.last() {
                None => without_prices.push(symbol.clone()),
                Some((session, _))
                    if (now.date_naive() - *session).num_days()
                        > crate::suggest::build::STALE_PRICE_DAYS =>
                {
                    stale_prices.push((symbol.clone(), *session))
                }
                Some(_) => {}
            }
        }

        let new_filings = match self.filings(account, AsOf::now()) {
            Ok(view) => view
                .items
                .iter()
                .filter(|w| {
                    (now - w.first_observed_at).num_days() <= crate::suggest::build::NEW_FILING_DAYS
                })
                .map(|w| crate::suggest::build::NewFiling {
                    symbol: w.issuer_name.clone(),
                    form: w.filing.form.clone(),
                    accession: w.filing.accession.clone(),
                })
                .collect(),
            Err(_) => Vec::new(),
        };

        let report = self.signals(account)?;
        let combined_ic = report
            .combined_ic
            .iter()
            .find(|(h, _)| *h == IDEA_HORIZON)
            .map(|(_, ic)| *ic)
            .unwrap_or(0.0);
        let signals_passed: Vec<String> = report
            .skill
            .iter()
            .filter(|s| s.horizon_sessions == IDEA_HORIZON && s.ic_used != 0.0)
            .map(|s| {
                format!(
                    "{} h{} t={:.1}",
                    serde_json::to_value(s.signal)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default(),
                    s.horizon_sessions,
                    s.t_stat
                )
            })
            .collect();

        let mut policy_plan = None;
        let mut idea_plan = None;
        if let Some(account_view) = insights.account.as_ref()
            && let Some(account_value) = account_view.account_value.amount.to_f64()
            && account_value > 0.0
        {
            let free = account_view.free_funds.amount.to_f64().unwrap_or(0.0);
            let mut assets = Vec::new();
            let mut series_in_order = Vec::new();
            for slice in &insights.allocation {
                let closes = data
                    .iter()
                    .find(|(key, _, _)| *key == slice.instrument_key)
                    .map(|(_, _, c)| c.as_slice())
                    .unwrap_or(&[]);
                let series = crate::forecast::CloseSeries {
                    instrument_key: &slice.instrument_key,
                    symbol: &slice.symbol,
                    closes,
                };
                let sigma_annual = ewma_sigma(&series, 20).map(|s| s * (252.0f64 / 20.0).sqrt());
                let alpha = report
                    .latest
                    .iter()
                    .find(|c| {
                        c.instrument_key == slice.instrument_key
                            && c.horizon_sessions == IDEA_HORIZON
                    })
                    .map(|c| c.alpha)
                    .unwrap_or(0.0);
                let suffix = slice
                    .symbol
                    .rsplit_once('.')
                    .map(|(_, s)| s.to_ascii_uppercase())
                    .unwrap_or_default();
                assets.push(crate::optimize::Asset {
                    instrument_key: slice.instrument_key.clone(),
                    symbol: slice.symbol.clone(),
                    weight: slice.value.amount.to_f64().unwrap_or(0.0) / account_value,
                    alpha,
                    sigma_annual,
                    foreign_currency: NON_EURO_SUFFIXES.contains(&suffix.as_str()),
                    locked: closes.is_empty(),
                });
                series_in_order.push(series);
            }
            let problem = crate::optimize::Problem {
                assets,
                cash: free / account_value,
                covariance: crate::optimize::covariance(&series_in_order, IDEA_HORIZON),
                account_value,
                horizon_sessions: IDEA_HORIZON,
            };
            let plan =
                crate::optimize::plan(&problem, &policy.limits, crate::optimize::Mode::Policy);
            policy_plan = Some(plan);
            if policy.is_confirmed() && combined_ic > 0.0 {
                idea_plan = Some(crate::optimize::plan(
                    &problem,
                    &policy.limits,
                    crate::optimize::Mode::Ideas,
                ));
            }
        }

        let facts = crate::suggest::build::Facts {
            now,
            account: insights.account.clone(),
            policy,
            snapshot_as_of: insights.freshness.snapshot_as_of,
            variances: projection
                .reconciliation
                .variances
                .iter()
                .map(|v| v.subject.clone())
                .collect(),
            unvalued: insights.unvalued.clone(),
            prices_configured,
            without_prices,
            stale_prices,
            new_filings,
            policy_plan,
            idea_plan,
            idea_horizon: IDEA_HORIZON,
            signals_passed,
            combined_ic,
        };
        Ok(crate::suggest::build::build(facts, &handled))
    }
}

/// The `EwmaVolatility` sigma of the horizon log return, when the series supports one.
fn ewma_sigma(series: &crate::forecast::CloseSeries<'_>, horizon: u32) -> Option<f64> {
    if series.closes.is_empty() {
        return None;
    }
    let f = crate::forecast::forecast(
        series,
        crate::forecast::ModelId::EwmaVolatility,
        horizon,
        crate::forecast::CalibrationState {
            verified: false,
            cohort: None,
        },
    );
    f.sigma_log_return
}

/// The last run of one model signal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelRun {
    /// When it finished.
    pub at: DateTime<Utc>,
    /// Model version the helper reported.
    pub model_version: Option<String>,
    /// Values asked for.
    pub requested: u32,
    /// New values stored.
    pub added: u32,
    /// Origins the helper couldn't score.
    pub skipped: u32,
    /// Why it failed, if it did.
    pub error: Option<String>,
    /// The helper's notes.
    pub notes: Vec<String>,
}

/// `GET /signals/models`: what each model signal is and what is stored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelsStatus {
    /// A refresh is running now.
    pub running: bool,
    /// Every known model, whether on or off.
    pub models: Vec<ModelStatus>,
}

/// One model's status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelStatus {
    /// What it is.
    pub spec: crate::models::ModelSpec,
    /// Turned on in `SAIL_MODEL_SIGNALS`.
    pub enabled: bool,
    /// The last run, if any since SAIL started.
    pub last_run: Option<ModelRun>,
    /// What is stored.
    pub stored: Option<crate::store::SignalValueCoverage>,
}

impl CapitalService {
    /// Turns on model signals (the service reads `SAIL_MODEL_SIGNALS` in `open`).
    pub fn with_models(mut self, specs: Vec<crate::models::ModelSpec>) -> Self {
        self.model_specs = specs;
        self
    }

    fn signal_name(signal: crate::signals::SignalId) -> String {
        serde_json::to_value(signal)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// Stored values of the enabled models for `held` instruments, from each model's
    /// `evaluate_from` on.
    fn external_scores(
        &self,
        held: &[&str],
    ) -> Result<Vec<crate::signals::ExternalScores>, ServiceError> {
        let mut out = Vec::new();
        for spec in &self.model_specs {
            let (_, values) = self.store.signal_values(&Self::signal_name(spec.signal))?;
            let values: Vec<_> = values
                .into_iter()
                .filter(|(key, session, _)| {
                    *session >= spec.evaluate_from && held.contains(&key.as_str())
                })
                .collect();
            if !values.is_empty() {
                out.push(crate::signals::ExternalScores {
                    signal: spec.signal,
                    values,
                });
            }
        }
        Ok(out)
    }

    /// What each model signal is, whether it is on, and what is stored.
    pub fn models_status(&self) -> Result<ModelsStatus, ServiceError> {
        let coverage = self.store.signal_value_coverage()?;
        let runs = self
            .model_runs
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default();
        let models = crate::models::specs()
            .into_iter()
            .map(|spec| {
                let name = Self::signal_name(spec.signal);
                ModelStatus {
                    enabled: self.model_specs.iter().any(|s| s.name == spec.name),
                    last_run: runs.get(&spec.name).cloned(),
                    stored: coverage.iter().rfind(|c| c.signal == name).cloned(),
                    spec,
                }
            })
            .collect();
        Ok(ModelsStatus {
            running: self.model_refreshing.try_lock().is_err(),
            models,
        })
    }

    /// Runs every enabled model helper over the held instruments, for the origins not
    /// stored yet, and stores the values. Slow the first time (it downloads weights and
    /// scores years of weekly origins); later runs only add new weeks.
    pub async fn refresh_model_signals(
        &self,
        account: &AccountId,
    ) -> Result<ModelsStatus, ServiceError> {
        let _running = self.model_refreshing.lock().await;
        let python = std::env::var("SAIL_SIGNALS_PYTHON")
            .unwrap_or_else(|_| crate::models::DEFAULT_PYTHON.to_string());
        let scripts = std::env::var("SAIL_SIGNALS_SCRIPTS")
            .unwrap_or_else(|_| crate::models::DEFAULT_SCRIPTS.to_string());
        let cache = std::env::var("SAIL_SIGNALS_CACHE")
            .unwrap_or_else(|_| crate::models::DEFAULT_CACHE.to_string());
        let held = self.held_instruments(account)?;
        let now = Utc::now();
        for spec in self.model_specs.clone() {
            let signal = Self::signal_name(spec.signal);
            let (_, stored) = self.store.signal_values(&signal)?;
            let done: std::collections::BTreeSet<(String, chrono::NaiveDate)> =
                stored.into_iter().map(|(k, d, _)| (k, d)).collect();
            let mut items = Vec::new();
            let mut requested = 0u32;
            for (key, _) in &held {
                let observations = self.store.prices(key, now)?;
                if spec.needs_ohl && observations.iter().all(|o| o.ohl.is_none()) {
                    continue;
                }
                let sessions: Vec<chrono::NaiveDate> =
                    observations.iter().map(|o| o.session).collect();
                let origins: Vec<chrono::NaiveDate> =
                    crate::models::origins(&sessions, spec.evaluate_from)
                        .into_iter()
                        .filter(|d| !done.contains(&(key.clone(), *d)))
                        .collect();
                if origins.is_empty() {
                    continue;
                }
                requested += origins.len() as u32;
                use rust_decimal::prelude::ToPrimitive;
                let bars = observations
                    .iter()
                    .filter_map(|o| {
                        let close = o.close.to_f64()?;
                        let (open, high, low) = match &o.ohl {
                            Some(ohl) => (ohl.open.to_f64(), ohl.high.to_f64(), ohl.low.to_f64()),
                            None => (None, None, None),
                        };
                        Some((o.session, open, high, low, close))
                    })
                    .collect();
                items.push(crate::models::HelperItem {
                    key: key.clone(),
                    origins,
                    bars,
                });
            }
            let run = if items.is_empty() {
                ModelRun {
                    at: Utc::now(),
                    model_version: None,
                    requested: 0,
                    added: 0,
                    skipped: 0,
                    error: None,
                    notes: vec!["Nothing new to compute.".into()],
                }
            } else {
                match crate::models::run_helper(&spec, &python, &scripts, Some(&cache), &items)
                    .await
                {
                    Ok(output) => {
                        let added = self.store.record_signal_values(
                            &signal,
                            &output.model_version,
                            &output.values,
                            Utc::now(),
                        )?;
                        ModelRun {
                            at: Utc::now(),
                            model_version: Some(output.model_version),
                            requested,
                            added,
                            skipped: output.skipped,
                            error: None,
                            notes: output.notes,
                        }
                    }
                    Err(e) => ModelRun {
                        at: Utc::now(),
                        model_version: None,
                        requested,
                        added: 0,
                        skipped: 0,
                        error: Some(e.to_string()),
                        notes: Vec::new(),
                    },
                }
            };
            if let Ok(mut runs) = self.model_runs.lock() {
                runs.insert(spec.name.clone(), run);
            }
        }
        self.models_status()
    }
}
