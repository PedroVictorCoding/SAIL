//! Transactional reservations: crashes consume a slot, never implicitly retry a request.
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sail_protocol::external::{AttemptOutcome, EgressReceipt};
use std::{path::Path, sync::Mutex, time::Duration};
use uuid::Uuid;

pub struct UsageLedger(Mutex<Connection>);

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
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
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
        if matches!(
            root.source,
            sail_common::storage::DataRootSource::Fallback(_)
        ) || !sail_common::storage::is_mount_point(&root.path)
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
