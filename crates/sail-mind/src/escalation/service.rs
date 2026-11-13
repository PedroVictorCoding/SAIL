//! Escalation orchestration (Claude): cache → policy → redaction → async provider call.
//!
//! [`EscalationService::submit`] never waits on the network. An approved escalation becomes
//! a durable job that runs in the background; finished jobs are broadcast so Dialogue can
//! hand the answer back to the conversation or to the owner's attention inbox.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::provider::{ExternalProvider, ProviderError};
use super::redact::{self, RedactionMap};
use super::store::EscalationStore;
use super::{
    Decision, DecisionRecord, DenyReason, EscalationJob, EscalationPolicy, EscalationRequest,
    EscalationStatus, JobState, MAX_STORED_BYTES, Outcome, policy,
};

/// The fixed instruction sent with every external request.
pub const SYSTEM_PROMPT: &str = "You are a careful expert consulted by a local assistant. \
Answer the question directly and concisely. Placeholders such as [NAME_1] or [EMAIL_1] stand \
for private values; keep them unchanged. Do not ask follow-up questions.";

/// Longest a typed turn may wait for an answer before continuing without it.
pub const MAX_WAIT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Submitted {
    pub decision: Decision,
    pub explanation: String,
    pub route_score: f32,
    pub job: Option<EscalationJob>,
}

pub struct EscalationService {
    policy: Mutex<EscalationPolicy>,
    store: Arc<EscalationStore>,
    providers: Mutex<HashMap<String, Arc<dyn ExternalProvider>>>,
    /// Redaction maps of jobs started by this process. After a restart, answers keep
    /// their placeholders (the private values are not persisted).
    maps: Mutex<HashMap<String, RedactionMap>>,
    known_names: Mutex<Vec<String>>,
    finished: broadcast::Sender<EscalationJob>,
    lifecycle: CancellationToken,
}

impl EscalationService {
    pub fn new(policy: EscalationPolicy, store: Arc<EscalationStore>) -> Arc<Self> {
        let (finished, _) = broadcast::channel(64);
        let service = Arc::new(Self {
            policy: Mutex::new(EscalationPolicy::default()),
            store,
            providers: Mutex::new(HashMap::new()),
            maps: Mutex::new(HashMap::new()),
            known_names: Mutex::new(Vec::new()),
            finished,
            lifecycle: CancellationToken::new(),
        });
        service.set_policy(policy);
        service
    }

    /// Replace the policy and rebuild providers. A provider that can't be built is left
    /// out (and therefore never selected), with a warning.
    pub fn set_policy(&self, policy: EscalationPolicy) {
        let mut providers = HashMap::new();
        for config in policy.providers.iter().filter(|p| p.enabled) {
            match super::provider::build(config) {
                Ok(provider) => {
                    providers.insert(config.id.clone(), provider);
                }
                Err(error) => {
                    tracing::warn!(provider = %config.id, ?error, "escalation provider unavailable")
                }
            }
        }
        *lock(&self.providers) = providers;
        *lock(&self.policy) = policy;
    }

    /// Test seam: register a provider object directly (it must also be in the policy).
    pub fn insert_provider(&self, provider: Arc<dyn ExternalProvider>) {
        lock(&self.providers).insert(provider.id().to_owned(), provider);
    }

    pub fn policy(&self) -> EscalationPolicy {
        lock(&self.policy).clone()
    }

    /// Names (people, places) to redact in addition to the pattern classes.
    pub fn set_known_names(&self, names: Vec<String>) {
        *lock(&self.known_names) = names;
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EscalationJob> {
        self.finished.subscribe()
    }

    pub fn store(&self) -> &Arc<EscalationStore> {
        &self.store
    }

    /// Decide and, when approved, start the job in the background. Must be called inside
    /// a Tokio runtime.
    pub fn submit(self: &Arc<Self>, request: EscalationRequest) -> Result<Submitted, String> {
        let now = Utc::now();
        let fingerprint = policy::fingerprint(request.kind, &request.question);
        let policy_now = self.policy();
        let deny = |reason: DenyReason, score: f32| Submitted {
            decision: Decision::Deny { reason },
            explanation: reason.explain().into(),
            route_score: score,
            job: None,
        };
        if let Err(message) = super::validate(&request) {
            let mut result = deny(DenyReason::Invalid, 0.0);
            result.explanation = message;
            self.log(&request, &fingerprint, &result, None);
            return Ok(result);
        }
        let outcomes = self.store.outcomes()?;
        let score = policy::route_score(&request, &outcomes);
        if policy_now.enabled
            && let Some(job) = self
                .store
                .cached(&fingerprint, policy_now.cache_ttl_secs, now)?
        {
            let result = Submitted {
                decision: Decision::Cached {
                    job_id: job.id.clone(),
                },
                explanation: "An equivalent question was answered recently; reusing that answer."
                    .into(),
                route_score: score,
                job: Some(job.clone()),
            };
            self.log(&request, &fingerprint, &result, Some(job.id));
            return Ok(result);
        }
        let mut usage = self.store.usage(&policy_now, now)?;
        // Providers that failed to build are treated as out of quota for this decision.
        let built = lock(&self.providers).keys().cloned().collect::<Vec<_>>();
        for config in &policy_now.providers {
            if !built.contains(&config.id) {
                usage
                    .blocked_until
                    .push((config.id.clone(), now + chrono::Duration::days(3650)));
            }
        }
        let decision = policy::decide(&request, &policy_now, &usage, &outcomes, now);
        let Decision::Escalate { provider, budget } = decision.clone() else {
            let reason = match decision {
                Decision::Deny { reason } => reason,
                _ => DenyReason::Invalid,
            };
            let result = deny(reason, score);
            self.log(&request, &fingerprint, &result, None);
            return Ok(result);
        };
        let names = lock(&self.known_names).clone();
        let redacted = match redact::redact(&request.question, &names) {
            Ok(redacted) => redacted,
            Err(error) => {
                tracing::warn!(%error, "escalation redaction failed; nothing sent");
                let result = deny(DenyReason::RedactionFailed, score);
                self.log(&request, &fingerprint, &result, None);
                return Ok(result);
            }
        };
        let model = lock(&self.providers)
            .get(&provider)
            .map(|p| p.model().to_owned())
            .unwrap_or_default();
        let job = EscalationJob {
            id: Uuid::new_v4().to_string(),
            fingerprint: fingerprint.clone(),
            kind: request.kind,
            origin: request.origin,
            provider: provider.clone(),
            model,
            budget,
            state: JobState::Pending,
            attempts: 0,
            question: cut(&redacted.text),
            justification: cut(&request.justification),
            expected_benefit: cut(&request.expected_benefit),
            answer: None,
            error: None,
            input_tokens: 0,
            output_tokens: 0,
            cost_micros: 0,
            correlation_id: request.correlation_id.clone(),
            created_at: now,
            finished_at: None,
            outcome: None,
        };
        self.store.insert_job(&job)?;
        lock(&self.maps).insert(job.id.clone(), redacted.map);
        let result = Submitted {
            decision,
            explanation: format!(
                "Escalated to {} ({}); the answer arrives in the background.",
                job.provider, job.model
            ),
            route_score: score,
            job: Some(job.clone()),
        };
        self.log(&request, &fingerprint, &result, Some(job.id.clone()));
        self.spawn(job);
        Ok(result)
    }

    /// Wait up to `timeout` (capped at [`MAX_WAIT`]) for a job to finish.
    pub async fn wait(&self, job_id: &str, timeout: Duration) -> Option<EscalationJob> {
        let mut finished = self.subscribe();
        if let Ok(Some(job)) = self.store.job(job_id)
            && matches!(job.state, JobState::Done | JobState::Failed)
        {
            return Some(job);
        }
        let deadline = tokio::time::Instant::now() + timeout.min(MAX_WAIT);
        loop {
            match tokio::time::timeout_at(deadline, finished.recv()).await {
                Ok(Ok(job)) if job.id == job_id => return Some(job),
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    if let Ok(Some(job)) = self.store.job(job_id)
                        && matches!(job.state, JobState::Done | JobState::Failed)
                    {
                        return Some(job);
                    }
                }
                _ => return None,
            }
        }
    }

    /// Resume jobs a previous process left pending or running.
    pub fn resume(self: &Arc<Self>) -> Result<usize, String> {
        let pending = self.store.recover()?;
        let count = pending.len();
        for job in pending {
            self.spawn(job);
        }
        Ok(count)
    }

    pub fn feedback(&self, job_id: &str, outcome: Outcome) -> Result<(), String> {
        self.store.record_outcome(job_id, outcome)
    }

    /// An automatic check measured whether job `job_id` helped (#450). The owner's own
    /// feedback always wins; returns whether this was recorded.
    pub fn observe_outcome(
        &self,
        job_id: &str,
        outcome: Outcome,
        check: &str,
    ) -> Result<bool, String> {
        let recorded = self.store.record_check_outcome(job_id, outcome, check)?;
        if recorded {
            tracing::info!(job_id, ?outcome, check, "escalation outcome observed");
        }
        Ok(recorded)
    }

    /// Like [`Self::observe_outcome`] for every job submitted with `correlation_id`
    /// (e.g. `forge:<job id>`). Returns how many were recorded.
    pub fn observe_correlated(
        &self,
        correlation_id: &str,
        outcome: Outcome,
        check: &str,
    ) -> Result<usize, String> {
        let mut recorded = 0;
        for job_id in self.store.jobs_by_correlation(correlation_id)? {
            if self.observe_outcome(&job_id, outcome, check)? {
                recorded += 1;
            }
        }
        Ok(recorded)
    }

    pub fn status(&self) -> Result<EscalationStatus, String> {
        let policy = self.policy();
        let now = Utc::now();
        Ok(EscalationStatus {
            enabled: policy.enabled,
            providers: policy
                .providers
                .iter()
                .filter(|p| p.enabled)
                .map(|p| format!("{} ({})", p.id, p.model))
                .collect(),
            usage: self.store.usage(&policy, now)?,
            daily_token_cap: policy.daily_token_cap,
            monthly_token_cap: policy.monthly_token_cap,
            daily_cost_cap_micros: policy.daily_cost_cap_micros,
            monthly_cost_cap_micros: policy.monthly_cost_cap_micros,
            outcomes: self.store.outcomes()?,
            recent_decisions: self.store.decisions(20)?,
        })
    }

    pub fn shutdown(&self) {
        self.lifecycle.cancel();
    }

    fn spawn(self: &Arc<Self>, job: EscalationJob) {
        let service = self.clone();
        tokio::spawn(async move { service.run(job).await });
    }

    async fn run(self: Arc<Self>, job: EscalationJob) {
        let provider = lock(&self.providers).get(&job.provider).cloned();
        let Some(provider) = provider else {
            let _ = self
                .store
                .fail_job(&job.id, "provider is not configured", false, Utc::now());
            self.announce(&job.id);
            return;
        };
        if let Err(error) = self.store.mark_running(&job.id) {
            tracing::warn!(job = %job.id, %error, "escalation job could not start");
            return;
        }
        let reply = provider
            .complete(
                SYSTEM_PROMPT,
                &job.question,
                job.budget,
                self.lifecycle.child_token(),
            )
            .await;
        let now = Utc::now();
        match reply {
            Ok(reply) => {
                let map = lock(&self.maps).remove(&job.id).unwrap_or_default();
                let answer = redact::rehydrate(&reply.text, &map);
                let policy = self.policy();
                let cost = policy
                    .providers
                    .iter()
                    .find(|p| p.id == job.provider)
                    .map(|p| {
                        (reply.input_tokens * p.input_price_micros_per_mtok
                            + reply.output_tokens * p.output_price_micros_per_mtok)
                            / 1_000_000
                    })
                    .unwrap_or(0);
                if let Err(error) = self.store.finish_job(
                    &job.id,
                    &cut(&answer),
                    &reply.model,
                    reply.input_tokens,
                    reply.output_tokens,
                    cost,
                    now,
                ) {
                    tracing::warn!(job = %job.id, %error, "escalation result not stored");
                }
            }
            Err(ProviderError::RateLimited { retry_after }) => {
                let until = retry_after.unwrap_or(now + chrono::Duration::hours(1));
                let _ = self.store.block_provider(&job.provider, until);
                let _ = self
                    .store
                    .fail_job(&job.id, "provider rate limit", false, now);
            }
            Err(ProviderError::Cancelled) => {
                // Shutdown: leave it for `resume` after the restart.
                let _ = self.store.fail_job(&job.id, "cancelled", true, now);
                return;
            }
            Err(ProviderError::Unavailable(message)) => {
                let _ = self.store.fail_job(&job.id, &message, false, now);
            }
            Err(ProviderError::Http(message)) => {
                let state = self.store.fail_job(&job.id, &message, true, now);
                if state == Ok(JobState::Pending) {
                    let service = self.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        if let Ok(Some(job)) = service.store.job(&job.id) {
                            service.spawn(job);
                        }
                    });
                    return;
                }
            }
        }
        self.announce(&job.id);
    }

    fn announce(&self, id: &str) {
        if let Ok(Some(job)) = self.store.job(id) {
            let _ = self.finished.send(job);
        }
    }

    fn log(
        &self,
        request: &EscalationRequest,
        fingerprint: &str,
        result: &Submitted,
        job_id: Option<String>,
    ) {
        let record = DecisionRecord {
            at: Utc::now(),
            kind: request.kind,
            origin: request.origin,
            fingerprint: fingerprint.to_owned(),
            decision: result.decision.clone(),
            route_score: result.route_score,
            job_id,
        };
        if let Err(error) = self.store.log_decision(&record) {
            tracing::warn!(%error, "escalation decision not logged");
        }
        tracing::info!(kind = request.kind.as_str(), decision = ?result.decision, score = result.route_score, "escalation decision");
    }
}

/// `$SAIL_DATA/mind/escalation.json`.
pub fn default_policy_path() -> PathBuf {
    sail_common::storage::data_path("mind").join("escalation.json")
}

/// `$SAIL_DATA/mind/escalation.db`.
pub fn default_store_path() -> PathBuf {
    sail_common::storage::data_path("mind").join("escalation.db")
}

/// Missing file → default (disabled). A malformed file is an error, never a silent default.
pub fn load_policy(path: &Path) -> Result<EscalationPolicy, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(EscalationPolicy::default())
        }
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

static GLOBAL: std::sync::OnceLock<Arc<EscalationService>> = std::sync::OnceLock::new();

pub fn install_global(service: Arc<EscalationService>) -> bool {
    GLOBAL.set(service).is_ok()
}

pub fn global() -> Option<Arc<EscalationService>> {
    GLOBAL.get().cloned()
}

fn cut(text: &str) -> String {
    if text.len() <= MAX_STORED_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_STORED_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}
