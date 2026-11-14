//! Idle-triggered durable memory processing. Deterministic consolidation and retention never
//! call a model. An optional proposer may ask the local model for a change set; Arena's
//! deterministic validator decides what, if anything, is committed.
//!
//! Round 22 (P5): every proposed change set is first scanned for text that must never
//! become memory (instructions aimed at the model, exfiltration, hidden Unicode, secrets);
//! a flagged set is dropped. When the owner turned on review, an accepted set waits in the
//! review queue instead of committing, and [`apply_reviewed`] commits it after approval,
//! through the same validator.
use sail_arena::{
    MemoryStore,
    knowledge::{
        Access, KnowledgeError, KnowledgeResult,
        changeset::{self, ChangeOp, ChangeSetReport, MAX_SOURCES, MemoryChangeSet, SourceTurn},
        memory_scan,
        review::{PendingChange, ReviewQueue},
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
    /// The review queue's folder (`ReviewQueue` itself is not `Clone`).
    review: Option<std::path::PathBuf>,
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
            review: None,
        }
    }
    /// Hold accepted proposals in the owner's review queue while review is turned on.
    pub fn with_review(mut self, queue: ReviewQueue) -> Self {
        self.review = Some(queue.dir);
        self
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
                Ok(set) if !scan_set(&set).is_empty() => {
                    tracing::warn!(scope, findings = ?scan_set(&set), "Memory proposal dropped by the injection scan");
                    k.mark_offered(scope, &self.access, &turns)?;
                }
                Ok(set)
                    if !set.operations.is_empty()
                        && self.review.as_ref().is_some_and(|dir| {
                            ReviewQueue { dir: dir.clone() }.enabled().unwrap_or(false)
                        }) =>
                {
                    let queue = ReviewQueue {
                        dir: self.review.clone().expect("checked above"),
                    };
                    let pending = PendingChange {
                        id: uuid::Uuid::new_v4().simple().to_string(),
                        change: serde_json::json!({"scope": scope, "turns": turns, "set": set}),
                        summary: summary(&set),
                        findings: Vec::new(),
                        session: scope.clone(),
                        proposed_at: now / 1000,
                    };
                    match queue.push(pending) {
                        Ok(_) => {
                            k.mark_offered(scope, &self.access, &turns)?;
                            tracing::info!(scope, "Memory proposal waiting for the owner's review");
                        }
                        Err(error) => {
                            tracing::warn!(%error, scope, "Review queue unavailable; turns stay queued")
                        }
                    }
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

/// Every finding of the memory scan over a set's quoted and written text.
fn scan_set(set: &MemoryChangeSet) -> Vec<memory_scan::ScanFinding> {
    set.operations
        .iter()
        .flat_map(|op| match op {
            ChangeOp::Assert {
                quote,
                subject,
                predicate,
                value,
                ..
            } => memory_scan::scan(&format!("{subject} {predicate} {value}\n{quote}")),
            ChangeOp::Correct { quote, value, .. } => {
                memory_scan::scan(&format!("{value}\n{quote}"))
            }
        })
        .collect()
}

/// One line per operation, for the owner.
pub fn summary(set: &MemoryChangeSet) -> Vec<String> {
    set.operations
        .iter()
        .map(|op| match op {
            ChangeOp::Assert {
                subject,
                predicate,
                value,
                ..
            } => {
                let who = if subject == changeset::OWNER {
                    "you"
                } else {
                    subject.as_str()
                };
                format!("Remember: {who} — {}: {value}", predicate.replace('_', " "))
            }
            ChangeOp::Correct { claim, value, .. } => {
                format!("Change remembered fact #{claim} to: {value}")
            }
        })
        .collect()
}

/// Commit a change the owner approved in the review queue. It goes through Arena's
/// validator again, against the current memory (`now` in milliseconds is unused by the
/// validator but kept for symmetry with [`MemoryMaintenance::tick`]).
pub fn apply_reviewed(
    store: &MemoryStore,
    access: &Access,
    pending: &PendingChange,
    _now: i64,
) -> Result<ChangeSetReport, String> {
    let scope = pending.change["scope"]
        .as_str()
        .ok_or("the queued change has no scope")?;
    let turns: Vec<SourceTurn> = serde_json::from_value(pending.change["turns"].clone())
        .map_err(|e| format!("the queued change is unreadable: {e}"))?;
    let set: MemoryChangeSet = serde_json::from_value(pending.change["set"].clone())
        .map_err(|e| format!("the queued change is unreadable: {e}"))?;
    if !scan_set(&set).is_empty() {
        return Err("the change contains text that can't be remembered".into());
    }
    let k = store.knowledge();
    let generation = k.deletion_generation().map_err(|e| e.to_string())?;
    k.apply_changeset(scope, access, generation, &turns, &set)
        .map_err(|e| e.to_string())
}
