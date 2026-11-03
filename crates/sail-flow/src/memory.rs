//! Idle-triggered durable memory processing. Deterministic consolidation and retention never
//! call a model. An optional proposer may ask the local model for a change set; Arena's
//! deterministic validator decides what, if anything, is committed.
use sail_arena::{
    MemoryStore,
    knowledge::{
        Access, KnowledgeError, KnowledgeResult,
        changeset::{self, MAX_SOURCES},
    },
};
use std::sync::Arc;

/// Sends a prompt to the local model and returns its text reply. Called on a blocking worker.
pub type Proposer = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;
#[derive(Clone)]
pub struct MemoryMaintenance {
    store: Arc<MemoryStore>,
    access: Access,
    scopes: Vec<String>,
    idle: Arc<dyn Fn() -> bool + Send + Sync>,
    retention_ms: i64,
    proposer: Option<Proposer>,
}
impl MemoryMaintenance {
    pub fn new(
        store: Arc<MemoryStore>,
        access: Access,
        scopes: Vec<String>,
        idle: Arc<dyn Fn() -> bool + Send + Sync>,
        retention_days: u32,
    ) -> Self {
        Self {
            store,
            access,
            scopes,
            idle,
            retention_ms: i64::from(retention_days.clamp(1, 3650)) * 86_400_000,
            proposer: None,
        }
    }
    /// Let the local model propose knowledge from turns the deterministic grammar did not
    /// cover. At most one model call per tick, only while idle.
    pub fn with_proposer(mut self, proposer: Proposer) -> Self {
        self.proposer = Some(proposer);
        self
    }
    /// Returns false when inference/coding work has admission priority.
    pub fn tick(&self, now: i64) -> KnowledgeResult<bool> {
        if !(self.idle)() {
            return Ok(false);
        }
        let k = self.store.knowledge();
        for scope in &self.scopes {
            k.enqueue_consolidation(scope, &self.access, now, 30_000)?;
        }
        if !(self.idle)() {
            return Ok(false);
        }
        if let Some(job) = k.lease_job(now)?
            && let Err(e) = k.run_job(&job, &self.access, now)
        {
            tracing::warn!(job=job.id,error=%e,"Memory job did not commit; source retained");
        }
        if let Some(proposer) = &self.proposer {
            if !(self.idle)() {
                return Ok(false);
            }
            self.propose(proposer, now)?;
        }
        if !(self.idle)() {
            return Ok(false);
        }
        for scope in &self.scopes {
            k.expire(scope, &self.access, now, self.retention_ms)?;
        }
        Ok(true)
    }

    /// One proposal round for the first scope with eligible turns. A model failure leaves the
    /// turns queued; an unusable reply marks them so they are not proposed again.
    fn propose(&self, proposer: &Proposer, now: i64) -> KnowledgeResult<()> {
        let k = self.store.knowledge();
        for scope in &self.scopes {
            let turns = k.proposal_sources(scope, &self.access, MAX_SOURCES)?;
            if turns.is_empty() {
                continue;
            }
            let generation = k.deletion_generation()?;
            let query = sail_protocol::memory::MemoryQuery {
                version: sail_protocol::memory::VERSION,
                text: turns
                    .iter()
                    .map(|t| t.user.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                limit: 16,
                token_budget: 4000,
                expand_graph: false,
            };
            let known = k
                .search(&query, &self.access, None, now)
                .map(|r| r.context)
                .unwrap_or_default();
            let reply = match proposer(&changeset::proposal_prompt(&turns, &known)) {
                Ok(reply) => reply,
                Err(error) => {
                    tracing::warn!(%error, scope, "Memory proposer unavailable; turns stay queued");
                    return Ok(());
                }
            };
            match changeset::parse_proposal(&reply) {
                Err(error) => {
                    tracing::warn!(%error, scope, "Memory proposal unusable; turns marked as offered");
                    k.mark_offered(scope, &self.access, &turns)?;
                }
                Ok(set) => match k.apply_changeset(scope, &self.access, generation, &turns, &set) {
                    Ok(report) => {
                        if let Some(reason) = &report.rejected {
                            tracing::warn!(scope, reason, "Memory proposal rejected by validator");
                        } else {
                            tracing::info!(
                                scope,
                                claims = report.claims.len(),
                                released = report.released,
                                "Memory proposal committed"
                            );
                        }
                    }
                    Err(KnowledgeError::StaleGeneration { .. }) => {
                        tracing::info!(
                            scope,
                            "Deletion during proposal; turns will be re-proposed"
                        );
                    }
                    Err(error) => return Err(error),
                },
            }
            return Ok(());
        }
        Ok(())
    }
}
