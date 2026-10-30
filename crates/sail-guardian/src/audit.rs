use chrono::{DateTime, Utc};
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use uuid::Uuid;

use crate::capability::Capability;
use sail_common::types::SubsystemId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    Granted,
    Denied,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReceipt {
    pub receipt_id: Uuid,
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
}

impl AuditLedger {
    pub fn new() -> Self {
        // Genesis block hash
        let genesis_hash = "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        Self {
            receipts: RwLock::new(Vec::new()),
            last_hash: RwLock::new(genesis_hash),
        }
    }

    /// Record a capability authorization decision and generate a SHA-256 chained receipt
    pub fn record(
        &self,
        requester: SubsystemId,
        capability: Capability,
        outcome: DecisionOutcome,
        reason: impl Into<String>,
    ) -> AuditReceipt {
        let receipt_id = Uuid::new_v4();
        let timestamp = Utc::now();
        let reason_str = reason.into();

        let mut last_hash_guard = self.last_hash.write().unwrap();
        let prev_hash = last_hash_guard.clone();

        // Compute SHA-256 hash chain: H(prev_hash + id + timestamp + requester + capability + outcome + reason)
        let mut ctx = Context::new(&SHA256);
        ctx.update(prev_hash.as_bytes());
        ctx.update(receipt_id.as_bytes());
        ctx.update(timestamp.to_rfc3339().as_bytes());
        ctx.update(requester.to_string().as_bytes());
        ctx.update(serde_json::to_string(&capability).unwrap_or_default().as_bytes());
        ctx.update(format!("{:?}", outcome).as_bytes());
        ctx.update(reason_str.as_bytes());

        let digest = ctx.finish();
        let current_hash = hex::encode(digest.as_ref());

        *last_hash_guard = current_hash.clone();

        let receipt = AuditReceipt {
            receipt_id,
            timestamp,
            requester,
            capability,
            outcome,
            reason: reason_str,
            prev_hash,
            hash: current_hash,
        };

        let mut receipts_guard = self.receipts.write().unwrap();
        receipts_guard.push(receipt.clone());

        receipt
    }

    /// Verify cryptographic integrity of the entire audit chain
    pub fn verify_chain(&self) -> bool {
        let receipts = self.receipts.read().unwrap();
        let mut expected_prev = "0000000000000000000000000000000000000000000000000000000000000000".to_string();

        for r in receipts.iter() {
            if r.prev_hash != expected_prev {
                return false;
            }

            let mut ctx = Context::new(&SHA256);
            ctx.update(r.prev_hash.as_bytes());
            ctx.update(r.receipt_id.as_bytes());
            ctx.update(r.timestamp.to_rfc3339().as_bytes());
            ctx.update(r.requester.to_string().as_bytes());
            ctx.update(serde_json::to_string(&r.capability).unwrap_or_default().as_bytes());
            ctx.update(format!("{:?}", r.outcome).as_bytes());
            ctx.update(r.reason.as_bytes());

            let computed = hex::encode(ctx.finish().as_ref());
            if computed != r.hash {
                return false;
            }

            expected_prev = r.hash.clone();
        }

        true
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

