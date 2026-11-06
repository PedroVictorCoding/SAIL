//! Idle-triggered durable memory processing; no model is called by this scheduler.
use sail_arena::{
    MemoryStore,
    knowledge::{Access, KnowledgeError, KnowledgeResult},
};
use std::sync::Arc;
#[derive(Clone)]
pub struct MemoryMaintenance {
    embedder: Option<sail_mind::embedding::EmbeddingClient>,
    store: Arc<MemoryStore>,
    access: Access,
    scopes: Vec<String>,
    idle: Arc<dyn Fn() -> bool + Send + Sync>,
    retention_ms: i64,
    client: Option<sail_arena::memory::MemoryClient>,
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
            embedder: None,
            store,
            access,
            scopes,
            idle,
            client: None,
            retention_ms: i64::from(retention_days.clamp(1, 3650)) * 86_400_000,
        }
    }
    pub fn with_embedder(
        mut self,
        embedder: Option<sail_mind::embedding::EmbeddingClient>,
    ) -> Self {
        self.embedder = embedder;
        self
    }
    pub async fn embed_idle(&self, now: i64) -> KnowledgeResult<()> {
        let (Some(embedder), Some(client)) = (&self.embedder, &self.client) else {
            return Ok(());
        };
        if !(self.idle)() {
            return Ok(());
        }
        let access = client.maintenance_access(uuid::Uuid::new_v4(), now)?;
        let (model, revision) = embedder.identity();
        let k = self.store.knowledge();
        let (generation, rows) = k.embedding_candidates(&access, model, revision, now, 2)?;
        for (id, text) in rows {
            if !(self.idle)() {
                break;
            }
            let future = embedder.embed(&text);
            tokio::pin!(future);
            let mut check = tokio::time::interval(std::time::Duration::from_millis(100));
            let embedding = loop {
                tokio::select! {
                    result=&mut future=>break result.map_err(KnowledgeError::Invalid)?,
                    _=check.tick()=>if !(self.idle)(){return Ok(());},
                }
            };
            let current = chrono::Utc::now().timestamp_millis();
            let access = client.maintenance_access(uuid::Uuid::new_v4(), current)?;
            if !(self.idle)() {
                break;
            }
            k.put_embedding(id, &text, &embedding, &access, generation)?;
        }
        Ok(())
    }

    pub fn with_client(mut self, client: sail_arena::memory::MemoryClient) -> Self {
        self.client = Some(client);
        self
    }
    /// Returns false when inference/coding work has admission priority.
    pub fn tick(&self, now: i64) -> KnowledgeResult<bool> {
        if !(self.idle)() {
            return Ok(false);
        }
        let grant = self
            .client
            .as_ref()
            .map(|c| c.maintenance_access(uuid::Uuid::new_v4(), now))
            .transpose()?;
        let access = grant.as_ref().unwrap_or(&self.access);
        let k = self.store.knowledge();
        for scope in &self.scopes {
            k.enqueue_consolidation(scope, access, now, 30_000)?;
        }
        if !(self.idle)() {
            return Ok(false);
        }
        if let Some(job) = k.lease_job(now)? {
            if let Err(e) = k.run_job(&job, access, now) {
                tracing::warn!(job=job.id,error=%e,"Memory job did not commit; source retained");
            }
        }
        if !(self.idle)() {
            return Ok(false);
        }
        for scope in &self.scopes {
            k.expire(scope, access, now, self.retention_ms)?;
        }
        Ok(true)
    }
}
