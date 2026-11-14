//! The durable outbox: replies arrive at least once, never twice.
//!
//! A reply is queued under a stable key. The sender is only removed after Telegram
//! confirms it, and its key is remembered for seven days, so a crash between "sent" and
//! "recorded" cannot duplicate a reply. Delivery backs off on failure and the getUpdates
//! offset is stored beside the queue.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

/// One queued reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutMsg {
    /// Stable key: the same reply always gets the same key, e.g. "<chat>:<update_id>:<part>".
    pub key: String,
    /// Destination private chat.
    pub chat_id: i64,
    /// The reply text after Telegram-safe chunking.
    pub text: String,
    /// Message id to reply to, when the reply belongs to a thread.
    pub reply_to: Option<i64>,
    /// Inline buttons as (label, callback data).
    pub buttons: Vec<(String, String)>,
    /// Delivery attempts so far.
    pub attempts: u32,
    /// Unix seconds before which no retry is attempted.
    pub next_attempt_at: i64,
    /// Unix seconds when the reply was queued.
    pub created_at: i64,
}

/// `<dir>/outbox.json` (0o600): queued messages plus the keys sent in the last 7 days
/// (`sent: {key: sent_at}`) and the update offset (`offset`). One process-wide mutex.
pub struct Outbox {
    pub dir: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OutFile {
    version: u32,
    queued: Vec<OutMsg>,
    sent: HashMap<String, i64>,
    offset: i64,
}

static OUTBOX_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
const SENT_TTL_SECS: i64 = 7 * 24 * 60 * 60;
const DROP_AFTER_ATTEMPTS: u32 = 20;
const DROP_AFTER_SECS: i64 = 24 * 60 * 60;

impl Outbox {
    /// Queue unless the key was already sent or is already queued (then Ok(false)).
    pub fn enqueue(&self, msg: OutMsg) -> Result<bool, String> {
        let _guard = lock();
        let mut file = self.load()?;
        if file.sent.contains_key(&msg.key)
            || file.queued.iter().any(|queued| queued.key == msg.key)
        {
            return Ok(false);
        }
        file.queued.push(msg);
        self.save(&file)?;
        Ok(true)
    }

    /// Messages due at `now`, oldest first, at most `limit`.
    pub fn due(&self, now: i64, limit: usize) -> Result<Vec<OutMsg>, String> {
        let _guard = lock();
        let file = self.load()?;
        let mut due: Vec<OutMsg> = file
            .queued
            .into_iter()
            .filter(|msg| msg.next_attempt_at <= now)
            .collect();
        due.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.key.cmp(&right.key))
        });
        due.truncate(limit);
        Ok(due)
    }

    /// Sent: remove from the queue, remember the key.
    pub fn sent(&self, key: &str, now: i64) -> Result<(), String> {
        let _guard = lock();
        let mut file = self.load()?;
        file.queued.retain(|msg| msg.key != key);
        file.sent.insert(key.to_string(), now);
        self.save(&file)
    }

    /// Failed: attempts += 1, back off (5 s, 30 s, 2 min, 10 min, then every 30 min); after
    /// 20 attempts or 24 h the message is dropped and Ok(true) is returned ("tell the owner").
    pub fn failed(&self, key: &str, now: i64) -> Result<bool, String> {
        let _guard = lock();
        let mut file = self.load()?;
        let Some(position) = file.queued.iter().position(|msg| msg.key == key) else {
            return Err("that message is not queued".to_string());
        };
        let msg = &mut file.queued[position];
        msg.attempts = msg.attempts.saturating_add(1);
        let drop = msg.attempts >= DROP_AFTER_ATTEMPTS
            || now.saturating_sub(msg.created_at) >= DROP_AFTER_SECS;
        if drop {
            file.queued.remove(position);
            self.save(&file)?;
            return Ok(true);
        }
        msg.next_attempt_at = now.saturating_add(backoff(msg.attempts));
        self.save(&file)?;
        Ok(false)
    }

    /// The getUpdates offset survives restarts, so updates are processed once.
    pub fn offset(&self) -> Result<i64, String> {
        let _guard = lock();
        Ok(self.load()?.offset)
    }

    /// Store the next getUpdates offset.
    pub fn set_offset(&self, offset: i64) -> Result<(), String> {
        let _guard = lock();
        let mut file = self.load()?;
        file.offset = offset;
        self.save(&file)
    }

    /// Drop `sent` keys older than 7 days.
    pub fn prune(&self, now: i64) -> Result<usize, String> {
        let _guard = lock();
        let mut file = self.load()?;
        let before = file.sent.len();
        file.sent
            .retain(|_, sent_at| now.saturating_sub(*sent_at) <= SENT_TTL_SECS);
        let removed = before - file.sent.len();
        if removed > 0 {
            self.save(&file)?;
        }
        Ok(removed)
    }

    fn load(&self) -> Result<OutFile, String> {
        let path = self.dir.join("outbox.json");
        let mut file = super::read_json::<OutFile>(&path)?.unwrap_or_default();
        if file.version == 0 {
            file.version = 1;
        }
        if file.version != 1 {
            return Err(format!("unsupported outbox version {}", file.version));
        }
        Ok(file)
    }

    fn save(&self, file: &OutFile) -> Result<(), String> {
        super::write_json(&self.dir.join("outbox.json"), file)
    }
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    match OUTBOX_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn backoff(attempts: u32) -> i64 {
    match attempts {
        1 => 5,
        2 => 30,
        3 => 120,
        4 => 600,
        _ => 1800,
    }
}
