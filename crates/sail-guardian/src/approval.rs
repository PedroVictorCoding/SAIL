use crate::capability::{Capability, RiskLevel};
use chrono::{DateTime, Duration, Utc};
use sail_common::types::SubsystemId;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Mutex};
use uuid::Uuid;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ApprovalError {
    #[error("Approval session {0} not found")]
    NotFound(Uuid),
    #[error("Approval session {0} expired")]
    Expired(Uuid),
    #[error("Approval explicitly denied")]
    Denied,
    #[error("Approval session revoked")]
    Revoked,
    #[error("Approval capacity exhausted")]
    Capacity,
}
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenConsumptionError {
    #[error("Capability token {0} not found")]
    TokenNotFound(Uuid),
    #[error("Capability token {0} expired")]
    Expired(Uuid),
    #[error("Capability token {0} revoked")]
    Revoked(Uuid),
    #[error("Capability token {0} already consumed")]
    TokenReplayed(Uuid),
    #[error("Caller {actual:?} does not match actor {expected:?}")]
    ActorMismatch {
        actual: SubsystemId,
        expected: SubsystemId,
    },
    #[error("Capability parameters do not match approval")]
    CapabilityMismatch,
    #[error("Task/session correlation does not match approval")]
    CorrelationMismatch,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalSession {
    pub session_id: Uuid,
    pub requester: SubsystemId,
    pub capability: Capability,
    pub risk_level: RiskLevel,
    pub rationale: String,
    pub correlation_id: Option<Uuid>,
    pub requested_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityToken {
    pub token_id: Uuid,
    pub session_id: Uuid,
    pub bound_actor: SubsystemId,
    pub bound_capability: Capability,
    pub correlation_id: Option<Uuid>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub single_use: bool,
}
#[derive(Clone, Copy)]
enum TokenState {
    Ready,
    Consumed,
    Revoked,
}
#[derive(Default)]
struct State {
    pending: HashMap<Uuid, ApprovalSession>,
    tokens: HashMap<Uuid, (CapabilityToken, TokenState)>,
}
/// Trusted in-process authority. Network/event clients must authenticate before
/// calling resolve_session. Approval events alone can never mint authority.
#[derive(Default)]
pub struct ApprovalBroker {
    state: Mutex<State>,
}
impl ApprovalBroker {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn create_session(
        &self,
        requester: SubsystemId,
        capability: Capability,
        rationale: impl Into<String>,
    ) -> ApprovalSession {
        self.create_session_for(None, requester, capability, rationale)
    }
    pub fn create_session_for(
        &self,
        correlation_id: Option<Uuid>,
        requester: SubsystemId,
        capability: Capability,
        rationale: impl Into<String>,
    ) -> ApprovalSession {
        let now = Utc::now();
        let session = ApprovalSession {
            session_id: Uuid::new_v4(),
            requester,
            risk_level: capability.risk_level(),
            capability,
            rationale: rationale.into(),
            correlation_id,
            requested_at: now,
            expires_at: now + Duration::seconds(60),
        };
        let mut state = self.state.lock().unwrap();
        state.pending.retain(|_, s| s.expires_at > now);
        state.tokens.retain(|_, (t, _)| t.expires_at > now);
        if state.pending.len() >= 1024 {
            if let Some(oldest) = state
                .pending
                .values()
                .min_by_key(|s| s.requested_at)
                .map(|s| s.session_id)
            {
                state.pending.remove(&oldest);
            }
        }
        state.pending.insert(session.session_id, session.clone());
        session
    }
    pub fn resolve_session(
        &self,
        id: Uuid,
        approved: bool,
    ) -> Result<CapabilityToken, ApprovalError> {
        let mut state = self.state.lock().unwrap();
        let session = state
            .pending
            .remove(&id)
            .ok_or(ApprovalError::NotFound(id))?;
        let now = Utc::now();
        if now >= session.expires_at {
            return Err(ApprovalError::Expired(id));
        }
        if !approved {
            return Err(ApprovalError::Denied);
        }
        state.tokens.retain(|_, (t, _)| t.expires_at > now);
        if state.tokens.len() >= 4096 {
            return Err(ApprovalError::Capacity);
        }
        let token = CapabilityToken {
            token_id: Uuid::new_v4(),
            session_id: id,
            bound_actor: session.requester,
            bound_capability: session.capability,
            correlation_id: session.correlation_id,
            issued_at: now,
            expires_at: now + Duration::seconds(60),
            single_use: true,
        };
        state
            .tokens
            .insert(token.token_id, (token.clone(), TokenState::Ready));
        Ok(token)
    }
    pub fn consume_token(
        &self,
        id: Uuid,
        actor: SubsystemId,
        capability: &Capability,
    ) -> Result<CapabilityToken, TokenConsumptionError> {
        self.consume_token_for(id, actor, capability, None)
    }
    pub fn consume_token_for(
        &self,
        id: Uuid,
        actor: SubsystemId,
        capability: &Capability,
        correlation_id: Option<Uuid>,
    ) -> Result<CapabilityToken, TokenConsumptionError> {
        let mut state = self.state.lock().unwrap();
        let (token, status) = state
            .tokens
            .get_mut(&id)
            .ok_or(TokenConsumptionError::TokenNotFound(id))?;
        match status {
            TokenState::Revoked => return Err(TokenConsumptionError::Revoked(id)),
            TokenState::Consumed => return Err(TokenConsumptionError::TokenReplayed(id)),
            TokenState::Ready => {}
        }
        if Utc::now() >= token.expires_at {
            return Err(TokenConsumptionError::Expired(id));
        }
        if token.bound_actor != actor {
            return Err(TokenConsumptionError::ActorMismatch {
                actual: actor,
                expected: token.bound_actor,
            });
        }
        if token.bound_capability != *capability {
            return Err(TokenConsumptionError::CapabilityMismatch);
        }
        if token.correlation_id != correlation_id {
            return Err(TokenConsumptionError::CorrelationMismatch);
        }
        *status = TokenState::Consumed;
        Ok(token.clone())
    }
    pub fn revoke_grant(&self, id: Uuid) {
        if let Some((_, status)) = self.state.lock().unwrap().tokens.get_mut(&id) {
            *status = TokenState::Revoked;
        }
    }
    pub fn revoke_session(&self, id: Uuid) {
        let mut state = self.state.lock().unwrap();
        state.pending.remove(&id);
        for (token, status) in state.tokens.values_mut() {
            if token.session_id == id {
                *status = TokenState::Revoked;
            }
        }
    }
    pub fn is_revoked(&self, id: Uuid) -> bool {
        self.state
            .lock()
            .unwrap()
            .tokens
            .get(&id)
            .is_some_and(|(_, s)| matches!(s, TokenState::Revoked))
    }
    pub fn list_pending(&self) -> Vec<ApprovalSession> {
        let now = Utc::now();
        self.state
            .lock()
            .unwrap()
            .pending
            .values()
            .filter(|s| s.expires_at > now)
            .cloned()
            .collect()
    }
}
