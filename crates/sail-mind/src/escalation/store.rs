//! Durable escalation ledger (Kilo). Spec: the module doc in `escalation/mod.rs`.
use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use super::{
    Decision, DecisionRecord, EscalationJob, EscalationPolicy, JobState, Outcome, OutcomeStats,
    UsageTotals,
};

pub struct EscalationStore {
    #[allow(dead_code)]
    conn: Mutex<Connection>,
}

impl EscalationStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let _ = path;
        unimplemented!("Kilo: EscalationStore::open")
    }
    pub fn open_in_memory() -> Result<Self, String> {
        unimplemented!("Kilo: EscalationStore::open_in_memory")
    }
    /// Totals for `now`'s UTC day/month, rate-limit blocks still in force, in-flight
    /// fingerprints, and fingerprints finished within `policy.cooldown_secs`.
    pub fn usage(
        &self,
        policy: &EscalationPolicy,
        now: DateTime<Utc>,
    ) -> Result<UsageTotals, String> {
        let _ = (policy, now);
        unimplemented!("Kilo: EscalationStore::usage")
    }
    pub fn outcomes(&self) -> Result<OutcomeStats, String> {
        unimplemented!("Kilo: EscalationStore::outcomes")
    }
    /// A `Done` job with this fingerprint finished within `ttl_secs` of `now`.
    pub fn cached(
        &self,
        fingerprint: &str,
        ttl_secs: u64,
        now: DateTime<Utc>,
    ) -> Result<Option<EscalationJob>, String> {
        let _ = (fingerprint, ttl_secs, now);
        unimplemented!("Kilo: EscalationStore::cached")
    }
    /// Insert a new job (state `Pending`). Text fields are cut to `MAX_STORED_BYTES`.
    pub fn insert_job(&self, job: &EscalationJob) -> Result<(), String> {
        let _ = job;
        unimplemented!("Kilo: EscalationStore::insert_job")
    }
    /// Move a job to `Running` and increment `attempts`.
    pub fn mark_running(&self, id: &str) -> Result<(), String> {
        let _ = id;
        unimplemented!("Kilo: EscalationStore::mark_running")
    }
    /// Finish a job: `Done` with answer and usage, which also writes one `usage` row.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_job(
        &self,
        id: &str,
        answer: &str,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cost_micros: u64,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        let _ = (
            id,
            answer,
            model,
            input_tokens,
            output_tokens,
            cost_micros,
            now,
        );
        unimplemented!("Kilo: EscalationStore::finish_job")
    }
    /// `Failed` (or back to `Pending` when `retry` and attempts < MAX_ATTEMPTS).
    pub fn fail_job(
        &self,
        id: &str,
        error: &str,
        retry: bool,
        now: DateTime<Utc>,
    ) -> Result<JobState, String> {
        let _ = (id, error, retry, now);
        unimplemented!("Kilo: EscalationStore::fail_job")
    }
    /// Record a provider rate limit until `until`.
    pub fn block_provider(&self, provider: &str, until: DateTime<Utc>) -> Result<(), String> {
        let _ = (provider, until);
        unimplemented!("Kilo: EscalationStore::block_provider")
    }
    pub fn job(&self, id: &str) -> Result<Option<EscalationJob>, String> {
        let _ = id;
        unimplemented!("Kilo: EscalationStore::job")
    }
    /// Newest first.
    pub fn jobs(&self, limit: usize) -> Result<Vec<EscalationJob>, String> {
        let _ = limit;
        unimplemented!("Kilo: EscalationStore::jobs")
    }
    /// Jobs left `Running` by a previous process go back to `Pending`; returns pending jobs.
    pub fn recover(&self) -> Result<Vec<EscalationJob>, String> {
        unimplemented!("Kilo: EscalationStore::recover")
    }
    /// Set (or replace) a job's outcome. Unknown job → error.
    pub fn record_outcome(&self, id: &str, outcome: Outcome) -> Result<(), String> {
        let _ = (id, outcome);
        unimplemented!("Kilo: EscalationStore::record_outcome")
    }
    pub fn log_decision(&self, record: &DecisionRecord) -> Result<(), String> {
        let _ = record;
        unimplemented!("Kilo: EscalationStore::log_decision")
    }
    /// Newest first.
    pub fn decisions(&self, limit: usize) -> Result<Vec<DecisionRecord>, String> {
        let _ = limit;
        unimplemented!("Kilo: EscalationStore::decisions")
    }
    /// Delete usage/decision rows older than `keep_days` and expired cache rows.
    pub fn prune(&self, keep_days: u32, now: DateTime<Utc>) -> Result<(), String> {
        let _ = (keep_days, now);
        unimplemented!("Kilo: EscalationStore::prune")
    }
}

#[allow(dead_code)]
fn _decision_is_serializable(decision: &Decision) -> String {
    serde_json::to_string(decision).unwrap_or_default()
}
