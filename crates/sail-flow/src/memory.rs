//! Idle-triggered durable memory processing; no model is called by this scheduler.
use sail_arena::{
    MemoryStore,
    knowledge::{Access, KnowledgeResult},
};
use std::sync::Arc;
#[derive(Clone)]
pub struct MemoryMaintenance {
    store: Arc<MemoryStore>,
    access: Access,
    scopes: Vec<String>,
    idle: Arc<dyn Fn() -> bool + Send + Sync>,
    retention_ms: i64,
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
        }
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
        if !(self.idle)() {
            return Ok(false);
        }
        for scope in &self.scopes {
            k.expire(scope, &self.access, now, self.retention_ms)?;
        }
        Ok(true)
    }
}
