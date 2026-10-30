use chrono::{DateTime, Utc};
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use tracing::info;
use uuid::Uuid;

use crate::capability::Capability;
use sail_common::types::SubsystemId;

pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuditIntegrityError {
    #[error("Audit log file I/O error: {0}")]
    Io(String),
    #[error("Corrupt audit record at sequence {sequence}: {detail}")]
    CorruptEntry { sequence: u64, detail: String },
    #[error("Broken hash chain at sequence {sequence}: expected prev_hash {expected}, found {actual}")]
    BrokenHashChain {
        sequence: u64,
        expected: String,
        actual: String,
    },
    #[error("Tampered audit entry at sequence {sequence}: computed hash {computed} does not match recorded {recorded}")]
    TamperedEntry {
        sequence: u64,
        computed: String,
        recorded: String,
    },
    #[error("Sequence numbering broken at entry index {entry_idx}: expected {expected}, found {actual}")]
    SequenceBroken {
        entry_idx: usize,
        expected: u64,
        actual: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    Granted,
    Denied,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditReceipt {
    pub sequence: u64,
    pub receipt_id: Uuid,
    pub correlation_id: Option<Uuid>,
    pub timestamp: DateTime<Utc>,
    pub requester: SubsystemId,
    pub capability: Capability,
    pub outcome: DecisionOutcome,
    pub reason: String,
    pub prev_hash: String,
    pub hash: String,
}

pub struct AuditLedger {
    receipts: RwLock<Vec<AuditReceipt>>,
    last_hash: RwLock<String>,
    next_sequence: RwLock<u64>,
    log_file: Mutex<Option<File>>,
    log_path: Option<PathBuf>,
}

impl AuditLedger {
    pub fn new() -> Self {
        Self {
            receipts: RwLock::new(Vec::new()),
            last_hash: RwLock::new(GENESIS_HASH.to_string()),
            next_sequence: RwLock::new(1),
            log_file: Mutex::new(None),
            log_path: None,
        }
    }

    /// Open or initialize a durable, append-only tamper-evident audit ledger on disk.
    /// Verifies the cryptographic chain and monotonic sequence on startup (boot-time verification).
    pub fn open_persistent(log_path: &Path) -> Result<Self, AuditIntegrityError> {
        if let Some(parent) = log_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let mut receipts = Vec::new();
        let mut last_hash = GENESIS_HASH.to_string();
        let mut next_seq = 1u64;

        if log_path.exists() {
            let file = File::open(log_path)
                .map_err(|e| AuditIntegrityError::Io(format!("Failed to open log file: {e}")))?;
            let reader = BufReader::new(file);

            for (idx, line_res) in reader.lines().enumerate() {
                let line = line_res
                    .map_err(|e| AuditIntegrityError::Io(format!("Failed to read audit line: {e}")))?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let receipt: AuditReceipt = serde_json::from_str(trimmed).map_err(|e| {
                    AuditIntegrityError::CorruptEntry {
                        sequence: (idx + 1) as u64,
                        detail: e.to_string(),
                    }
                })?;

                // 1. Verify monotonic sequence
                if receipt.sequence != next_seq {
                    return Err(AuditIntegrityError::SequenceBroken {
                        entry_idx: idx,
                        expected: next_seq,
                        actual: receipt.sequence,
                    });
                }

                // 2. Verify previous hash pointer
                if receipt.prev_hash != last_hash {
                    return Err(AuditIntegrityError::BrokenHashChain {
                        sequence: receipt.sequence,
                        expected: last_hash,
                        actual: receipt.prev_hash.clone(),
                    });
                }

                // 3. Verify content hash
                let computed = Self::compute_hash(
                    &receipt.prev_hash,
                    receipt.sequence,
                    receipt.receipt_id,
                    receipt.correlation_id,
                    receipt.timestamp,
                    receipt.requester,
                    &receipt.capability,
                    receipt.outcome,
                    &receipt.reason,
                );

                if computed != receipt.hash {
                    return Err(AuditIntegrityError::TamperedEntry {
                        sequence: receipt.sequence,
                        computed,
                        recorded: receipt.hash,
                    });
                }

                last_hash = receipt.hash.clone();
                next_seq += 1;
                receipts.push(receipt);
            }

            info!(
                count = receipts.len(),
                last_hash = %last_hash,
                "Durable audit ledger verified successfully on boot"
            );
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .map_err(|e| AuditIntegrityError::Io(format!("Failed to open log file for appending: {e}")))?;

        Ok(Self {
            receipts: RwLock::new(receipts),
            last_hash: RwLock::new(last_hash),
            next_sequence: RwLock::new(next_seq),
            log_file: Mutex::new(Some(file)),
            log_path: Some(log_path.to_path_buf()),
        })
    }

    /// Calculate SHA-256 hash for an audit receipt
    fn compute_hash(
        prev_hash: &str,
        sequence: u64,
        receipt_id: Uuid,
        correlation_id: Option<Uuid>,
        timestamp: DateTime<Utc>,
        requester: SubsystemId,
        capability: &Capability,
        outcome: DecisionOutcome,
        reason: &str,
    ) -> String {
        let mut ctx = Context::new(&SHA256);
        ctx.update(prev_hash.as_bytes());
        ctx.update(&sequence.to_be_bytes());
        ctx.update(receipt_id.as_bytes());
        if let Some(corr) = correlation_id {
            ctx.update(corr.as_bytes());
        }
        ctx.update(timestamp.to_rfc3339().as_bytes());
        ctx.update(requester.to_string().as_bytes());
        ctx.update(serde_json::to_string(capability).unwrap_or_default().as_bytes());
        ctx.update(format!("{:?}", outcome).as_bytes());
        ctx.update(reason.as_bytes());

        let digest = ctx.finish();
        hex::encode(digest.as_ref())
    }

    /// Record a capability authorization decision and generate a SHA-256 chained receipt.
    /// Atomically persists to disk if backed by durable file.
    pub fn record(
        &self,
        requester: SubsystemId,
        capability: Capability,
        outcome: DecisionOutcome,
        reason: impl Into<String>,
    ) -> AuditReceipt {
        self.record_with_correlation(requester, capability, outcome, reason, None)
    }

    /// Record with explicit correlation ID for cross-subsystem tracing
    pub fn record_with_correlation(
        &self,
        requester: SubsystemId,
        capability: Capability,
        outcome: DecisionOutcome,
        reason: impl Into<String>,
        correlation_id: Option<Uuid>,
    ) -> AuditReceipt {
        let receipt_id = Uuid::new_v4();
        let timestamp = Utc::now();
        let reason_str = reason.into();

        // Lock file mutex first to serialize concurrent disk writes and prevent chain forks
        let mut file_guard = self.log_file.lock().unwrap();

        let mut seq_guard = self.next_sequence.write().unwrap();
        let sequence = *seq_guard;
        *seq_guard += 1;

        let mut last_hash_guard = self.last_hash.write().unwrap();
        let prev_hash = last_hash_guard.clone();

        let current_hash = Self::compute_hash(
            &prev_hash,
            sequence,
            receipt_id,
            correlation_id,
            timestamp,
            requester,
            &capability,
            outcome,
            &reason_str,
        );

        *last_hash_guard = current_hash.clone();

        let receipt = AuditReceipt {
            sequence,
            receipt_id,
            correlation_id,
            timestamp,
            requester,
            capability,
            outcome,
            reason: reason_str,
            prev_hash,
            hash: current_hash,
        };

        if let Some(file) = file_guard.as_mut() {
            if let Ok(line) = serde_json::to_string(&receipt) {
                let _ = writeln!(file, "{}", line);
                let _ = file.flush();
            }
        }

        let mut receipts_guard = self.receipts.write().unwrap();
        receipts_guard.push(receipt.clone());

        receipt
    }

    /// Verify cryptographic integrity of the entire audit chain in memory
    pub fn verify_chain(&self) -> bool {
        let receipts = self.receipts.read().unwrap();
        let mut expected_prev = GENESIS_HASH.to_string();
        let mut expected_seq = 1u64;

        for r in receipts.iter() {
            if r.sequence != expected_seq {
                return false;
            }
            if r.prev_hash != expected_prev {
                return false;
            }

            let computed = Self::compute_hash(
                &r.prev_hash,
                r.sequence,
                r.receipt_id,
                r.correlation_id,
                r.timestamp,
                r.requester,
                &r.capability,
                r.outcome,
                &r.reason,
            );

            if computed != r.hash {
                return false;
            }

            expected_prev = r.hash.clone();
            expected_seq += 1;
        }

        true
    }

    pub fn log_path(&self) -> Option<&Path> {
        self.log_path.as_deref()
    }

    pub fn len(&self) -> usize {
        self.receipts.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn list(&self, limit: usize) -> Vec<AuditReceipt> {
        let receipts = self.receipts.read().unwrap();
        receipts.iter().rev().take(limit).cloned().collect()
    }
}

impl Default for AuditLedger {
    fn default() -> Self {
        Self::new()
    }
}

mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            use std::fmt::Write;
            let _ = write!(s, "{:02x}", b);
        }
        s
    }
}
