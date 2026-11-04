//! Transactional reservations: crashes consume a slot, never implicitly retry a request.
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sail_protocol::external::{AttemptOutcome, EgressReceipt};
use std::{path::Path, sync::Mutex, time::Duration};
use uuid::Uuid;

pub struct UsageLedger(Mutex<Connection>);

// Fixed descriptor policy; OpenOptions itself supplies CLOEXEC.
const LEDGER_OPEN_FLAGS: i32 = libc::O_NOFOLLOW | libc::O_NONBLOCK;

impl UsageLedger {
    /// Explicit paths support tests/embedders. Production uses `open_default`.
    pub fn open(path: &Path) -> Result<Self, String> {
        use std::os::unix::fs::OpenOptionsExt;
        // Create privately before SQLite opens the path; reject symlinks and special files.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(LEDGER_OPEN_FLAGS)
            .open(path)
            .map_err(|_| "external ledger file unavailable")?;
        if !file
            .metadata()
            .map_err(|_| "cannot inspect external ledger")?
            .is_file()
        {
            return Err("external ledger must be a regular file".into());
        }
        let connection = Connection::open(path).map_err(|_| "external ledger cannot be opened")?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|_| "ledger timeout setup failed")?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS external_attempts (
                id TEXT PRIMARY KEY, provider TEXT NOT NULL, day INTEGER NOT NULL,
                receipt TEXT NOT NULL, outcome TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS external_daily ON external_attempts(provider, day);
            CREATE TABLE IF NOT EXISTS external_cooldowns (
                provider TEXT PRIMARY KEY, until_at INTEGER NOT NULL);",
            )
            .map_err(|_| "external ledger schema unavailable")?;
        Ok(Self(Mutex::new(connection)))
    }

    /// Never fall back to a root-disk or in-memory quota ledger.
    pub fn open_default() -> Result<Self, String> {
        let root = sail_common::storage::resolve();
        let mounted = sail_common::storage::is_mount_point(&root.path);
        Self::open_data_root(root, mounted)
    }

    fn open_data_root(root: sail_common::storage::DataRoot, mounted: bool) -> Result<Self, String> {
        if matches!(
            root.source,
            sail_common::storage::DataRootSource::Fallback(_)
        ) || !mounted
        {
            return Err("external providers require a mounted SAIL_DATA directory".into());
        }
        let directory = root.path.join("mind");
        std::fs::create_dir_all(&directory)
            .map_err(|_| "cannot create external ledger directory")?;
        Self::open(&directory.join("external-usage.sqlite3"))
    }

    pub fn reserve(&self, receipt: &EgressReceipt, daily_cap: u32) -> Result<(), String> {
        self.reserve_at(receipt, daily_cap, Utc::now().timestamp())
    }

    fn reserve_at(&self, receipt: &EgressReceipt, daily_cap: u32, now: i64) -> Result<(), String> {
        if daily_cap == 0
            || daily_cap > 10_000
            || receipt.outcome != AttemptOutcome::Reserved
            || !(now.saturating_sub(5)..=now).contains(&receipt.reserved_at)
            || receipt.version != sail_protocol::external::VERSION
            || receipt.request_sha256.len() != 64
            || !receipt
                .request_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
            || receipt.input_bytes > 32_768
            || receipt.provider.is_empty()
            || receipt.provider.len() > 64
            || receipt.model.is_empty()
            || receipt.model.len() > 128
            || receipt.purpose != "explicit_text"
            || receipt.finished_at.is_some()
        {
            return Err("invalid external quota reservation".into());
        }
        let mut connection = self
            .0
            .lock()
            .map_err(|_| "external ledger lock unavailable")?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "external quota transaction unavailable")?;
        let cooldown: Option<i64> = transaction
            .query_row(
                "SELECT until_at FROM external_cooldowns WHERE provider=?1",
                [&receipt.provider],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| "cannot check provider cooldown")?;
        if cooldown.is_some_and(|until| until > now) {
            return Err("external provider is cooling down".into());
        }
        let day = now.div_euclid(86400);
        let count: u32 = transaction
            .query_row(
                "SELECT count(*) FROM external_attempts WHERE provider=?1 AND day=?2",
                params![receipt.provider, day],
                |r| r.get(0),
            )
            .map_err(|_| "cannot check external daily cap")?;
        if count >= daily_cap {
            return Err("external provider daily cap reached".into());
        }
        transaction
            .execute(
                "INSERT INTO external_attempts VALUES (?1, ?2, ?3, ?4, 'reserved')",
                params![
                    receipt.request_id.to_string(),
                    receipt.provider,
                    day,
                    serde_json::to_string(receipt).map_err(|_| "cannot encode egress receipt")?
                ],
            )
            .map_err(|_| "external request already reserved or ledger unavailable")?;
        transaction
            .commit()
            .map_err(|_| "cannot commit external quota reservation".into())
    }

    pub fn dispatch(&self, id: Uuid) -> Result<(), String> {
        self.transition(id, AttemptOutcome::Dispatched, None, None, None)
    }

    pub fn finish(
        &self,
        id: Uuid,
        outcome: AttemptOutcome,
        input: Option<u32>,
        output: Option<u32>,
        cooldown: Option<i64>,
    ) -> Result<(), String> {
        if matches!(
            outcome,
            AttemptOutcome::Reserved | AttemptOutcome::Dispatched
        ) {
            return Err("terminal external outcome required".into());
        }
        self.transition(id, outcome, input, output, cooldown)
    }

    fn transition(
        &self,
        id: Uuid,
        outcome: AttemptOutcome,
        input: Option<u32>,
        output: Option<u32>,
        cooldown: Option<i64>,
    ) -> Result<(), String> {
        let mut connection = self
            .0
            .lock()
            .map_err(|_| "external ledger lock unavailable")?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "ledger unavailable")?;
        let json: String = transaction
            .query_row(
                "SELECT receipt FROM external_attempts WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .map_err(|_| "external reservation not found")?;
        let mut receipt: EgressReceipt =
            serde_json::from_str(&json).map_err(|_| "corrupt egress receipt")?;
        if receipt.outcome != AttemptOutcome::Reserved
            && receipt.outcome != AttemptOutcome::Dispatched
        {
            return Err("external request already finished".into());
        }
        if outcome == AttemptOutcome::Dispatched && receipt.outcome != AttemptOutcome::Reserved {
            return Err("external request already dispatched".into());
        }
        receipt.outcome = outcome;
        receipt.input_tokens = input;
        receipt.output_tokens = output;
        if receipt.outcome != AttemptOutcome::Dispatched {
            receipt.finished_at = Some(Utc::now().timestamp());
        }
        transaction
            .execute(
                "UPDATE external_attempts SET receipt=?2,outcome=?3 WHERE id=?1",
                params![
                    id.to_string(),
                    serde_json::to_string(&receipt).map_err(|_| "cannot encode receipt")?,
                    format!("{:?}", receipt.outcome)
                ],
            )
            .map_err(|_| "cannot update egress receipt")?;
        if let Some(until) = cooldown {
            transaction.execute("INSERT INTO external_cooldowns VALUES (?1,?2) ON CONFLICT(provider) DO UPDATE SET until_at=max(until_at,excluded.until_at)", params![receipt.provider, until]).map_err(|_| "cannot persist cooldown")?;
        }
        transaction
            .commit()
            .map_err(|_| "cannot commit egress outcome".into())
    }

    pub fn receipt(&self, id: Uuid) -> Result<Option<EgressReceipt>, String> {
        let connection = self
            .0
            .lock()
            .map_err(|_| "external ledger lock unavailable")?;
        let json: Option<String> = connection
            .query_row(
                "SELECT receipt FROM external_attempts WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| "cannot read egress receipt")?;
        json.map(|value| serde_json::from_str(&value).map_err(|_| "corrupt egress receipt".into()))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(now: i64) -> EgressReceipt {
        EgressReceipt {
            version: 1,
            request_id: Uuid::new_v4(),
            provider: "test".into(),
            model: "test-model".into(),
            purpose: "explicit_text".into(),
            request_sha256: "a".repeat(64),
            input_bytes: 12,
            redactions: 0,
            reserved_at: now,
            finished_at: None,
            outcome: AttemptOutcome::Reserved,
            input_tokens: None,
            output_tokens: None,
        }
    }

    struct TemporaryLedger(std::path::PathBuf);
    impl TemporaryLedger {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("external-tests-{}", Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn open(&self) -> UsageLedger {
            UsageLedger::open(&self.0.join("ledger.db")).unwrap()
        }
    }
    impl Drop for TemporaryLedger {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn each_reservation_invariant_is_enforced_before_inserting_a_row() {
        let directory = TemporaryLedger::new();
        let ledger = directory.open();
        let now = 1_800_000_000;
        let mut cases = Vec::new();
        let mut r = sample(now);
        r.outcome = AttemptOutcome::Completed;
        cases.push(r);
        let mut r = sample(now);
        r.reserved_at = now - 6;
        cases.push(r);
        let mut r = sample(now);
        r.reserved_at = now + 1;
        cases.push(r);
        let mut r = sample(now);
        r.version = 2;
        cases.push(r);
        for hash in ["a".repeat(63), "a".repeat(65), "z".repeat(64)] {
            let mut r = sample(now);
            r.request_sha256 = hash;
            cases.push(r);
        }
        for bytes in [32769, 32770] {
            let mut r = sample(now);
            r.input_bytes = bytes;
            cases.push(r);
        }
        for name in [String::new(), "x".repeat(65), "x".repeat(66)] {
            let mut r = sample(now);
            r.provider = name;
            cases.push(r);
        }
        for model in [String::new(), "x".repeat(129), "x".repeat(130)] {
            let mut r = sample(now);
            r.model = model;
            cases.push(r);
        }
        let mut r = sample(now);
        r.purpose = "implicit_memory".into();
        cases.push(r);
        let mut r = sample(now);
        r.finished_at = Some(now);
        cases.push(r);
        for receipt in cases {
            assert!(
                ledger.reserve_at(&receipt, 10000, now).is_err(),
                "invalid reservation accepted: {receipt:?}"
            );
            assert!(ledger.receipt(receipt.request_id).unwrap().is_none());
        }
        for cap in [0, 10001, 10002] {
            assert!(ledger.reserve_at(&sample(now), cap, now).is_err());
        }
        let mut boundary = sample(now);
        boundary.input_bytes = 32768;
        boundary.provider = "p".repeat(64);
        boundary.model = "m".repeat(128);
        boundary.reserved_at = now - 5;
        ledger.reserve_at(&boundary, 10000, now).unwrap();
        assert_eq!(
            ledger
                .receipt(boundary.request_id)
                .unwrap()
                .unwrap()
                .input_bytes,
            32768
        );
        let receipt = sample(Utc::now().timestamp());
        ledger.reserve(&receipt, 10000).unwrap();
        assert_eq!(
            ledger.receipt(receipt.request_id).unwrap().unwrap().outcome,
            AttemptOutcome::Reserved
        );
    }

    #[test]
    fn dispatch_and_finish_are_monotonic_and_preserve_usage_and_terminal_timestamps() {
        let directory = TemporaryLedger::new();
        let ledger = directory.open();
        let now = 1_800_000_000;
        let receipt = sample(now);
        ledger.reserve_at(&receipt, 1, now).unwrap();
        for outcome in [AttemptOutcome::Reserved, AttemptOutcome::Dispatched] {
            assert!(
                ledger
                    .finish(receipt.request_id, outcome, None, None, None)
                    .is_err()
            );
        }
        ledger.dispatch(receipt.request_id).unwrap();
        let dispatched = ledger.receipt(receipt.request_id).unwrap().unwrap();
        assert_eq!(dispatched.outcome, AttemptOutcome::Dispatched);
        assert!(dispatched.finished_at.is_none());
        assert!(ledger.dispatch(receipt.request_id).is_err());
        ledger
            .finish(
                receipt.request_id,
                AttemptOutcome::Completed,
                Some(12),
                Some(8),
                None,
            )
            .unwrap();
        let completed = ledger.receipt(receipt.request_id).unwrap().unwrap();
        assert_eq!(completed.outcome, AttemptOutcome::Completed);
        assert!(completed.finished_at.is_some());
        assert_eq!(
            (completed.input_tokens, completed.output_tokens),
            (Some(12), Some(8))
        );
        assert!(ledger.dispatch(receipt.request_id).is_err());
        assert!(
            ledger
                .finish(receipt.request_id, AttemptOutcome::Failed, None, None, None)
                .is_err()
        );
        assert_eq!(
            ledger.receipt(receipt.request_id).unwrap().unwrap().outcome,
            AttemptOutcome::Completed
        );
        assert!(ledger.dispatch(Uuid::new_v4()).is_err());
    }

    #[test]
    fn cooldown_expires_at_the_reported_second_not_one_second_later() {
        let directory = TemporaryLedger::new();
        let ledger = directory.open();
        let now = 1_800_000_000;
        let receipt = sample(now);
        ledger.reserve_at(&receipt, 10, now).unwrap();
        ledger
            .finish(
                receipt.request_id,
                AttemptOutcome::RateLimited,
                None,
                None,
                Some(now + 100),
            )
            .unwrap();
        assert!(ledger.reserve_at(&sample(now + 99), 10, now + 99).is_err());
        ledger
            .reserve_at(&sample(now + 100), 10, now + 100)
            .unwrap();
    }

    #[test]
    fn default_storage_refuses_fallbacks_and_unmounted_paths_without_creating_files() {
        use sail_common::storage::{DataRoot, DataRootSource};
        let directory = TemporaryLedger::new();
        for (source, mounted) in [
            (DataRootSource::Fallback("unavailable".into()), true),
            (DataRootSource::Env, false),
        ] {
            assert!(
                UsageLedger::open_data_root(
                    DataRoot {
                        path: directory.0.clone(),
                        source
                    },
                    mounted
                )
                .is_err()
            );
            assert!(!directory.0.join("mind").exists());
        }
        let ledger = UsageLedger::open_data_root(
            DataRoot {
                path: directory.0.clone(),
                source: DataRootSource::DataDisk,
            },
            true,
        )
        .unwrap();
        assert!(ledger.receipt(Uuid::new_v4()).unwrap().is_none());
        assert!(directory.0.join("mind/external-usage.sqlite3").is_file());
    }

    #[test]
    fn ledger_files_are_private_and_links_and_special_files_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        // This establishes the narrowly documented equivalent mutant: disjoint
        // flags have the same union under OR and XOR on the target platform.
        assert_eq!(libc::O_NOFOLLOW & libc::O_NONBLOCK, 0);
        let directory = TemporaryLedger::new();
        let _ledger = directory.open();
        assert_eq!(
            std::fs::metadata(directory.0.join("ledger.db"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let target = directory.0.join("target");
        std::fs::write(&target, "untouched").unwrap();
        let link = directory.0.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(UsageLedger::open(&link).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "untouched");
        // A valid target must still be refused, rather than merely failing later
        // because SQLite cannot parse the sentinel contents above.
        let database_link = directory.0.join("database-link");
        std::os::unix::fs::symlink(directory.0.join("ledger.db"), &database_link).unwrap();
        assert!(UsageLedger::open(&database_link).is_err());
        assert!(UsageLedger::open(&directory.0).is_err());
    }
    #[test]
    fn cap_dedup_restart_and_cooldown_are_durable() {
        let path = std::env::temp_dir().join(format!("external-ledger-{}.db", Uuid::new_v4()));
        let now = 1_800_000_000;
        let a = sample(now);
        {
            let ledger = UsageLedger::open(&path).unwrap();
            ledger.reserve_at(&a, 2, now).unwrap();
            assert!(ledger.reserve_at(&a, 2, now).is_err());
            ledger.dispatch(a.request_id).unwrap();
        }
        {
            let ledger = UsageLedger::open(&path).unwrap();
            assert_eq!(
                ledger.receipt(a.request_id).unwrap().unwrap().outcome,
                AttemptOutcome::Dispatched
            );
            let b = sample(now);
            ledger.reserve_at(&b, 2, now).unwrap();
            assert!(ledger.reserve_at(&sample(now), 2, now).is_err());
            ledger
                .finish(
                    b.request_id,
                    AttemptOutcome::RateLimited,
                    None,
                    None,
                    Some(now + 100),
                )
                .unwrap();
            assert!(ledger.reserve_at(&sample(now), 10, now).is_err());
            ledger
                .reserve_at(&sample(now + 86400), 2, now + 86400)
                .unwrap();
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn independent_connections_cannot_overspend() {
        let path = std::env::temp_dir().join(format!("external-race-{}.db", Uuid::new_v4()));
        let first = UsageLedger::open(&path).unwrap();
        let second = UsageLedger::open(&path).unwrap();
        let now = Utc::now().timestamp();
        let handles = [first, second].map(|ledger| {
            std::thread::spawn(move || ledger.reserve_at(&sample(now), 1, now).is_ok())
        });
        assert_eq!(
            handles
                .into_iter()
                .map(|h| usize::from(h.join().unwrap()))
                .sum::<usize>(),
            1
        );
        std::fs::remove_file(path).unwrap();
    }
}
