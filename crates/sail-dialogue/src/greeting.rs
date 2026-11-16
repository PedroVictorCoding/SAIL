//! Deterministic arrival greeting.
//!
//! Composed from held announcements and the return briefing only: no model call and no
//! invented clause. Every clause is taken verbatim from a data field. K4 implements the
//! composition rules; this module holds the fixed contract.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use sail_pulse::attention::AttentionItem;

use crate::briefing::ReturnBriefing;

/// Greeting language. serde: `"en" | "pt_pt"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang {
    En,
    PtPt,
}

/// What to say, and which held items it mentioned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Greeting {
    pub spoken: String,
    pub item_ids: Vec<String>,
}

/// Longest spoken greeting, in characters (fixed a priori: two short sentences).
pub const MAX_SPOKEN_CHARS: usize = 220;

/// Longest data clause, in characters, trimmed at a word boundary.
pub const MAX_CLAUSE_CHARS: usize = 80;

/// Compose the greeting. Pure. `local_hour` is the owner's local hour (0..=23) at `now`.
pub fn compose(
    _briefing: &ReturnBriefing,
    _held: &[AttentionItem],
    _away_for_secs: u64,
    _now: DateTime<Utc>,
    _local_hour: u32,
    _lang: Lang,
) -> Greeting {
    Greeting {
        spoken: String::new(),
        item_ids: Vec::new(),
    }
}
