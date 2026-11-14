//! Who may talk to SAIL: paired senders, one-time pairing codes, policy and lockout.
//!
//! Pairing state lives in `<dir>/pairing.json` (0600, written atomically under one
//! process-wide lock). A stranger never reaches a conversation: until the owner approves
//! the code shown in the web UI, [`PairingStore::admit`] only returns a code or
//! [`Admission::Ignore`].
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Characters used for pairing codes; excludes 0/O/1/I/L.
pub const CODE_ALPHABET: &str = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";
/// Pairing code length.
pub const CODE_LEN: usize = 8;
/// How long an unused pairing code stays live.
pub const CODE_TTL_SECS: i64 = 3600;
/// The most waiting pairing codes.
pub const MAX_PENDING: usize = 3;
/// The minimum time between code requests from one sender.
pub const REQUEST_COOLDOWN_SECS: i64 = 600;
/// Wrong approvals before approvals are locked.
pub const MAX_FAILED_APPROVALS: u32 = 5;

/// One owner-approved sender.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Paired {
    /// Telegram user id.
    pub sender_id: i64,
    /// Private chat id the sender talks from.
    pub chat_id: i64,
    /// Display name captured at pairing time.
    pub name: String,
    /// Unix seconds when the owner approved the code.
    pub paired_at: i64,
}

/// One waiting pairing code shown to the owner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingPair {
    /// The code shown to the owner.
    pub code: String,
    /// Telegram user id asking to be paired.
    pub sender_id: i64,
    /// Private chat id the request came from.
    pub chat_id: i64,
    /// Display name captured from Telegram.
    pub name: String,
    /// Unix seconds when the code was issued.
    pub requested_at: i64,
}

/// What to do with a message from `sender_id`.
#[derive(Debug, Clone, PartialEq)]
pub enum Admission {
    /// Paired: handle it.
    Allowed,
    /// New code issued: reply with it (only once per cooldown).
    NewCode(String),
    /// Already has a live code, or in cooldown, or pending list full, or locked out: say nothing.
    Ignore,
    /// The owner chose "decline unknown senders": reply once with a fixed refusal, then Ignore.
    Decline,
}

/// The owner's view of pairing state.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PairingView {
    /// Owner-approved senders.
    pub paired: Vec<Paired>,
    /// Live pairing codes.
    pub pending: Vec<PendingPair>,
    /// "pair", "ignore" or "decline".
    pub policy: String,
    /// Unix seconds an approval lockout ends, when one is active.
    pub locked_until: Option<i64>,
}

/// `<dir>/pairing.json` (0o600): paired senders, live codes, lockout and policy.
pub struct PairingStore {
    pub dir: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct PairingFile {
    version: u32,
    paired: Vec<Paired>,
    pending: Vec<PendingPair>,
    failed_approvals: u32,
    locked_until: Option<i64>,
    policy: String,
    declined: Vec<i64>,
}

impl Default for PairingFile {
    fn default() -> Self {
        Self {
            version: 1,
            paired: Vec::new(),
            pending: Vec::new(),
            failed_approvals: 0,
            locked_until: None,
            policy: "pair".to_string(),
            declined: Vec::new(),
        }
    }
}

static PAIRING_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static LAST_CODE_AT: LazyLock<Mutex<HashMap<PathBuf, HashMap<i64, i64>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

impl PairingStore {
    /// Answer one incoming stranger. `random` supplies `CODE_LEN` bytes for a new code;
    /// callers pass OS randomness, tests pass fixed bytes.
    pub fn admit(
        &self,
        sender_id: i64,
        chat_id: i64,
        name: &str,
        now: i64,
        random: [u8; CODE_LEN],
    ) -> Result<Admission, String> {
        let _guard = lock();
        let mut file = self.load()?;
        let mut changed = expire_pending(&mut file, now);
        if let Some(until) = file.locked_until
            && now >= until
        {
            file.locked_until = None;
            file.failed_approvals = 0;
            changed = true;
        }
        if file.paired.iter().any(|pair| pair.sender_id == sender_id) {
            if changed {
                self.save(&file)?;
            }
            return Ok(Admission::Allowed);
        }
        if file.locked_until.is_some() {
            if changed {
                self.save(&file)?;
            }
            return Ok(Admission::Ignore);
        }
        match file.policy.as_str() {
            "ignore" => {
                if changed {
                    self.save(&file)?;
                }
                return Ok(Admission::Ignore);
            }
            "decline" => {
                if file.declined.contains(&sender_id) {
                    if changed {
                        self.save(&file)?;
                    }
                    return Ok(Admission::Ignore);
                }
                file.declined.push(sender_id);
                self.save(&file)?;
                return Ok(Admission::Decline);
            }
            _ => {}
        }
        if file
            .pending
            .iter()
            .any(|pending| pending.sender_id == sender_id)
        {
            if changed {
                self.save(&file)?;
            }
            return Ok(Admission::Ignore);
        }
        if file.pending.len() >= MAX_PENDING {
            if changed {
                self.save(&file)?;
            }
            return Ok(Admission::Ignore);
        }
        if in_cooldown(&self.dir, sender_id, now) {
            if changed {
                self.save(&file)?;
            }
            return Ok(Admission::Ignore);
        }
        let code = make_code(random);
        let name = clean_name(name);
        file.pending.push(PendingPair {
            code: code.clone(),
            sender_id,
            chat_id,
            name,
            requested_at: now,
        });
        remember_request(&self.dir, sender_id, now);
        self.save(&file)?;
        Ok(Admission::NewCode(code))
    }

    /// Owner approves a code typed in the web UI (case-insensitive, spaces ignored).
    pub fn approve(&self, code: &str, now: i64) -> Result<Paired, String> {
        let _guard = lock();
        let mut file = self.load()?;
        if expire_pending(&mut file, now) {
            self.save(&file)?;
        }
        if let Some(until) = file.locked_until {
            if now < until {
                return Err("too many incorrect codes; approvals are locked for now".to_string());
            }
            file.locked_until = None;
            file.failed_approvals = 0;
            self.save(&file)?;
        }
        let normalized = normalize_code(code);
        if let Some(position) = file.pending.iter().position(|p| p.code == normalized) {
            let pending = file.pending.remove(position);
            file.paired
                .retain(|pair| pair.sender_id != pending.sender_id);
            let paired = Paired {
                sender_id: pending.sender_id,
                chat_id: pending.chat_id,
                name: pending.name,
                paired_at: now,
            };
            file.paired.push(paired.clone());
            file.failed_approvals = 0;
            file.locked_until = None;
            self.save(&file)?;
            return Ok(paired);
        }
        file.failed_approvals = file.failed_approvals.saturating_add(1);
        if file.failed_approvals >= MAX_FAILED_APPROVALS {
            file.locked_until = Some(now.saturating_add(CODE_TTL_SECS));
        }
        self.save(&file)?;
        Err("that code is not valid or has expired".to_string())
    }

    /// Forget one waiting code without pairing the sender.
    pub fn deny(&self, code: &str) -> Result<(), String> {
        let _guard = lock();
        let mut file = self.load()?;
        let normalized = normalize_code(code);
        let Some(position) = file.pending.iter().position(|p| p.code == normalized) else {
            return Err("that code is not waiting".to_string());
        };
        file.pending.remove(position);
        self.save(&file)
    }

    /// Remove one person's pairing.
    pub fn revoke(&self, sender_id: i64) -> Result<(), String> {
        let _guard = lock();
        let mut file = self.load()?;
        let before = file.paired.len();
        file.paired.retain(|pair| pair.sender_id != sender_id);
        if file.paired.len() == before {
            return Err("that sender is not paired".to_string());
        }
        self.save(&file)
    }

    /// Set what unknown senders get: "pair" (default), "ignore" or "decline".
    pub fn set_policy(&self, policy: &str) -> Result<(), String> {
        let policy = policy.trim().to_ascii_lowercase();
        if !matches!(policy.as_str(), "pair" | "ignore" | "decline") {
            return Err("policy must be pair, ignore or decline".to_string());
        }
        let _guard = lock();
        let mut file = self.load()?;
        file.policy = policy;
        self.save(&file)
    }

    /// The owner's view: paired senders, live codes, policy and an active lockout.
    pub fn snapshot(&self, now: i64) -> Result<PairingView, String> {
        let _guard = lock();
        let file = self.load()?;
        let pending = file
            .pending
            .into_iter()
            .filter(|pending| live(pending, now))
            .collect();
        let locked_until = file.locked_until.filter(|until| now < *until);
        Ok(PairingView {
            paired: file.paired,
            pending,
            policy: file.policy,
            locked_until,
        })
    }

    /// Whether this sender already passed pairing.
    pub fn is_paired(&self, sender_id: i64) -> Result<bool, String> {
        let _guard = lock();
        Ok(self
            .load()?
            .paired
            .iter()
            .any(|pair| pair.sender_id == sender_id))
    }

    fn load(&self) -> Result<PairingFile, String> {
        let path = self.dir.join("pairing.json");
        let file = super::read_json::<PairingFile>(&path)?.unwrap_or_default();
        if file.version != 1 {
            return Err(format!("unsupported pairing version {}", file.version));
        }
        Ok(file)
    }

    fn save(&self, file: &PairingFile) -> Result<(), String> {
        super::write_json(&self.dir.join("pairing.json"), file)
    }
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    match PAIRING_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn expire_pending(file: &mut PairingFile, now: i64) -> bool {
    let before = file.pending.len();
    file.pending.retain(|pending| live(pending, now));
    file.pending.len() != before
}

fn live(pending: &PendingPair, now: i64) -> bool {
    now < pending.requested_at.saturating_add(CODE_TTL_SECS)
}

fn make_code(random: [u8; CODE_LEN]) -> String {
    let alphabet = CODE_ALPHABET.as_bytes();
    random
        .iter()
        .map(|byte| alphabet[*byte as usize % alphabet.len()] as char)
        .collect()
}

fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_uppercase)
        .collect()
}

fn clean_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return "Telegram user".to_string();
    }
    name.chars().take(64).collect()
}

fn in_cooldown(dir: &Path, sender_id: i64, now: i64) -> bool {
    let Ok(mut map) = LAST_CODE_AT.lock() else {
        return false;
    };
    let Some(senders) = map.get_mut(dir) else {
        return false;
    };
    match senders.get(&sender_id).copied() {
        Some(last) if now.saturating_sub(last) < REQUEST_COOLDOWN_SECS => true,
        Some(_) => {
            senders.remove(&sender_id);
            false
        }
        None => false,
    }
}

fn remember_request(dir: &Path, sender_id: i64, now: i64) {
    if let Ok(mut map) = LAST_CODE_AT.lock() {
        map.entry(dir.to_path_buf())
            .or_default()
            .insert(sender_id, now);
    }
}
