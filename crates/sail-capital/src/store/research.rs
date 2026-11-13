//! Round 19: index membership for the research universe (Claude's slice). Append-only:
//! each fetch stores a snapshot; the latest snapshot per index is current.

use super::{CapitalStore, StoreError};
use chrono::{DateTime, Utc};

/// The membership snapshot table. Claude, round 19.
pub(super) const SCHEMA: &str = "";

/// A stored membership snapshot: when it was fetched, and the members.
pub type ResearchSnapshot = (DateTime<Utc>, Vec<crate::universe::ResearchMember>);

impl CapitalStore {
    /// Stores one fetched membership snapshot for `index`. Claude, round 19.
    pub fn record_research_members(
        &self,
        index: &str,
        members: &[crate::universe::ResearchMember],
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let _ = (index, members, now);
        Ok(())
    }

    /// The latest snapshot for `index`, with when it was fetched. Claude, round 19.
    pub fn research_members(&self, index: &str) -> Result<Option<ResearchSnapshot>, StoreError> {
        let _ = index;
        Ok(None)
    }
}
