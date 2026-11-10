//! Wires the importers, the store and the ledger together for the HTTP layer. Every method
//! is blocking (SQLite, spreadsheet parsing), so async callers run them on a blocking
//! thread.
use crate::contracts::*;
use crate::ledger::{self, DividendView, Projection};
use crate::store::{AccountSummary, BatchStatus, CapitalStore, ConflictView, StoreError};
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

/// Most import batches `/sync/status` returns.
pub const MAX_BATCHES_REPLY: u32 = 100;

/// Most ledger records one `/cashflows` reply carries.
pub const MAX_RECORDS_REPLY: usize = 5_000;

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

/// The Capital service: one store, used through the importers and the ledger.
pub struct CapitalService {
    store: CapitalStore,
}

/// Where the ledger lives: `$SAIL_CAPITAL_DIR`, or else SAIL's data dir `capital/`.
pub fn default_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SAIL_CAPITAL_DIR") {
        return dir.into();
    }
    sail_common::storage::data_dir("capital")
}

impl CapitalService {
    /// Opens the store in `dir`.
    pub fn open(dir: &Path) -> Result<Self, ServiceError> {
        Ok(Self {
            store: CapitalStore::open(dir)?,
        })
    }

    /// Imports one XTB report.
    ///
    /// - `account_label` overrides the account named in the file.
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
        let label = account_label
            .filter(|l| !l.trim().is_empty())
            .or(report.account_label.as_deref())
            .unwrap_or("default");
        let account = AccountId::new(Broker::Xtb, label);
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
}
