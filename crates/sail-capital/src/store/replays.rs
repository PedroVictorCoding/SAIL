//! Round 19: the replay cache's stored half (Kilo's slice). A **cache**, not a record:
//! one row per instrument key and cost tag, replaced on each run (`INSERT OR REPLACE`),
//! never read as evidence of what SAIL published. Deleting the table only costs one slow
//! run.

use super::{CapitalStore, StoreError};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// The cache table. Kilo, round 19: `trade_replays (key TEXT, costs_tag TEXT, cut TEXT,
/// prefix_hash TEXT, payload TEXT /* Vec<Trade> JSON */, stored_at TEXT,
/// PRIMARY KEY (key, costs_tag))`. No append-only triggers.
pub(super) const SCHEMA: &str = "";

/// One instrument's cached replay prefix.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayCache {
    /// Instrument key.
    pub key: String,
    /// [`crate::trade::cache::costs_tag`].
    pub costs_tag: String,
    /// Trades signalled before this session are cached.
    pub cut: NaiveDate,
    /// [`crate::trade::cache::prefix_hash`] of the bars before `cut`.
    pub prefix_hash: String,
    /// The trades with `signal_session < cut`, in replay order.
    pub trades: Vec<crate::trade::Trade>,
}

impl CapitalStore {
    /// The cached prefix for `key` and `costs_tag`. Kilo, round 19.
    pub fn replay_cache(
        &self,
        key: &str,
        costs_tag: &str,
    ) -> Result<Option<ReplayCache>, StoreError> {
        let _ = (key, costs_tag);
        Ok(None)
    }

    /// Stores (replaces) the prefixes in one transaction. Kilo, round 19.
    pub fn record_replay_caches(
        &self,
        caches: &[ReplayCache],
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let _ = (caches, now);
        Ok(())
    }
}
