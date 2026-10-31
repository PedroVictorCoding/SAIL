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

#[derive(Default)]
struct ApprovalState {
    pending: HashMap<Uuid, ApprovalSession>,
    tokens: HashMap<Uuid, CapabilityToken>,
    consumed_tokens: HashSet<Uuid>,
    revoked_sessions: HashSet<Uuid>,
    revoked_tokens: HashSet<Uuid>,
}

pub struct ApprovalBroker {
    // One lock linearizes issuance, revocation, and successful consumption.
    state: RwLock<ApprovalState>,
    timeout_duration: Duration,
    token_ttl: Duration,
}

impl ApprovalBroker {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(ApprovalState::default()),
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

        let mut state = self.state.write().unwrap();
        state.pending.insert(session_id, session.clone());
        session
    }

    /// Resolve an approval from a trusted caller and issue an opaque, single-use handle.
    /// Binding is enforced against broker-owned state; this method does not authenticate users.
    pub fn resolve_session(
        &self,
        session_id: Uuid,
        approved: bool,
    ) -> Result<CapabilityToken, ApprovalError> {
        let mut state = self.state.write().unwrap();
        if state.revoked_sessions.contains(&session_id) {
            return Err(ApprovalError::Revoked);
        }
        let session = state
            .pending
            .remove(&session_id)
            .ok_or(ApprovalError::NotFound(session_id))?;

        let now = Utc::now();
        if now >= session.expires_at {
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

        state.tokens.insert(token_id, token.clone());

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
        let mut state = self.state.write().unwrap();
        if state.revoked_tokens.contains(&token_id) {
            return Err(TokenConsumptionError::Revoked(token_id));
        }
        let token = state
            .tokens
            .get(&token_id)
            .cloned()
            .ok_or(TokenConsumptionError::TokenNotFound(token_id))?;
        if state.revoked_sessions.contains(&token.session_id) {
            return Err(TokenConsumptionError::Revoked(token_id));
        }
        if state.consumed_tokens.contains(&token_id) {
            return Err(TokenConsumptionError::TokenReplayed(token_id));
        }
        if Utc::now() >= token.expires_at {
            return Err(TokenConsumptionError::Expired(token_id));
        }
        if token.bound_actor != requester {
            return Err(TokenConsumptionError::ActorMismatch {
                actual: requester,
                expected: token.bound_actor,
            });
        }
        if token.bound_capability != *target_cap {
            return Err(TokenConsumptionError::CapabilityMismatch);
        }

        // Invalid attempts cannot burn another caller's grant or grow replay state.
        state.consumed_tokens.insert(token_id);
        Ok(token)
    }

    /// Revoke a grant. Consumption that linearizes after this call fails closed.
    /// Already authorized work requires cancellation at its execution layer.
    pub fn revoke_grant(&self, token_id: Uuid) {
        self.state.write().unwrap().revoked_tokens.insert(token_id);
    }

    /// Revoke a session atomically with issuance and consumption of its tokens.
    pub fn revoke_session(&self, session_id: Uuid) {
        let mut state = self.state.write().unwrap();
        state.revoked_sessions.insert(session_id);
        state.pending.remove(&session_id);
    }

    pub fn is_revoked(&self, token_id: Uuid) -> bool {
        let state = self.state.read().unwrap();
        state.revoked_tokens.contains(&token_id)
            || state
                .tokens
                .get(&token_id)
                .is_some_and(|token| state.revoked_sessions.contains(&token.session_id))
    }

    pub fn list_pending(&self) -> Vec<ApprovalSession> {
        let state = self.state.read().unwrap();
        let now = Utc::now();
        state
            .pending
            .values()
            .filter(|s| now < s.expires_at)
            .cloned()
            .collect()
    }
}

impl Default for ApprovalBroker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::ToolCapability;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn capability() -> Capability {
        Capability::Tool(ToolCapability::KernelTool("vision:frame".into()))
    }

    fn issue(broker: &ApprovalBroker) -> CapabilityToken {
        let session = broker.create_session(SubsystemId::Bridge, capability(), "Capture frame");
        broker.resolve_session(session.session_id, true).unwrap()
    }

    #[test]
    fn invalid_attempts_do_not_consume_or_allocate_replay_entries() {
        let broker = ApprovalBroker::new();
        let token = issue(&broker);
        assert!(matches!(
            broker.consume_token(token.token_id, SubsystemId::Agents, &capability()),
            Err(TokenConsumptionError::ActorMismatch { .. })
        ));
        let other = Capability::Tool(ToolCapability::KernelTool("other:action".into()));
        assert_eq!(
            broker.consume_token(token.token_id, SubsystemId::Bridge, &other),
            Err(TokenConsumptionError::CapabilityMismatch)
        );
        for _ in 0..32 {
            let unknown = Uuid::new_v4();
            for _ in 0..2 {
                assert_eq!(
                    broker.consume_token(unknown, SubsystemId::Bridge, &capability()),
                    Err(TokenConsumptionError::TokenNotFound(unknown))
                );
            }
        }
        assert!(broker.state.read().unwrap().consumed_tokens.is_empty());
        assert!(
            broker
                .consume_token(token.token_id, SubsystemId::Bridge, &capability())
                .is_ok()
        );
        assert_eq!(
            broker.consume_token(token.token_id, SubsystemId::Bridge, &capability()),
            Err(TokenConsumptionError::TokenReplayed(token.token_id))
        );
    }

    #[test]
    fn session_revocation_removes_pending_and_invalidates_issued_tokens() {
        let broker = ApprovalBroker::new();
        let pending = broker.create_session(SubsystemId::Bridge, capability(), "Pending");
        broker.revoke_session(pending.session_id);
        assert!(broker.list_pending().is_empty());
        assert_eq!(
            broker.resolve_session(pending.session_id, true),
            Err(ApprovalError::Revoked)
        );

        let token = issue(&broker);
        broker.revoke_session(token.session_id);
        assert!(broker.is_revoked(token.token_id));
        assert_eq!(
            broker.consume_token(token.token_id, SubsystemId::Bridge, &capability()),
            Err(TokenConsumptionError::Revoked(token.token_id))
        );
    }

    #[test]
    fn grant_revocation_preserves_other_grants() {
        let broker = ApprovalBroker::new();
        let revoked = issue(&broker);
        let valid = issue(&broker);
        broker.revoke_grant(revoked.token_id);
        assert!(broker.is_revoked(revoked.token_id));
        assert!(!broker.is_revoked(valid.token_id));
        assert_eq!(
            broker.consume_token(revoked.token_id, SubsystemId::Bridge, &capability()),
            Err(TokenConsumptionError::Revoked(revoked.token_id))
        );
        assert!(
            broker
                .consume_token(valid.token_id, SubsystemId::Bridge, &capability())
                .is_ok()
        );
    }

    #[test]
    fn denied_and_expired_sessions_never_issue_tokens() {
        let mut broker = ApprovalBroker::new();
        let denied = broker.create_session(SubsystemId::Bridge, capability(), "Denied");
        assert_eq!(
            broker.resolve_session(denied.session_id, false),
            Err(ApprovalError::Denied)
        );
        assert_eq!(
            broker.resolve_session(denied.session_id, true),
            Err(ApprovalError::NotFound(denied.session_id))
        );
        broker.timeout_duration = Duration::zero();
        let expired = broker.create_session(SubsystemId::Bridge, capability(), "Expired");
        assert!(broker.list_pending().is_empty());
        assert_eq!(
            broker.resolve_session(expired.session_id, true),
            Err(ApprovalError::Expired(expired.session_id))
        );
        assert!(broker.state.read().unwrap().tokens.is_empty());
    }

    #[test]
    fn expired_tokens_are_not_consumed() {
        let mut broker = ApprovalBroker::new();
        broker.token_ttl = Duration::zero();
        let token = issue(&broker);
        assert_eq!(
            broker.consume_token(token.token_id, SubsystemId::Bridge, &capability()),
            Err(TokenConsumptionError::Expired(token.token_id))
        );
        assert!(broker.state.read().unwrap().consumed_tokens.is_empty());
    }

    #[test]
    fn concurrent_consumers_have_exactly_one_winner() {
        let broker = Arc::new(ApprovalBroker::new());
        let token = issue(&broker);
        let barrier = Arc::new(Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let broker = Arc::clone(&broker);
                let barrier = Arc::clone(&barrier);
                let token_id = token.token_id;
                thread::spawn(move || {
                    barrier.wait();
                    broker.consume_token(token_id, SubsystemId::Bridge, &capability())
                })
            })
            .collect();
        let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Err(TokenConsumptionError::TokenReplayed(_))))
                .count(),
            7
        );
    }

    #[test]
    fn concurrent_resolution_cannot_escape_session_revocation() {
        for _ in 0..128 {
            let broker = Arc::new(ApprovalBroker::new());
            let session = broker.create_session(SubsystemId::Bridge, capability(), "Race");
            let barrier = Arc::new(Barrier::new(2));
            let resolver = {
                let broker = Arc::clone(&broker);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    broker.resolve_session(session.session_id, true)
                })
            };
            barrier.wait();
            broker.revoke_session(session.session_id);
            match resolver.join().unwrap() {
                Ok(token) => {
                    assert!(broker.is_revoked(token.token_id));
                    assert_eq!(
                        broker.consume_token(token.token_id, SubsystemId::Bridge, &capability()),
                        Err(TokenConsumptionError::Revoked(token.token_id))
                    );
                }
                Err(error) => assert_eq!(error, ApprovalError::Revoked),
            }
            assert!(broker.list_pending().is_empty());
        }
    }

    #[test]
    fn concurrent_consumption_and_revocation_leave_token_revoked() {
        for _ in 0..128 {
            let broker = Arc::new(ApprovalBroker::new());
            let token = issue(&broker);
            let barrier = Arc::new(Barrier::new(2));
            let consumer = {
                let broker = Arc::clone(&broker);
                let barrier = Arc::clone(&barrier);
                let token_id = token.token_id;
                thread::spawn(move || {
                    barrier.wait();
                    broker.consume_token(token_id, SubsystemId::Bridge, &capability())
                })
            };
            barrier.wait();
            broker.revoke_session(token.session_id);
            let result = consumer.join().unwrap();
            // Consumption may win before revocation; revocation cannot undo completed work.
            assert!(
                result.is_ok() || result == Err(TokenConsumptionError::Revoked(token.token_id))
            );
            assert!(broker.is_revoked(token.token_id));
            assert_eq!(
                broker.consume_token(token.token_id, SubsystemId::Bridge, &capability()),
                Err(TokenConsumptionError::Revoked(token.token_id))
            );
        }
    }
}
