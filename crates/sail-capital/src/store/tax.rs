//! Round 25 B7: the owner's tax profile, one row per account, last write wins.

use super::{CapitalStore, StoreError, decode, encode};
use crate::contracts::AccountId;
use crate::dividends::tax::TaxProfile;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

/// The tax-profile table.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tax_profiles (
    account TEXT PRIMARY KEY,
    payload TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
";

impl CapitalStore {
    /// The saved profile, if the owner set one.
    pub fn tax_profile(&self, account: &AccountId) -> Result<Option<TaxProfile>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let payload: Option<String> = conn
            .query_row(
                "SELECT payload FROM tax_profiles WHERE account = ?1",
                params![account.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        payload.map(|p| decode(&p)).transpose()
    }

    /// Saves the profile, replacing any earlier one.
    pub fn set_tax_profile(
        &self,
        account: &AccountId,
        profile: &TaxProfile,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        conn.execute(
            "INSERT INTO tax_profiles (account, payload, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(account) DO UPDATE SET payload = excluded.payload,
                                                updated_at = excluded.updated_at",
            params![account.as_str(), encode(profile)?, now.to_rfc3339()],
        )?;
        Ok(())
    }
}
