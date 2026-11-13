//! The private ledger store: SQLite, append-only.
//!
//! - **Batches.** One row per uploaded file. The pair (account, file sha256) is unique, so
//!   uploading the same bytes again returns the first batch and adds nothing.
//! - **Entries** are insert-only. The pair (account, natural key) is unique.
//!   - Same key, same content: counted as a duplicate.
//!   - Same key, different content: recorded as a **conflict**. Both versions are kept,
//!     the stored one stays in force, and the owner decides.
//! - **Snapshots.** One per batch that carried broker figures.
//! - **Original files** are kept under `reports/<sha>.<ext>` as evidence. Only the owner
//!   can read the directory and the files.
//!
//! Nothing here ever updates or deletes an entry.
use crate::contracts::*;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

mod board;
mod dividends;
mod evidence;
mod grants;
mod outlook;
mod prices;
mod replays;
mod research;
mod signals;
mod suggest;
mod trade;

pub use dividends::DividendHistory;
pub use evidence::EvidenceOutcome;
pub use grants::{GrantView, RetractOutcome};
pub use prices::{PriceBarRevision, PriceCoverage, PriceOutcome};
pub use replays::ReplayCache;
pub use signals::{SignalValue, SignalValueCoverage};
pub use trade::{EarningsSnapshot, GradedCard};

/// Why the store failed. The messages are safe to show the owner because they never
/// include report contents.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite failed.
    #[error("ledger database error: {0}")]
    Db(#[from] rusqlite::Error),
    /// Reading or writing a file under the store directory failed.
    #[error("ledger file error: {0}")]
    Io(#[from] std::io::Error),
    /// A stored row couldn't be decoded, which means corruption or a version mismatch.
    #[error("ledger row could not be decoded: {0}")]
    Decode(String),
    /// The lock was poisoned by a panic elsewhere.
    #[error("ledger store is unavailable")]
    Poisoned,
}

/// What one import did. It doubles as the `/sync/status` record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchStatus {
    /// The account the batch was booked to.
    pub account: AccountId,
    /// The first 16 hex characters of sha256(account + file sha).
    pub batch_id: String,
    /// The broker.
    pub broker: Broker,
    /// The uploaded file name, cleaned and capped.
    pub file_name: String,
    /// sha256 of the file bytes.
    pub file_sha256: String,
    /// When SAIL stored it.
    pub imported_at: DateTime<Utc>,
    /// The report's own "as of", if it states one.
    pub report_as_of: Option<DateTime<Utc>>,
    /// Entries new to the ledger.
    pub added: u32,
    /// Entries already in the ledger with identical content.
    pub duplicates: u32,
    /// Entries whose key was already known but whose content differs.
    pub conflicts: u32,
    /// The importer's issues for this file.
    pub issues: Vec<ImportIssue>,
    /// The importer's issues beyond the stored cap.
    pub issues_dropped: u32,
    /// Per-sheet counts.
    pub sheets: Vec<SheetSummary>,
    /// The ledger contract version the batch was written with.
    pub contract: String,
    /// True when these bytes had been imported before, so this call added nothing.
    pub already_imported: bool,
}

/// A key seen again with different content. The owner is shown both versions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictView {
    /// The account.
    pub account: AccountId,
    /// The natural key both versions share.
    pub natural_key: String,
    /// The version in force.
    pub existing: ParsedEntry,
    /// The version that arrived later.
    pub incoming: ParsedEntry,
    /// The batch that brought the later version.
    pub batch_id: String,
    /// When the later version arrived.
    pub seen_at: DateTime<Utc>,
}

/// One account's summary, for `/accounts`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSummary {
    /// The account id.
    pub account: AccountId,
    /// The broker.
    pub broker: Broker,
    /// The currency from the latest report that states one.
    pub currency: Option<Currency>,
    /// When the latest file was imported.
    pub last_import_at: Option<DateTime<Utc>>,
    /// The "as of" of the latest broker snapshot.
    pub last_snapshot_as_of: Option<DateTime<Utc>>,
    /// How many entries the account's ledger holds.
    pub entries: u32,
}

/// The ledger, backed by SQLite.
pub struct CapitalStore {
    conn: Mutex<Connection>,
    dir: PathBuf,
    /// Decoded price histories, so analysis doesn't re-read and re-decode them on every
    /// request (see `prices::PriceCache`).
    price_cache: Mutex<prices::PriceCache>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS batches (
    batch_id TEXT PRIMARY KEY,
    account TEXT NOT NULL,
    broker TEXT NOT NULL,
    file_name TEXT NOT NULL,
    file_sha256 TEXT NOT NULL,
    imported_at TEXT NOT NULL,
    report_as_of TEXT,
    currency TEXT,
    status_json TEXT NOT NULL,
    UNIQUE (account, file_sha256)
);
CREATE TABLE IF NOT EXISTS entries (
    id TEXT PRIMARY KEY,
    account TEXT NOT NULL,
    natural_key TEXT NOT NULL,
    batch_id TEXT NOT NULL,
    ingested_at TEXT NOT NULL,
    occurred_at TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    entry_json TEXT NOT NULL,
    UNIQUE (account, natural_key)
);
CREATE INDEX IF NOT EXISTS entries_by_time ON entries (account, occurred_at);
CREATE TABLE IF NOT EXISTS conflicts (
    account TEXT NOT NULL,
    natural_key TEXT NOT NULL,
    batch_id TEXT NOT NULL,
    seen_at TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    entry_json TEXT NOT NULL,
    UNIQUE (account, natural_key, content_sha256)
);
CREATE TABLE IF NOT EXISTS snapshots (
    batch_id TEXT PRIMARY KEY,
    account TEXT NOT NULL,
    ingested_at TEXT NOT NULL,
    snapshot_json TEXT NOT NULL
);
CREATE TRIGGER IF NOT EXISTS entries_no_update BEFORE UPDATE ON entries
BEGIN SELECT RAISE(ABORT, 'ledger entries are append-only'); END;
CREATE TRIGGER IF NOT EXISTS entries_no_delete BEFORE DELETE ON entries
BEGIN SELECT RAISE(ABORT, 'ledger entries are append-only'); END;
";

impl CapitalStore {
    /// Opens the store in `dir`, creating it if needed. `dir` and `dir/reports` are
    /// created so that only the owner can read them.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        create_private_dir(dir)?;
        create_private_dir(&dir.join("reports"))?;
        let db = dir.join("capital.sqlite3");
        let conn = Connection::open(&db)?;
        restrict_file(&db)?;
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(evidence::SCHEMA)?;
        conn.execute_batch(grants::SCHEMA)?;
        conn.execute_batch(suggest::SCHEMA)?;
        conn.execute_batch(trade::SCHEMA)?;
        conn.execute_batch(dividends::SCHEMA)?;
        conn.execute_batch(board::SCHEMA)?;
        conn.execute_batch(replays::SCHEMA)?;
        conn.execute_batch(research::SCHEMA)?;
        prices::init_price_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            dir: dir.to_path_buf(),
            price_cache: Mutex::new(prices::PriceCache::default()),
        })
    }

    /// Books a parsed report to `account` and keeps the original `bytes` as evidence.
    /// `now` is the ingest time; it's a parameter so tests can control "known since".
    pub fn record_import(
        &self,
        account: &AccountId,
        file_name: &str,
        bytes: &[u8],
        report: &ParsedReport,
        now: DateTime<Utc>,
    ) -> Result<BatchStatus, StoreError> {
        let file_sha = sha256_hex(bytes);
        let batch_id = short_id(account, &file_sha);
        let mut conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;

        let known: Option<String> = conn
            .query_row(
                "SELECT status_json FROM batches WHERE account = ?1 AND file_sha256 = ?2",
                params![account.as_str(), file_sha],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(json) = known {
            let mut status: BatchStatus = decode(&json)?;
            status.added = 0;
            status.duplicates = 0;
            status.conflicts = 0;
            status.already_imported = true;
            return Ok(status);
        }

        let ext = if file_name.to_ascii_lowercase().ends_with(".csv") {
            "csv"
        } else {
            "xlsx"
        };
        let evidence = self.dir.join("reports").join(format!("{file_sha}.{ext}"));
        if !evidence.exists() {
            std::fs::write(&evidence, bytes)?;
            restrict_file(&evidence)?;
        }

        let tx = conn.transaction()?;
        let (mut added, mut duplicates, mut conflicts) = (0u32, 0u32, 0u32);
        for entry in &report.entries {
            let content = content_sha(entry)?;
            let existing: Option<String> = tx
                .query_row(
                    "SELECT content_sha256 FROM entries
                     WHERE account = ?1 AND natural_key = ?2",
                    params![account.as_str(), entry.natural_key],
                    |r| r.get(0),
                )
                .optional()?;
            match existing {
                None => {
                    tx.execute(
                        "INSERT INTO entries (id, account, natural_key, batch_id, ingested_at,
                         occurred_at, content_sha256, entry_json)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            short_id(account, &entry.natural_key),
                            account.as_str(),
                            entry.natural_key,
                            batch_id,
                            now.to_rfc3339(),
                            entry.occurred.at.to_rfc3339(),
                            content,
                            encode(entry)?,
                        ],
                    )?;
                    added += 1;
                }
                Some(sha) if sha == content => duplicates += 1,
                Some(_) => {
                    tx.execute(
                        "INSERT OR IGNORE INTO conflicts (account, natural_key, batch_id,
                         seen_at, content_sha256, entry_json)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            account.as_str(),
                            entry.natural_key,
                            batch_id,
                            now.to_rfc3339(),
                            content,
                            encode(entry)?,
                        ],
                    )?;
                    conflicts += 1;
                }
            }
        }

        let snap = &report.snapshot;
        let has_snapshot = snap.as_of.is_some()
            || !snap.open_positions.is_empty()
            || !snap.closed_positions.is_empty()
            || snap.balance.is_some()
            || snap.equity.is_some();
        if has_snapshot {
            tx.execute(
                "INSERT INTO snapshots (batch_id, account, ingested_at, snapshot_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![batch_id, account.as_str(), now.to_rfc3339(), encode(snap)?],
            )?;
        }

        let (clean_name, _) = clean_cell_text(file_name);
        let status = BatchStatus {
            account: account.clone(),
            batch_id: batch_id.clone(),
            broker: report.broker,
            file_name: clean_name,
            file_sha256: file_sha.clone(),
            imported_at: now,
            report_as_of: snap.as_of,
            added,
            duplicates,
            conflicts,
            issues: report.issues.clone(),
            issues_dropped: report.issues_dropped,
            sheets: report.sheets.clone(),
            contract: CONTRACT_VERSION.to_string(),
            already_imported: false,
        };
        tx.execute(
            "INSERT INTO batches (batch_id, account, broker, file_name, file_sha256,
             imported_at, report_as_of, currency, status_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                batch_id,
                account.as_str(),
                report.broker.as_str(),
                status.file_name,
                file_sha,
                now.to_rfc3339(),
                status.report_as_of.map(|t| t.to_rfc3339()),
                report
                    .account_currency
                    .as_ref()
                    .map(|c| c.as_str().to_string()),
                encode(&status)?,
            ],
        )?;
        tx.commit()?;
        Ok(status)
    }

    /// A cheap fingerprint of everything the ledger projection reads for `account`
    /// (entries, snapshots and grants, including retractions). It changes on any write.
    pub fn ledger_version(&self, account: &AccountId) -> Result<String, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(conn.query_row(
            "SELECT (SELECT COUNT(*) || ':' || IFNULL(MAX(rowid), 0) FROM entries WHERE account = ?1)
                 || '|' || (SELECT COUNT(*) || ':' || IFNULL(MAX(rowid), 0) FROM snapshots WHERE account = ?1)
                 || '|' || (SELECT COUNT(*) || ':' || IFNULL(MAX(rowid), 0) || ':' || COUNT(retracted_at)
                            FROM grants WHERE account = ?1)",
            params![account.as_str()],
            |r| r.get(0),
        )?)
    }

    /// Every entry of `account`, in the order it was inserted. The ledger does its own
    /// sorting.
    pub fn records(&self, account: &AccountId) -> Result<Vec<LedgerRecord>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT id, batch_id, ingested_at, entry_json FROM entries
             WHERE account = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map(params![account.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, batch_id, ingested, json) = row?;
            out.push(LedgerRecord {
                id,
                account: account.clone(),
                batch_id,
                ingested_at: parse_ts(&ingested)?,
                entry: decode(&json)?,
            });
        }
        Ok(out)
    }

    /// Every broker snapshot of `account`, oldest import first.
    pub fn snapshots(&self, account: &AccountId) -> Result<Vec<StoredSnapshot>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT batch_id, ingested_at, snapshot_json FROM snapshots
             WHERE account = ?1 ORDER BY ingested_at, batch_id",
        )?;
        let rows = stmt.query_map(params![account.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (batch_id, ingested, json) = row?;
            out.push(StoredSnapshot {
                account: account.clone(),
                batch_id,
                ingested_at: parse_ts(&ingested)?,
                snapshot: decode(&json)?,
            });
        }
        Ok(out)
    }

    /// Import history, newest first, optionally for one account, at most `limit` rows.
    pub fn batches(
        &self,
        account: Option<&AccountId>,
        limit: u32,
    ) -> Result<Vec<BatchStatus>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT status_json FROM batches WHERE (?1 IS NULL OR account = ?1)
             ORDER BY imported_at DESC, batch_id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![account.map(|a| a.as_str()), limit], |r| {
            r.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(decode(&row?)?);
        }
        Ok(out)
    }

    /// Open conflicts for `account`, oldest first.
    pub fn conflicts(&self, account: &AccountId) -> Result<Vec<ConflictView>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT c.natural_key, c.batch_id, c.seen_at, c.entry_json, e.entry_json
             FROM conflicts c JOIN entries e
               ON e.account = c.account AND e.natural_key = c.natural_key
             WHERE c.account = ?1 ORDER BY c.seen_at, c.natural_key",
        )?;
        let rows = stmt.query_map(params![account.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (natural_key, batch_id, seen, incoming, existing) = row?;
            out.push(ConflictView {
                account: account.clone(),
                natural_key,
                existing: decode(&existing)?,
                incoming: decode(&incoming)?,
                batch_id,
                seen_at: parse_ts(&seen)?,
            });
        }
        Ok(out)
    }

    /// Every account with at least one import.
    pub fn accounts(&self) -> Result<Vec<AccountSummary>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT b.account, b.broker, MAX(b.imported_at),
                    (SELECT currency FROM batches c WHERE c.account = b.account
                       AND c.currency IS NOT NULL ORDER BY c.imported_at DESC LIMIT 1),
                    (SELECT MAX(report_as_of) FROM batches d WHERE d.account = b.account),
                    (SELECT COUNT(*) FROM entries e WHERE e.account = b.account)
             FROM batches b GROUP BY b.account, b.broker ORDER BY b.account",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, u32>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (account, broker, last, currency, snap, entries) = row?;
            out.push(AccountSummary {
                account: AccountId::from_stored(account),
                broker: match broker.as_str() {
                    "xtb" => Broker::Xtb,
                    _ => Broker::Manual,
                },
                currency: currency.as_deref().and_then(Currency::parse),
                last_import_at: last.as_deref().map(parse_ts).transpose()?,
                last_snapshot_as_of: snap.as_deref().map(parse_ts).transpose()?,
                entries,
            });
        }
        Ok(out)
    }
}

/// sha256 of an entry's content with its row position cleared, so the same operation
/// found at a different row of a longer export has the same content hash.
fn content_sha(entry: &ParsedEntry) -> Result<String, StoreError> {
    let mut neutral = entry.clone();
    neutral.source.row = 0;
    neutral.source.sheet = None;
    Ok(sha256_hex(encode(&neutral)?.as_bytes()))
}

fn short_id(account: &AccountId, key: &str) -> String {
    sha256_hex(format!("{}\n{key}", account.as_str()).as_bytes())[..16].to_string()
}

fn encode<T: Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|e| StoreError::Decode(e.to_string()))
}

fn decode<T: for<'de> Deserialize<'de>>(json: &str) -> Result<T, StoreError> {
    serde_json::from_str(json).map_err(|e| StoreError::Decode(e.to_string()))
}

fn parse_ts(text: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(text)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| StoreError::Decode(format!("timestamp {text:?}: {e}")))
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn restrict_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn entry(key: &str, row: u32, amount: &str) -> ParsedEntry {
        ParsedEntry {
            natural_key: key.to_string(),
            source: RowRef {
                sheet: Some("CASH OPERATION HISTORY".into()),
                sheet_kind: SheetKind::CashOperations,
                row,
            },
            broker_ref: None,
            occurred: EventTime {
                at: Utc.with_ymd_and_hms(2026, 3, 2, 10, 0, 0).unwrap(),
                precision: TimePrecision::Exact,
            },
            kind: EntryKind::Deposit,
            cash_effect: Some(Money::new(
                Decimal::from_str(amount).unwrap(),
                Currency::eur(),
            )),
            comment: None,
        }
    }

    fn report(entries: Vec<ParsedEntry>) -> ParsedReport {
        ParsedReport {
            broker: Broker::Xtb,
            account_label: None,
            account_currency: Currency::parse("EUR"),
            entries,
            snapshot: BrokerSnapshot::default(),
            sheets: Vec::new(),
            issues: Vec::new(),
            issues_dropped: 0,
        }
    }

    fn t(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, 9, 0, 0).unwrap()
    }

    #[test]
    fn same_bytes_twice_adds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = CapitalStore::open(dir.path()).unwrap();
        let acct = AccountId::new(Broker::Xtb, "1234");
        let r = report(vec![entry("xtb:cash:1", 12, "100")]);
        let first = store
            .record_import(&acct, "a.xlsx", b"one", &r, t(10))
            .unwrap();
        assert_eq!((first.added, first.already_imported), (1, false));
        let again = store
            .record_import(&acct, "a.xlsx", b"one", &r, t(10))
            .unwrap();
        assert_eq!((again.added, again.already_imported), (0, true));
        assert_eq!(store.records(&acct).unwrap().len(), 1);
    }

    #[test]
    fn longer_export_adds_only_new_rows_and_flags_changed_ones() {
        let dir = tempfile::tempdir().unwrap();
        let store = CapitalStore::open(dir.path()).unwrap();
        let acct = AccountId::new(Broker::Xtb, "1234");
        let first = report(vec![entry("k1", 12, "100"), entry("k2", 13, "50")]);
        store
            .record_import(&acct, "a.xlsx", b"one", &first, t(10))
            .unwrap();
        // k1 moved rows (same content), k2 changed amount, k3 is new.
        let later = report(vec![
            entry("k1", 20, "100"),
            entry("k2", 21, "55"),
            entry("k3", 22, "7"),
        ]);
        let s = store
            .record_import(&acct, "b.xlsx", b"two", &later, t(11))
            .unwrap();
        assert_eq!((s.added, s.duplicates, s.conflicts), (1, 1, 1));

        let records = store.records(&acct).unwrap();
        assert_eq!(records.len(), 3);
        let k2 = records
            .iter()
            .find(|r| r.entry.natural_key == "k2")
            .unwrap();
        let amount = k2.entry.cash_effect.as_ref().unwrap().amount;
        assert_eq!(
            amount,
            Decimal::from(50),
            "the stored version stays in force"
        );
        assert_eq!(k2.ingested_at, t(10));

        let conflicts = store.conflicts(&acct).unwrap();
        assert_eq!(conflicts.len(), 1);
        let incoming = conflicts[0].incoming.cash_effect.as_ref().unwrap().amount;
        assert_eq!(incoming, Decimal::from(55));

        let accounts = store.accounts().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].entries, 3);
        assert_eq!(accounts[0].last_import_at, Some(t(11)));
        assert_eq!(store.batches(Some(&acct), 10).unwrap().len(), 2);
    }

    #[test]
    fn entries_cannot_be_updated_or_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let store = CapitalStore::open(dir.path()).unwrap();
        let acct = AccountId::new(Broker::Xtb, "1234");
        let r = report(vec![entry("k", 2, "1")]);
        store
            .record_import(&acct, "a.csv", b"x", &r, t(10))
            .unwrap();
        let conn = store.conn.lock().unwrap();
        assert!(
            conn.execute("UPDATE entries SET batch_id = 'x'", [])
                .is_err()
        );
        assert!(conn.execute("DELETE FROM entries", []).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn evidence_and_database_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("capital");
        let store = CapitalStore::open(&root).unwrap();
        let acct = AccountId::new(Broker::Xtb, "1234");
        let s = store
            .record_import(&acct, "a.csv", b"x", &report(vec![]), t(10))
            .unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&root.join("capital.sqlite3")), 0o600);
        let evidence = root.join("reports").join(format!("{}.csv", s.file_sha256));
        assert_eq!(mode(&evidence), 0o600);
    }

    #[test]
    fn a_pre_release_price_table_is_rebuilt_without_losing_rows() {
        use crate::market::{Adjustment, MarketObservation, PriceBar, PriceSeries};

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("capital.sqlite3");
        let session = chrono::NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
        let old = MarketObservation {
            instrument_key: "sym:X".into(),
            symbol: "X".into(),
            session,
            close: Decimal::from_str("100").unwrap(),
            ohl: None,
            currency: None,
            price_kind: "close".into(),
            adjustment: Adjustment::Unknown,
            source_id: "fake".into(),
            first_available_at: t(10),
            ingested_at: t(10),
        };
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE price_bars (
                    id TEXT PRIMARY KEY,
                    instrument_key TEXT NOT NULL,
                    source_id TEXT NOT NULL,
                    session TEXT NOT NULL,
                    bar_sha256 TEXT NOT NULL,
                    first_available_at TEXT NOT NULL,
                    ingested_at TEXT NOT NULL,
                    observation_json TEXT NOT NULL,
                    UNIQUE (instrument_key, source_id, session, bar_sha256)
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO price_bars VALUES ('old', 'sym:X', 'fake', '2025-01-02',
                    'old', ?1, ?1, ?2)",
                params![t(10).to_rfc3339(), encode(&old).unwrap()],
            )
            .unwrap();
        }

        let store = CapitalStore::open(dir.path()).unwrap();
        {
            let conn = store.conn.lock().unwrap();
            let sql: String = conn
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'price_bars'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(!sql.contains("UNIQUE (instrument_key, source_id, session, bar_sha256)"));
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM price_bars", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 1);
        }

        let series = PriceSeries {
            source_id: "fake".into(),
            symbol: "X".into(),
            source_symbol: "x".into(),
            currency: None,
            adjustment: Adjustment::Unknown,
            bars: vec![PriceBar {
                session,
                open: None,
                high: None,
                low: None,
                close: Decimal::from_str("110").unwrap(),
                volume: None,
            }],
            payload_sha256: "x".into(),
        };
        let outcome = store.record_prices("sym:X", &series, t(20)).unwrap();
        assert_eq!((outcome.added, outcome.revisions), (0, 1));
        let rows = store.prices("sym:X", t(20)).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].close, Decimal::from_str("110").unwrap());
        assert_eq!(rows[0].first_available_at, t(10));
    }
}
