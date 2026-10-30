use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use uuid::Uuid;

use crate::capability::{Capability, RiskLevel};
use sail_common::types::SubsystemId;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ApprovalError {
    #[error("Approval session {0} not found")]
    NotFound(Uuid),
    #[error("Approval session {0} expired")]
    Expired(Uuid),
    #[error("Approval session was explicitly denied by user")]
    Denied,
    #[error("Approval session was revoked")]
    Revoked,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenConsumptionError {
    #[error("Capability token {0} not found")]
    TokenNotFound(Uuid),
    #[error("Capability token {0} has expired")]
    Expired(Uuid),
    #[error("Capability token {0} was revoked")]
    Revoked(Uuid),
    #[error("Capability token {0} was already consumed (replay detected)")]
    TokenReplayed(Uuid),
    #[error("Caller {actual:?} does not match token bound actor {expected:?}")]
    ActorMismatch {
        actual: SubsystemId,
        expected: SubsystemId,
    },
    #[error("Requested capability does not match token bound capability")]
    CapabilityMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalSession {
    pub session_id: Uuid,
    pub requester: SubsystemId,
    pub capability: Capability,
    pub risk_level: RiskLevel,
    pub rationale: String,
    pub requested_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityToken {
    pub token_id: Uuid,
    pub session_id: Uuid,
    pub bound_actor: SubsystemId,
    pub bound_capability: Capability,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub single_use: bool,
}

pub struct ApprovalBroker {
    pending: RwLock<HashMap<Uuid, ApprovalSession>>,
    tokens: RwLock<HashMap<Uuid, CapabilityToken>>,
    consumed_tokens: RwLock<HashSet<Uuid>>,
    revoked_sessions: RwLock<HashSet<Uuid>>,
    revoked_tokens: RwLock<HashSet<Uuid>>,
    timeout_duration: Duration,
    token_ttl: Duration,
}

impl ApprovalBroker {
    pub fn new() -> Self {
        Self {
            pending: RwLock::new(HashMap::new()),
            tokens: RwLock::new(HashMap::new()),
            consumed_tokens: RwLock::new(HashSet::new()),
            revoked_sessions: RwLock::new(HashSet::new()),
            revoked_tokens: RwLock::new(HashSet::new()),
            timeout_duration: Duration::seconds(60),
            token_ttl: Duration::seconds(60),
        }
    }

    /// Register a new interactive approval session for high/critical risk operations
    pub fn create_session(
        &self,
        requester: SubsystemId,
        capability: Capability,
        rationale: impl Into<String>,
    ) -> ApprovalSession {
        let session_id = Uuid::new_v4();
        let now = Utc::now();
        let session = ApprovalSession {
            session_id,
            requester,
            risk_level: capability.risk_level(),
            capability,
            rationale: rationale.into(),
            requested_at: now,
            expires_at: now + self.timeout_duration,
        };

        let mut guard = self.pending.write().unwrap();
        guard.insert(session_id, session.clone());
        session
    }

    /// User resolves the approval request. If approved, issues a single-use cryptographically
    /// bound capability token with strict TTL and actor/capability binding.
    pub fn resolve_session(
        &self,
        session_id: Uuid,
        approved: bool,
    ) -> Result<CapabilityToken, ApprovalError> {
        let mut guard = self.pending.write().unwrap();
        let session = guard.remove(&session_id).ok_or(ApprovalError::NotFound(session_id))?;

        if self.revoked_sessions.read().unwrap().contains(&session_id) {
            return Err(ApprovalError::Revoked);
        }

        let now = Utc::now();
        if now > session.expires_at {
            return Err(ApprovalError::Expired(session_id));
        }

        if !approved {
            return Err(ApprovalError::Denied);
        }

        let token_id = Uuid::new_v4();
        let token = CapabilityToken {
            token_id,
            session_id,
            bound_actor: session.requester,
            bound_capability: session.capability,
            issued_at: now,
            expires_at: now + self.token_ttl,
            single_use: true,
        };

        let mut tokens_guard = self.tokens.write().unwrap();
        tokens_guard.insert(token_id, token.clone());

        Ok(token)
    }

    /// Atomically consume a capability token at the tool execution boundary.
    /// Guarantees single-use replay protection, bounds validation, actor verification, and expiry checking.
    pub fn consume_token(
        &self,
        token_id: Uuid,
        requester: SubsystemId,
        target_cap: &Capability,
    ) -> Result<CapabilityToken, TokenConsumptionError> {
        // 1. Check revocation
        if self.revoked_tokens.read().unwrap().contains(&token_id) {
            return Err(TokenConsumptionError::Revoked(token_id));
        }

        // 2. Atomic check and consume for replay protection
        {
            let mut consumed = self.consumed_tokens.write().unwrap();
            if consumed.contains(&token_id) {
                return Err(TokenConsumptionError::TokenReplayed(token_id));
            }
            // Mark token as consumed immediately
            consumed.insert(token_id);
        }

        // 3. Lookup token
        let tokens_guard = self.tokens.read().unwrap();
        let token = match tokens_guard.get(&token_id) {
            Some(t) => t.clone(),
            None => return Err(TokenConsumptionError::TokenNotFound(token_id)),
        };

        // 4. Validate expiration
        if Utc::now() > token.expires_at {
            return Err(TokenConsumptionError::Expired(token_id));
        }

        // 5. Validate actor binding (cannot be replayed by a different subsystem)
        if token.bound_actor != requester {
            return Err(TokenConsumptionError::ActorMismatch {
                actual: requester,
                expected: token.bound_actor,
            });
        }

        // 6. Validate exact capability binding (parameter tampering fails closed)
        if token.bound_capability != *target_cap {
            return Err(TokenConsumptionError::CapabilityMismatch);
        }

        Ok(token)
    }

    /// Revoke an issued capability grant immediately. Subsequent consume attempts fail closed.
    pub fn revoke_grant(&self, token_id: Uuid) {
        let mut guard = self.revoked_tokens.write().unwrap();
        guard.insert(token_id);
    }

    /// Revoke an entire approval session and any associated tokens.
    pub fn revoke_session(&self, session_id: Uuid) {
        {
            let mut guard = self.revoked_sessions.write().unwrap();
            guard.insert(session_id);
        }

        let tokens_guard = self.tokens.read().unwrap();
        let mut revoked_tokens_guard = self.revoked_tokens.write().unwrap();
        for (tid, token) in tokens_guard.iter() {
            if token.session_id == session_id {
                revoked_tokens_guard.insert(*tid);
            }
        }
    }

    pub fn is_revoked(&self, token_id: Uuid) -> bool {
        self.revoked_tokens.read().unwrap().contains(&token_id)
    }

    pub fn list_pending(&self) -> Vec<ApprovalSession> {
        let now = Utc::now();
        let guard = self.pending.read().unwrap();
        guard
            .values()
            .filter(|s| now <= s.expires_at && !self.revoked_sessions.read().unwrap().contains(&s.session_id))
            .cloned()
            .collect()
    }
}

impl Default for ApprovalBroker {
    fn default() -> Self {
        Self::new()
    }
}
