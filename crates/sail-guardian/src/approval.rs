use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;
use uuid::Uuid;

use crate::capability::{Capability, RiskLevel};
use sail_common::types::SubsystemId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalSession {
    pub session_id: Uuid,
    pub requester: SubsystemId,
    pub capability: Capability,
    pub risk_level: RiskLevel,
    pub rationale: String,
    pub requested_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

pub struct ApprovalBroker {
    pending: RwLock<HashMap<Uuid, ApprovalSession>>,
    timeout_duration: Duration,
}

impl ApprovalBroker {
    pub fn new() -> Self {
        Self {
            pending: RwLock::new(HashMap::new()),
            timeout_duration: Duration::seconds(60),
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

    /// User resolves the approval request
    pub fn resolve_session(&self, session_id: Uuid, approved: bool) -> Option<ApprovalSession> {
        let mut guard = self.pending.write().unwrap();
        if let Some(session) = guard.remove(&session_id) {
            if Utc::now() <= session.expires_at && approved {
                return Some(session);
            }
        }
        None
    }

    pub fn list_pending(&self) -> Vec<ApprovalSession> {
        let now = Utc::now();
        let guard = self.pending.read().unwrap();
        guard
            .values()
            .filter(|s| now <= s.expires_at)
            .cloned()
            .collect()
    }
}

impl Default for ApprovalBroker {
    fn default() -> Self {
        Self::new()
    }
}
