//! In-process memory capabilities issued only by the configured Guardian authority.
//! Grants cannot be deserialized from model/tool input, and another issuer is rejected.
use sail_common::types::SubsystemId;
use std::{collections::HashMap, sync::RwLock};
use uuid::Uuid;
#[derive(Clone)]
pub struct MemoryPolicy {
    pub scopes: Vec<String>,
    pub max_sensitivity: u8,
    pub observe: bool,
    pub maintain: bool,
    pub approved_only: bool,
}
#[derive(Clone)]
pub struct MemoryGrant {
    issuer: Uuid,
    principal: SubsystemId,
    task: Uuid,
    revision: u64,
    expires: i64,
    issued_at: i64,
    scopes: Vec<String>,
    policy: MemoryPolicy,
}
pub enum MemoryUse {
    Read,
    Observe,
    Maintain,
}
pub struct MemoryAuthority {
    issuer: Uuid,
    policies: RwLock<HashMap<SubsystemId, (u64, MemoryPolicy)>>,
}
impl Default for MemoryAuthority {
    fn default() -> Self {
        Self::new()
    }
}
impl MemoryAuthority {
    pub fn new() -> Self {
        Self {
            issuer: Uuid::new_v4(),
            policies: RwLock::new(HashMap::new()),
        }
    }
    /// Trusted startup/owner policy, never inferred from conversation text.
    pub fn configure(&self, principal: SubsystemId, policy: MemoryPolicy) -> Result<(), String> {
        if policy.max_sensitivity > 2 || policy.scopes.iter().any(|s| s.trim().is_empty()) {
            return Err("invalid memory policy".into());
        }
        let mut p = self
            .policies
            .write()
            .map_err(|_| "memory policy lock poisoned")?;
        let revision = p.get(&principal).map_or(1, |(v, _)| v + 1);
        p.insert(principal, (revision, policy));
        Ok(())
    }
    pub fn issue(
        &self,
        principal: SubsystemId,
        task: Uuid,
        scopes: &[String],
        now: i64,
        ttl_ms: i64,
    ) -> Result<MemoryGrant, String> {
        if task.is_nil() || !(1..=300_000).contains(&ttl_ms) || scopes.is_empty() {
            return Err("invalid memory task or duration".into());
        }
        let p = self
            .policies
            .read()
            .map_err(|_| "memory policy lock poisoned")?;
        let (revision, policy) = p
            .get(&principal)
            .ok_or("memory access is not configured for this subsystem")?;
        if scopes.iter().any(|s| !policy.scopes.contains(s)) {
            return Err("memory scope denied".into());
        }
        Ok(MemoryGrant {
            issuer: self.issuer,
            principal,
            task,
            revision: *revision,
            issued_at: now,
            expires: now.saturating_add(ttl_ms),
            scopes: scopes.to_vec(),
            policy: policy.clone(),
        })
    }
    pub fn validate(
        &self,
        g: &MemoryGrant,
        principal: SubsystemId,
        task: Uuid,
        usage: MemoryUse,
        now: i64,
    ) -> Result<(), String> {
        let p = self
            .policies
            .read()
            .map_err(|_| "memory policy lock poisoned")?;
        if g.issuer != self.issuer
            || g.principal != principal
            || g.task != task
            || now < g.issued_at
            || now >= g.expires
            || p.get(&principal).is_none_or(|(r, _)| *r != g.revision)
        {
            return Err("memory grant expired, revoked or belongs to another issuer/task".into());
        }
        if matches!(usage, MemoryUse::Observe) && !g.policy.observe
            || matches!(usage, MemoryUse::Maintain) && !g.policy.maintain
        {
            return Err("memory operation denied".into());
        }
        Ok(())
    }
}
impl MemoryGrant {
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }
    pub fn max_sensitivity(&self) -> u8 {
        self.policy.max_sensitivity
    }
    pub fn approved_only(&self) -> bool {
        self.policy.approved_only
    }
}
