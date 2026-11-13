//! Round 19: the replay cache's stored half (Kilo's slice). A **cache**, not a record:
//! one row per instrument key and cost tag, replaced on each run (`INSERT OR REPLACE`),
//! never read as evidence of what SAIL published. Deleting the table only costs one slow
//! run.

use super::{CapitalStore, StoreError, decode};
use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::io;

/// The cache table. Kilo, round 19: `trade_replays (key TEXT, costs_tag TEXT, cut TEXT,
/// prefix_hash TEXT, payload TEXT /* Vec<Trade> JSON */, stored_at TEXT,
/// PRIMARY KEY (key, costs_tag))`. No append-only triggers.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS trade_replays (
    key TEXT NOT NULL,
    costs_tag TEXT NOT NULL,
    cut TEXT NOT NULL,
    prefix_hash TEXT NOT NULL,
    payload TEXT NOT NULL,
    stored_at TEXT NOT NULL,
    PRIMARY KEY (key, costs_tag)
);
";

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
        let conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let row: Option<(String, String, String)> = conn
            .query_row(
                "SELECT cut, prefix_hash, payload FROM trade_replays
                 WHERE key = ?1 AND costs_tag = ?2",
                params![key, costs_tag],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        row.map(|(cut, prefix_hash, payload)| {
            let cut = NaiveDate::parse_from_str(&cut, "%Y-%m-%d")
                .map_err(|_| StoreError::Decode(format!("bad replay cut {cut}")))?;
            Ok(ReplayCache {
                key: key.to_string(),
                costs_tag: costs_tag.to_string(),
                cut,
                prefix_hash,
                trades: decode(&payload)?,
            })
        })
        .transpose()
    }

    /// Stores (replaces) the prefixes in one transaction. Kilo, round 19.
    pub fn record_replay_caches(
        &self,
        caches: &[ReplayCache],
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO trade_replays
                 (key, costs_tag, cut, prefix_hash, payload, stored_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for cache in caches {
                stmt.execute(params![
                    cache.key,
                    cache.costs_tag,
                    cache.cut.format("%Y-%m-%d").to_string(),
                    cache.prefix_hash,
                    encode_trades(&cache.trades)?,
                    now.to_rfc3339()
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

/// JSON with every `f64` written with 25 significant digits. serde_json's parser can
/// lose the last bit on a shortest float, and the cache must return the exact trades.
fn encode_trades(trades: &[crate::trade::Trade]) -> Result<String, StoreError> {
    let mut bytes = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut bytes, ExactFloats);
    trades
        .serialize(&mut serializer)
        .map_err(|error| StoreError::Decode(error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| StoreError::Decode(error.to_string()))
}

struct ExactFloats;

impl serde_json::ser::Formatter for ExactFloats {
    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if value.is_finite() {
            write!(writer, "{value:.24e}")
        } else {
            writer.write_all(b"null")
        }
    }
}
