//! Capability-bound shared memory clients for trusted subsystem wiring.
use crate::{MemoryStore, NewRecord, knowledge::*};
use sail_common::types::SubsystemId;
use sail_guardian::memory::{MemoryAuthority, MemoryUse};
use sail_protocol::memory::{Embedding, MemoryQuery, MemoryReply};
use std::sync::Arc;
use uuid::Uuid;
#[derive(Clone)]
pub struct MemoryClient {
    store: Arc<MemoryStore>,
    authority: Arc<MemoryAuthority>,
    principal: SubsystemId,
    scopes: Vec<String>,
}
impl MemoryClient {
    pub fn new(
        store: Arc<MemoryStore>,
        authority: Arc<MemoryAuthority>,
        principal: SubsystemId,
        scopes: Vec<String>,
    ) -> Self {
        Self {
            store,
            authority,
            principal,
            scopes,
        }
    }
    pub fn principal(&self) -> SubsystemId {
        self.principal
    }
    fn access(
        &self,
        task: Uuid,
        scopes: &[String],
        usage: MemoryUse,
        now: i64,
    ) -> KnowledgeResult<Access> {
        if scopes.iter().any(|s| !self.scopes.contains(s)) {
            return Err(KnowledgeError::Invalid("client scope denied".into()));
        }
        let grant = self
            .authority
            .issue(self.principal, task, scopes, now, 60_000)
            .map_err(KnowledgeError::Invalid)?;
        self.authority
            .validate(&grant, self.principal, task, usage, now)
            .map_err(KnowledgeError::Invalid)?;
        Ok(Access {
            scopes: Some(grant.scopes().to_vec()),
            max_sensitivity: Sensitivity::from_i64(grant.max_sensitivity().into()),
            approved_only: grant.approved_only(),
        })
    }
    pub fn search(
        &self,
        task: Uuid,
        query: &MemoryQuery,
        now: i64,
    ) -> KnowledgeResult<MemoryReply> {
        self.search_in(task, &self.scopes, query, None, now)
    }
    pub fn search_in(
        &self,
        task: Uuid,
        scopes: &[String],
        query: &MemoryQuery,
        embedding: Option<&Embedding>,
        now: i64,
    ) -> KnowledgeResult<MemoryReply> {
        let access = self.access(task, scopes, MemoryUse::Read, now)?;
        self.store
            .knowledge()
            .search(query, &access, embedding, now)
    }
    pub fn context(&self, task: Uuid, text: &str, now: i64) -> KnowledgeResult<String> {
        Ok(self
            .search(
                task,
                &MemoryQuery {
                    version: 1,
                    text: text.into(),
                    limit: 12,
                    token_budget: 4000,
                    expand_graph: true,
                },
                now,
            )?
            .context)
    }
    pub fn observe(
        &self,
        task: Uuid,
        scope: &str,
        observation: &knowledge::consolidation::Observation,
        now: i64,
    ) -> KnowledgeResult<i64> {
        self.access(task, &[scope.into()], MemoryUse::Observe, now)?;
        let body = serde_json::to_string(observation)
            .map_err(|e| KnowledgeError::Invalid(e.to_string()))?;
        self.store
            .put(&NewRecord {
                scope,
                session: &task.to_string(),
                kind: "tool_observation",
                turn: None,
                gist: &observation.predicate,
                body: &body,
                provenance: Some("trusted-subsystem-observation"),
            })
            .map_err(Into::into)
    }
    pub fn maintenance_access(&self, task: Uuid, now: i64) -> KnowledgeResult<Access> {
        self.access(task, &self.scopes, MemoryUse::Maintain, now)
    }
}
use crate::knowledge;

impl MemoryClient {
    pub fn context_with_embedding(
        &self,
        task: Uuid,
        text: &str,
        embedding: Option<&Embedding>,
        now: i64,
    ) -> KnowledgeResult<String> {
        Ok(self
            .search_in(
                task,
                &self.scopes,
                &MemoryQuery {
                    version: 1,
                    text: text.into(),
                    limit: 12,
                    token_budget: 4000,
                    expand_graph: true,
                },
                embedding,
                now,
            )?
            .context)
    }
}
