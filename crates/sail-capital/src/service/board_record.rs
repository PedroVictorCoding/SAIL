//! Recording and grading the signal board's reads ([`crate::board_record`]).

use super::{CapitalService, ServiceError};
use crate::board_record::{BoardRecord, grade, snapshot, summarize};
use crate::contracts::AccountId;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use std::collections::BTreeMap;

impl CapitalService {
    /// Stores today's board reads unless this session is already stored. Returns how many
    /// were new. Called by the background warm-up after it builds the board.
    pub fn record_board_snapshot(
        &self,
        account: &AccountId,
        now: DateTime<Utc>,
    ) -> Result<u32, ServiceError> {
        let board = self.signal_board(account)?;
        let Some(session) = board.session else {
            return Ok(0);
        };
        if self.store.latest_board_session(account)? == Some(session) {
            return Ok(0);
        }
        self.store
            .record_board_reads(account, &snapshot(&board), now)
            .map_err(ServiceError::from)
    }

    /// The graded record. Cached until the stored reads or prices change.
    pub fn board_record(&self, account: &AccountId) -> Result<BoardRecord, ServiceError> {
        let reads = self.store.board_reads(account)?;
        let version = format!(
            "{}|{}|{}",
            account.as_str(),
            self.store.price_version()?,
            reads.len()
        );
        if let Ok(cache) = self.board_record_cache.lock()
            && let Some((cached, record)) = cache.as_ref()
            && *cached == version
        {
            return Ok(record.clone());
        }

        let mut pairs: Vec<(String, String)> = Vec::new();
        for read in &reads {
            if !pairs.iter().any(|(key, _)| key == &read.key) {
                pairs.push((read.key.clone(), read.symbol.clone()));
            }
        }
        let data = self.closes_for(&pairs, Utc::now())?;
        let mut closes: BTreeMap<String, Vec<(NaiveDate, f64)>> = BTreeMap::new();
        for (key, _, series) in data {
            closes.insert(
                key,
                series
                    .into_iter()
                    .filter_map(|(session, close)| Some((session, close.to_f64()?)))
                    .collect(),
            );
        }
        let graded = grade(&reads, &|key| closes.get(key).cloned().unwrap_or_default());
        let record = summarize(&reads, &graded);
        if let Ok(mut cache) = self.board_record_cache.lock() {
            *cache = Some((version, record.clone()));
        }
        Ok(record)
    }
}
