//! The always-in-context profile block and the memory review queue (P5, round 22).
//!
//! * **Profile block.** A short "About the owner" block (≤ 1,400 characters) built by
//!   `sail_arena::knowledge::profile::build` from the owner's active, normal-sensitivity
//!   claims: pinned lines first, then explicit preferences, then the best-supported facts.
//!   It is built once per conversation and then frozen, so the prompt prefix stays stable;
//!   retrieval still supplies task-specific facts on every turn.
//! * **Pins.** The owner pins individual lines (`$SAIL_DATA/memory/profile-pins.json`).
//! * **Review queue.** With review on, model-proposed memory changes wait in
//!   `$SAIL_DATA/memory/memory-review.json` until the owner saves or discards them; saving
//!   still goes through Arena's deterministic validator (`sail_flow::memory`).
use sail_arena::{
    MemoryStore,
    knowledge::{
        Access, ClaimObject, EpistemicStatus, PreferenceBasis,
        changeset::OWNER,
        profile::{self, PROFILE_BUDGET_CHARS, ProfileBlock, ProfileCandidate},
        review::ReviewQueue,
    },
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    io::Write,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

const FROZEN_SESSIONS: usize = 256;

/// `$SAIL_DATA/memory`, where pins and the review queue live.
pub fn memory_dir() -> PathBuf {
    sail_common::storage::data_path("memory")
}

/// The owner's review queue.
pub fn review_queue() -> ReviewQueue {
    ReviewQueue { dir: memory_dir() }
}

fn pins_path() -> PathBuf {
    memory_dir().join("profile-pins.json")
}

/// Claim ids the owner pinned into the profile.
pub fn pins() -> BTreeSet<i64> {
    std::fs::read(pins_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Pin or unpin one claim (owner action).
pub fn set_pin(claim_id: i64, pinned: bool) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut set = pins();
    if pinned {
        set.insert(claim_id);
    } else {
        set.remove(&claim_id);
    }
    let path = pins_path();
    std::fs::create_dir_all(memory_dir()).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(&set).map_err(|e| e.to_string())?)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// The owner's active claims, offered to the profile builder.
pub fn candidates(store: &MemoryStore, access: &Access) -> Vec<ProfileCandidate> {
    let k = store.knowledge();
    let pins = pins();
    let mut out = Vec::new();
    let owners = k.find_entities(OWNER, None, access).unwrap_or_default();
    for owner in owners {
        for claim in k.claims_about(owner.id, access, false).unwrap_or_default() {
            let value = match &claim.object {
                ClaimObject::Value(v) => v.clone(),
                ClaimObject::Entity(id) => match k.entity(*id, access) {
                    Ok(Some(e)) => e.name,
                    _ => continue,
                },
            };
            let support = k
                .support(claim.id, access)
                .map(|s| s.direct_origins + s.user_origins)
                .unwrap_or(0);
            out.push(ProfileCandidate {
                claim_id: claim.id,
                text: format!("{}: {value}", claim.predicate.replace('_', " ")),
                category: claim.category,
                status: claim.status,
                sensitivity: claim.sensitivity,
                basis: claim.basis,
                pinned: pins.contains(&claim.id),
                support,
                last_verified_at: claim.last_verified_at,
                updated_at: claim.updated_at,
            });
        }
    }
    out
}

/// The profile block as it would be built now.
pub fn current(store: &MemoryStore, access: &Access) -> (ProfileBlock, Vec<ProfileCandidate>) {
    let candidates = candidates(store, access);
    let block = profile::build(
        &candidates,
        PROFILE_BUDGET_CHARS,
        chrono::Utc::now().timestamp_millis(),
    );
    (block, candidates)
}

/// The block for one conversation: built on its first turn, then frozen.
pub fn for_session(store: &MemoryStore, access: &Access, session: &str) -> Option<String> {
    static FROZEN: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let frozen = FROZEN.get_or_init(Default::default);
    if let Some(text) = frozen.lock().unwrap().get(session) {
        return (!text.is_empty()).then(|| text.clone());
    }
    let (block, _) = current(store, access);
    let mut map = frozen.lock().unwrap();
    if map.len() >= FROZEN_SESSIONS {
        map.clear();
    }
    map.insert(session.to_string(), block.text.clone());
    (!block.text.is_empty()).then_some(format!("{}\n", block.text))
}

/// The Memory › Review view: switch, pending changes, and the profile with why each line is there.
pub fn review_view(store: &MemoryStore, access: &Access) -> Result<Value, String> {
    let queue = review_queue();
    let (block, candidates) = current(store, access);
    let lines: Vec<Value> = block
        .claim_ids
        .iter()
        .filter_map(|id| candidates.iter().find(|c| c.claim_id == *id))
        .map(|c| {
            let why = if c.pinned {
                "pinned"
            } else if c.basis == Some(PreferenceBasis::Explicit) {
                "explicit"
            } else if c.status == EpistemicStatus::Verified {
                "verified"
            } else {
                "reported"
            };
            json!({"claim_id": c.claim_id, "text": c.text, "why": why, "pinned": c.pinned})
        })
        .collect();
    Ok(json!({
        "enabled": queue.enabled()?,
        "pending": queue.list()?,
        "profile": {
            "text": block.text,
            "chars": block.text.chars().count(),
            "budget": PROFILE_BUDGET_CHARS,
            "lines": lines,
        },
    }))
}
