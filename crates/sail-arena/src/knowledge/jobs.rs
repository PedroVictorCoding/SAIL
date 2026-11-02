//! Durable, leased, bounded consolidation queue. Never stores transcript content in jobs.
use super::*;
use crate::store::sha16;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
const EXTRACTOR_VERSION: i64 = 1;
const MAX_ATTEMPTS: i64 = 5;
#[derive(Debug, Clone, Serialize)]
pub struct MemoryJob {
    pub id: i64,
    pub scope: String,
    pub generation: i64,
    pub sources: String,
    pub attempts: i64,
    pub lease_token: String,
}
#[derive(Debug, Default, Serialize)]
pub struct RetentionReport {
    pub archive_records: usize,
    pub expired_claims: usize,
}
impl KnowledgeStore<'_> {
    /// Idempotency includes source identity, hash and creation time, not reusable row ids alone.
    pub fn enqueue_consolidation(
        &self,
        scope: &str,
        access: &Access,
        now: i64,
        idle_ms: i64,
    ) -> KnowledgeResult<Option<i64>> {
        if !access.allows(scope, Sensitivity::Private) {
            return Err(KnowledgeError::Invalid("scope not authorized".into()));
        }
        self.write(Actor::subsystem("flow"),|tx| {
            let sources={let mut stmt=tx.conn.prepare("SELECT r.id,hex(r.hash),r.created_at FROM records r WHERE r.scope=?1 AND r.created_at<=?2 AND NOT EXISTS(SELECT 1 FROM kn_source_progress p WHERE p.record_id=r.id AND p.extractor_version=?3) ORDER BY r.id LIMIT 256")?;stmt.query_map(params![scope,now.saturating_sub(idle_ms.max(0)),EXTRACTOR_VERSION],|r|Ok(serde_json::json!({"id":r.get::<_,i64>(0)?,"hash":r.get::<_,String>(1)?,"created_at":r.get::<_,i64>(2)?})))?.collect::<Result<Vec<_>,_>>()?};
            if sources.is_empty(){return Ok(None);}
            let generation=super::write::generation(tx.conn)?;
            let payload=serde_json::to_string(&sources).map_err(|e|KnowledgeError::Invalid(e.to_string()))?;
            let digest=sha16(&[scope.as_bytes(),&generation.to_le_bytes(),payload.as_bytes(),&EXTRACTOR_VERSION.to_le_bytes()]);
            let key=digest.iter().map(|b|format!("{b:02x}")).collect::<String>();
            tx.conn.execute("INSERT OR IGNORE INTO kn_jobs(idempotency_key,scope,generation,sources,state,not_before,created_at,updated_at) VALUES(?1,?2,?3,?4,'pending',?5,?5,?5)",params![key,scope,generation,payload,now])?;
            let id=tx.conn.query_row("SELECT id FROM kn_jobs WHERE idempotency_key=?1",[key],|r|r.get(0))?;
            for s in sources {tx.conn.execute("INSERT OR IGNORE INTO kn_source_progress(record_id,extractor_version) VALUES(?1,?2)",params![s["id"].as_i64(),EXTRACTOR_VERSION])?;}
            Ok(Some(id))
        })
    }
    pub fn lease_job(&self, now: i64) -> KnowledgeResult<Option<MemoryJob>> {
        self.write(Actor::subsystem("flow"),|tx| {
            tx.conn.execute("UPDATE kn_jobs SET state='failed',last_error='lease attempts exhausted',updated_at=?1 WHERE state='running' AND lease_until<=?1 AND attempts>=?2",[now,MAX_ATTEMPTS])?;
            let busy:bool=tx.conn.query_row("SELECT EXISTS(SELECT 1 FROM kn_jobs WHERE state='running' AND lease_until>?1)",[now],|r|r.get(0))?;
            if busy{return Ok(None);}
            let row:Option<(i64,String,i64,String,i64)>=tx.conn.query_row("SELECT id,scope,generation,sources,attempts FROM kn_jobs WHERE attempts<?2 AND ((state='pending' AND not_before<=?1) OR (state='running' AND lease_until<=?1)) ORDER BY id LIMIT 1",[now,MAX_ATTEMPTS],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            let Some((id,scope,generation,sources,attempts))=row else{return Ok(None)};
            let lease_token=uuid::Uuid::new_v4().to_string();
            tx.conn.execute("UPDATE kn_jobs SET state='running',attempts=attempts+1,lease_until=?2,lease_token=?3,updated_at=?4 WHERE id=?1",params![id,now.saturating_add(300_000),lease_token,now])?;
            Ok(Some(MemoryJob{id,scope,generation,sources,attempts:attempts+1,lease_token}))
        })
    }
    /// Admission is checked by Flow before leasing; no model call is made by this worker.
    pub fn run_job(
        &self,
        job: &MemoryJob,
        access: &Access,
        now: i64,
    ) -> KnowledgeResult<consolidation::ConsolidationReport> {
        let owned=self.read(|conn|Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM kn_jobs WHERE id=?1 AND state='running' AND lease_token=?2 AND lease_until>?3)",params![job.id,job.lease_token,now],|r|r.get::<_,bool>(0))?))?;
        if !owned {
            return Err(KnowledgeError::Invalid(
                "job lease is no longer owned".into(),
            ));
        }
        let result =
            self.consolidate_sources(&job.scope, access, job.generation, 256, Some(&job.sources));
        let (state, delay, error) = match &result {
            Ok(_) => ("done", 0, None),
            Err(KnowledgeError::StaleGeneration { .. } | KnowledgeError::Tombstoned) => {
                ("obsolete", 0, Some("deleted or privacy-changed source"))
            }
            Err(KnowledgeError::Storage(_)) if job.attempts < MAX_ATTEMPTS => (
                "pending",
                30_000i64.saturating_mul(1i64 << job.attempts.min(6)),
                Some("storage failure; retry scheduled"),
            ),
            Err(_) => (
                "failed",
                0,
                Some("candidate rejected; source retained until retention expiry"),
            ),
        };
        self.write(Actor::subsystem("flow"),|tx| {
            let count=tx.conn.execute("UPDATE kn_jobs SET state=?3,not_before=?4,lease_until=NULL,lease_token=NULL,last_error=?5,updated_at=?6 WHERE id=?1 AND lease_token=?2 AND state='running'",params![job.id,job.lease_token,state,now.saturating_add(delay),error,now])?;
            if count!=1{return Err(KnowledgeError::Invalid("job lease changed before completion".into()));}Ok(())
        })?;
        result
    }
    /// Owner-configured raw retention and unpinned transient expiry, bounded per tick.
    pub fn expire(
        &self,
        scope: &str,
        access: &Access,
        now: i64,
        archive_ttl_ms: i64,
    ) -> KnowledgeResult<RetentionReport> {
        if archive_ttl_ms < 60_000 || !access.allows(scope, Sensitivity::Private) {
            return Err(KnowledgeError::Invalid(
                "invalid retention interval or access".into(),
            ));
        }
        self.write(Actor::subsystem("retention"),|tx| {
            let ids:Vec<i64>={let mut s=tx.conn.prepare("SELECT id FROM records WHERE scope=?1 AND created_at<?2 ORDER BY id LIMIT 256")?;s.query_map(params![scope,now.saturating_sub(archive_ttl_ms)],|r|r.get(0))?.collect::<Result<_,_>>()?};
            for id in &ids {tx.conn.execute("DELETE FROM records_fts WHERE rowid=?1",[id])?;tx.conn.execute("DELETE FROM records WHERE id=?1",[id])?;}
            let claims:Vec<i64>={let mut s=tx.conn.prepare("SELECT c.id FROM kn_claims c WHERE c.scope=?1 AND c.sensitivity<=?3 AND c.expires_at<=?2 AND NOT EXISTS(SELECT 1 FROM kn_pins p WHERE p.entity_id=c.subject_id AND p.pinned=1) ORDER BY c.id LIMIT 256")?;s.query_map(params![scope,now,access.max_sensitivity as i64],|r|r.get(0))?.collect::<Result<_,_>>()?};
            for id in &claims {tx.delete_claim(*id,"transient retention expiry")?;}
            tx.conn.execute("DELETE FROM kn_jobs WHERE state IN ('done','failed','obsolete') AND updated_at<?1",[now.saturating_sub(archive_ttl_ms)])?;
            Ok(RetentionReport{archive_records:ids.len(),expired_claims:claims.len()})
        })
    }
}

#[derive(Debug, Serialize)]
pub struct JobSummary {
    pub id: i64,
    pub scope: String,
    pub state: String,
    pub attempts: i64,
    pub last_error: Option<String>,
}
impl KnowledgeStore<'_> {
    pub fn job_summaries(&self, access: &Access) -> KnowledgeResult<Vec<JobSummary>> {
        let scopes = serde_json::to_string(&access.scopes)
            .map_err(|e| KnowledgeError::Invalid(e.to_string()))?;
        self.read(|conn|{let mut stmt=conn.prepare("SELECT id,scope,state,attempts,last_error FROM kn_jobs WHERE (?1=1 OR scope IN (SELECT value FROM json_each(?2))) AND ?3>=1 ORDER BY id DESC LIMIT 100")?;Ok(stmt.query_map(params![access.scopes.is_none(),scopes,access.max_sensitivity as i64],|r|Ok(JobSummary{id:r.get(0)?,scope:r.get(1)?,state:r.get(2)?,attempts:r.get(3)?,last_error:r.get(4)?}))?.collect::<Result<_,_>>()?)})
    }
}
