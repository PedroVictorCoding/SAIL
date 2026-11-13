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
use std::sync::{Arc, OnceLock};

mod dividends_run;
use grants::grant_record;
mod grants;
mod outlook;
mod prices;
mod research;
mod scorecard;
mod today;
mod trade_run;
pub use dividends_run::DividendRefresh;
pub use grants::{GrantCostInput, GrantInput};
pub use outlook::{ModelRun, ModelStatus, ModelsStatus, OutlookModelStatus, OutlookView};
pub use prices::{ForecastsView, MAX_PRICES_REPLY, PriceImport, PriceStatus};
pub use research::{FilingsView, MatchView, SecStatus};
pub use today::{IDEA_HORIZON, PolicyView};
pub use trade_run::{
    FxCloses, RecordTotals, SymbolSearchResult, TradeRecord, TradeSignalCheck, TradeView, fx_rate,
    next_run_after, watch_key,
};

/// Most import batches `/sync/status` returns.
pub const MAX_BATCHES_REPLY: u32 = 100;

/// Most ledger records one `/cashflows` reply carries.
pub const MAX_RECORDS_REPLY: usize = 5_000;

/// A stored ticker map younger than this is reused instead of fetched again.
pub const TICKER_MAP_MAX_AGE_HOURS: i64 = 24;

/// Default hours between background SEC refreshes (`$SAIL_SEC_REFRESH_HOURS`, 1–168).
pub const DEFAULT_SEC_REFRESH_HOURS: u64 = 12;

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
    /// One trade-signal run at a time.
    trade_running: tokio::sync::Mutex<()>,
    /// Why the last trade-signal run failed, if it did.
    trade_last_error: std::sync::Mutex<Option<String>>,
    /// Model signals the owner turned on (`SAIL_MODEL_SIGNALS`).
    model_specs: Vec<crate::models::ModelSpec>,
    /// Outlook models the owner turned on (`SAIL_MODEL_SIGNALS`).
    outlook_specs: Vec<crate::models::OutlookSpec>,
    /// Interpreter, script directory and cache for model helpers.
    helper_paths: (String, String, String),
    /// Track records, cached until prices, stored paths or holdings change.
    track_cache: std::sync::Mutex<std::collections::BTreeMap<String, crate::outlook::TrackRecord>>,
    /// One model refresh at a time.
    model_refreshing: tokio::sync::Mutex<()>,
    /// The last model refresh, per model name.
    model_runs: std::sync::Mutex<std::collections::BTreeMap<String, ModelRun>>,
    evaluation_cache: std::sync::Mutex<Option<(String, crate::forecast::EvalReport)>>,
    /// The signal report, cached like the evaluation.
    signals_cache: std::sync::Mutex<Option<(String, crate::signals::SignalReport)>>,
}

/// The daemon's Capital service, set once when Dialogue mounts the owner API.
static GLOBAL: OnceLock<Arc<CapitalService>> = OnceLock::new();

/// Publishes the daemon's service to in-process tools (the voice assistant).
pub fn set_global(service: Arc<CapitalService>) {
    let _ = GLOBAL.set(service);
}

/// The daemon's service, when Capital started.
pub fn global() -> Option<Arc<CapitalService>> {
    GLOBAL.get().cloned()
}

/// Where the ledger lives: `$SAIL_CAPITAL_DIR`, or else SAIL's data dir `capital/`.
pub fn default_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SAIL_CAPITAL_DIR") {
        return dir.into();
    }
    sail_common::storage::data_dir("capital")
}

impl CapitalService {
    /// `$SAIL_SEC_USER_AGENT`).
    pub fn open(dir: &Path) -> Result<Self, ServiceError> {
        let client = crate::sec::EdgarClient::from_env();
        let configured = client.is_configured();
        let mut service = Self::with_edgar(dir, std::sync::Arc::new(client), configured)?;
        let (prices, prices_configured) = crate::market::source_from_env();
        service.prices = prices;
        service.prices_configured = prices_configured;
        service.model_specs = crate::models::enabled_from_env();
        service.outlook_specs = crate::models::outlook_enabled_from_env();
        let var = |name: &str, default: &str| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        service.helper_paths = (
            var("SAIL_SIGNALS_PYTHON", crate::models::DEFAULT_PYTHON),
            var("SAIL_SIGNALS_SCRIPTS", crate::models::DEFAULT_SCRIPTS),
            var("SAIL_SIGNALS_CACHE", crate::models::DEFAULT_CACHE),
        );
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
            trade_running: tokio::sync::Mutex::new(()),
            trade_last_error: std::sync::Mutex::new(None),
            model_specs: Vec::new(),
            outlook_specs: Vec::new(),
            helper_paths: (
                crate::models::DEFAULT_PYTHON.to_string(),
                crate::models::DEFAULT_SCRIPTS.to_string(),
                crate::models::DEFAULT_CACHE.to_string(),
            ),
            track_cache: std::sync::Mutex::new(Default::default()),
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
}
