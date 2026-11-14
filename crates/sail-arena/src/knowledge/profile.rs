//! The small profile block SAIL keeps in every conversation.
//!
//! It is a pure, bounded rendering of a few strong claims from the knowledge graph:
//! pinned first, then explicit preferences, then well-supported facts. Sessions freeze
//! the block by its SHA-256 hash, so a conversation never sees it change under it.
use super::memory_scan;
use super::model::{ClaimCategory, EpistemicStatus, PreferenceBasis, Sensitivity};
use ring::digest;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt::Write as _;

/// The maximum size of the block, including its header.
pub const PROFILE_BUDGET_CHARS: usize = 1400;

/// One active claim offered for the block. Claude fills these from the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileCandidate {
    pub claim_id: i64,
    /// A one-line rendering, e.g. "Prefers replies in Portuguese".
    pub text: String,
    pub category: ClaimCategory,
    pub status: EpistemicStatus,
    pub sensitivity: Sensitivity,
    pub basis: Option<PreferenceBasis>,
    /// Owner pinned it (always first).
    pub pinned: bool,
    /// Distinct supporting origins (from `Support`): direct + user.
    pub support: usize,
    pub last_verified_at: Option<i64>,
    pub updated_at: i64,
}

/// The rendered block and what went into it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileBlock {
    /// "About the owner (from SAIL's memory):\n- …\n- …"; empty when no line qualifies.
    pub text: String,
    pub claim_ids: Vec<i64>,
    /// SHA-256 hex of `text`; sessions freeze the block by this hash.
    pub hash: String,
    pub omitted: usize,
}

/// Pure. Builds the block from eligible candidates, without I/O or a wall clock.
pub fn build(candidates: &[ProfileCandidate], budget: usize, now: i64) -> ProfileBlock {
    let mut eligible: Vec<&ProfileCandidate> = candidates
        .iter()
        .filter(|candidate| qualifies(candidate))
        .collect();
    eligible.sort_by(|left, right| compare(left, right, now));

    let mut seen = HashSet::new();
    let mut lines: Vec<(i64, String)> = Vec::new();
    for candidate in eligible {
        let line = render(&candidate.text);
        if line.is_empty() || !seen.insert(line.to_lowercase()) {
            continue;
        }
        if !memory_scan::scan(&line).is_empty() {
            continue;
        }
        lines.push((candidate.claim_id, line));
    }

    let header = "About the owner (from SAIL's memory):";
    let mut text = String::new();
    let mut claim_ids = Vec::new();
    let mut omitted = 0;
    for (index, (claim_id, line)) in lines.iter().enumerate() {
        let addition = if index == 0 {
            format!("{header}\n- {line}")
        } else {
            format!("\n- {line}")
        };
        if text.chars().count() + addition.chars().count() <= budget {
            text.push_str(&addition);
            claim_ids.push(*claim_id);
        } else {
            omitted = lines.len() - index;
            break;
        }
    }
    if claim_ids.is_empty() {
        text.clear();
    }
    let hash = sha256_hex(&text);
    ProfileBlock {
        text,
        claim_ids,
        hash,
        omitted,
    }
}

fn qualifies(candidate: &ProfileCandidate) -> bool {
    if candidate.sensitivity != Sensitivity::Normal {
        return false;
    }
    if !matches!(
        candidate.category,
        ClaimCategory::Fact | ClaimCategory::Preference | ClaimCategory::Capability
    ) {
        return false;
    }
    match candidate.status {
        EpistemicStatus::Verified | EpistemicStatus::UserReported => true,
        EpistemicStatus::Inferred => {
            candidate.basis == Some(PreferenceBasis::Explicit) || candidate.support >= 2
        }
        _ => false,
    }
}

fn compare(left: &ProfileCandidate, right: &ProfileCandidate, now: i64) -> std::cmp::Ordering {
    right
        .pinned
        .cmp(&left.pinned)
        .then_with(|| explicit_preference(right).cmp(&explicit_preference(left)))
        .then_with(|| right.support.cmp(&left.support))
        .then_with(|| fresh(right, now).cmp(&fresh(left, now)))
        .then_with(|| {
            right
                .last_verified_at
                .unwrap_or(i64::MIN)
                .cmp(&left.last_verified_at.unwrap_or(i64::MIN))
        })
        .then_with(|| right.updated_at.cmp(&left.updated_at))
        .then_with(|| left.claim_id.cmp(&right.claim_id))
}

fn explicit_preference(candidate: &ProfileCandidate) -> bool {
    candidate.category == ClaimCategory::Preference
        && candidate.basis == Some(PreferenceBasis::Explicit)
}

fn fresh(candidate: &ProfileCandidate, now: i64) -> bool {
    candidate
        .last_verified_at
        .is_some_and(|verified| now.saturating_sub(verified) <= 365 * 24 * 60 * 60)
}

fn render(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|character| match character {
            '\n' | '\r' => ' ',
            other => other,
        })
        .collect();
    let line = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    line.chars().take(160).collect()
}

fn sha256_hex(text: &str) -> String {
    let digest = digest::digest(&digest::SHA256, text.as_bytes());
    let mut out = String::with_capacity(digest.as_ref().len() * 2);
    for byte in digest.as_ref() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
