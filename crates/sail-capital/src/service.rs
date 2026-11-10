//! Wires the importers, the store and the ledger together for the HTTP layer. Every method
//! is blocking (SQLite, spreadsheet parsing), so async callers run them on a blocking
//! thread.
use crate::contracts::*;
use crate::evidence::{Cik, EdgarApi, IssuerMatch, SourceError, WatchItem};
use crate::ledger::{self, DividendView, Projection};
use crate::store::EvidenceOutcome;
use crate::store::{AccountSummary, BatchStatus, CapitalStore, ConflictView, StoreError};
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
    store: CapitalStore,
    edgar: std::sync::Arc<dyn EdgarApi>,
    sec_configured: bool,
    sec_status: std::sync::Mutex<SecStatus>,
    sec_refreshing: tokio::sync::Mutex<()>,
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
        Self::with_edgar(dir, std::sync::Arc::new(client), configured)
    }

    /// Opens the store in `dir` with a given SEC source (tests pass a fake).
    pub fn with_edgar(
        dir: &Path,
        edgar: std::sync::Arc<dyn EdgarApi>,
        sec_configured: bool,
    ) -> Result<Self, ServiceError> {
        Ok(Self {
            store: CapitalStore::open(dir)?,
            edgar,
            sec_configured,
            sec_status: std::sync::Mutex::new(SecStatus {
                configured: sec_configured,
                ..SecStatus::default()
            }),
            sec_refreshing: tokio::sync::Mutex::new(()),
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
        let records = self.store.records(account)?;
        let snapshots = self.store.snapshots(account)?;
        Ok(ledger::project(account, &records, &snapshots, as_of))
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
