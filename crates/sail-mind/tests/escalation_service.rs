//! Service end to end: policy, redaction, background job, cache and feedback.

mod escalation_fixtures;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use escalation_fixtures::{open_policy, provider, request};
use tokio_util::sync::CancellationToken;

use sail_mind::escalation::provider::{ExternalProvider, ProviderError, ProviderReply};
use sail_mind::escalation::service::{EscalationService, Submitted};
use sail_mind::escalation::store::EscalationStore;
use sail_mind::escalation::{
    Decision, DenyReason, EscalationJob, EscalationPolicy, JobState, Origin, Outcome, TaskKind,
};

struct FakeProvider {
    id: String,
    model: String,
    answer: String,
    error: Option<ProviderError>,
    delay: Duration,
    calls: AtomicUsize,
}

impl FakeProvider {
    fn answering(id: &str, answer: &str) -> Arc<Self> {
        Arc::new(Self {
            id: id.to_string(),
            model: format!("{id}-model"),
            answer: answer.to_string(),
            error: None,
            delay: Duration::ZERO,
            calls: AtomicUsize::new(0),
        })
    }

    fn failing(id: &str, error: ProviderError) -> Arc<Self> {
        Arc::new(Self {
            id: id.to_string(),
            model: format!("{id}-model"),
            answer: String::new(),
            error: Some(error),
            delay: Duration::ZERO,
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ExternalProvider for FakeProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(
        &self,
        _system: &str,
        _prompt: &str,
        _max_tokens: u32,
        cancel: CancellationToken,
    ) -> Result<ProviderReply, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() {
            tokio::select! {
                _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
                _ = tokio::time::sleep(self.delay) => {}
            }
        }
        if let Some(error) = self.error.clone() {
            return Err(error);
        }
        Ok(ProviderReply {
            text: self.answer.clone(),
            model: self.model.clone(),
            input_tokens: 40,
            output_tokens: 20,
        })
    }
}

fn service_policy() -> EscalationPolicy {
    let mut policy = open_policy();
    let mut config = provider("fake");
    config.adapter = "fake".to_string();
    config.model = "fake-model".to_string();
    policy.providers = vec![config];
    policy.cache_ttl_secs = 3_600;
    policy
}

fn service(policy: EscalationPolicy) -> (Arc<EscalationService>, Arc<EscalationStore>) {
    let store = Arc::new(EscalationStore::open_in_memory().expect("store"));
    let service = EscalationService::new(policy, store.clone());
    (service, store)
}

fn escalate(submitted: &Submitted) -> (&str, &EscalationJob) {
    match &submitted.decision {
        Decision::Escalate { provider, .. } => (
            provider.as_str(),
            submitted.job.as_ref().expect("job for an escalation"),
        ),
        other => panic!("expected an escalation, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_runs_in_the_background_and_identical_questions_are_cached() {
    let (service, store) = service(service_policy());
    let fake = FakeProvider::answering("fake", "Ask [NAME_1] directly.");
    service.insert_provider(fake.clone());
    service.set_known_names(vec!["Ana".to_string()]);

    let mut escalation = request(TaskKind::Coding);
    escalation.question = "What did Ana decide about the retry budget?".to_string();
    escalation.origin = Origin::ModelTool;
    let submitted = service.submit(escalation.clone()).expect("submit");
    let (chosen, job) = escalate(&submitted);
    assert_eq!(chosen, "fake");
    assert_eq!(job.state, JobState::Pending);
    assert_eq!(fake.calls(), 0, "submit must not wait on the network");

    let finished = service
        .wait(&job.id, Duration::from_secs(5))
        .await
        .expect("job finished");
    assert_eq!(finished.state, JobState::Done);
    assert_eq!(finished.answer.as_deref(), Some("Ask Ana directly."));
    assert_eq!(finished.model, "fake-model");
    assert_eq!(finished.input_tokens, 40);
    assert_eq!(finished.output_tokens, 20);
    assert_eq!(fake.calls(), 1);

    let usage = store
        .usage(&service.policy(), chrono::Utc::now())
        .expect("usage");
    assert_eq!(usage.tokens_today, 60);
    assert_eq!(usage.calls_today, 1);

    let again = service.submit(escalation).expect("submit");
    match again.decision {
        Decision::Cached { job_id } => assert_eq!(job_id, job.id),
        other => panic!("expected a cached decision, got {other:?}"),
    }
    let cached = again.job.expect("cached job");
    assert_eq!(cached.answer.as_deref(), Some("Ask Ana directly."));
    assert_eq!(fake.calls(), 1, "the cache must not call the provider");
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_policy_denies_before_any_work() {
    let mut policy = service_policy();
    policy.enabled = false;
    let (service, _store) = service(policy);
    let fake = FakeProvider::answering("fake", "answer");
    service.insert_provider(fake.clone());
    let submitted = service
        .submit(request(TaskKind::Reasoning))
        .expect("submit");
    assert_eq!(
        submitted.decision,
        Decision::Deny {
            reason: DenyReason::Disabled
        }
    );
    assert!(submitted.job.is_none());
    assert_eq!(fake.calls(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_requests_are_denied_with_the_validation_message() {
    let (service, _store) = service(service_policy());
    service.insert_provider(FakeProvider::answering("fake", "answer"));
    let mut invalid = request(TaskKind::Writing);
    invalid.question = "   ".to_string();
    let submitted = service.submit(invalid).expect("submit");
    assert_eq!(
        submitted.decision,
        Decision::Deny {
            reason: DenyReason::Invalid
        }
    );
    assert!(
        submitted.explanation.contains("question"),
        "{}",
        submitted.explanation
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn redaction_failure_sends_nothing() {
    let (service, _store) = service(service_policy());
    let fake = FakeProvider::answering("fake", "answer");
    service.insert_provider(fake.clone());
    service.set_known_names(vec!["[broken]".to_string()]);
    let submitted = service.submit(request(TaskKind::Research)).expect("submit");
    assert_eq!(
        submitted.decision,
        Decision::Deny {
            reason: DenyReason::RedactionFailed
        }
    );
    assert!(submitted.job.is_none());
    assert_eq!(fake.calls(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_rate_limits_fail_the_job_and_block_the_provider() {
    let (service, store) = service(service_policy());
    service.insert_provider(FakeProvider::failing(
        "fake",
        ProviderError::RateLimited { retry_after: None },
    ));
    let submitted = service.submit(request(TaskKind::Research)).expect("submit");
    let (_, job) = escalate(&submitted);
    let finished = service
        .wait(&job.id, Duration::from_secs(5))
        .await
        .expect("job finished");
    assert_eq!(finished.state, JobState::Failed);
    assert_eq!(finished.error.as_deref(), Some("provider rate limit"));

    let usage = store
        .usage(&service.policy(), chrono::Utc::now())
        .expect("usage");
    assert_eq!(usage.blocked_until.len(), 1);
    assert!(usage.blocked_until[0].1 > chrono::Utc::now());

    let mut other = request(TaskKind::Research);
    other.question = "A different question that is not rate limited?".to_string();
    let denied = service.submit(other).expect("submit");
    assert_eq!(
        denied.decision,
        Decision::Deny {
            reason: DenyReason::NoProvider
        }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_picks_up_a_job_left_pending_by_a_previous_process() {
    let (service, store) = service(service_policy());
    let fake = FakeProvider::answering("fake", "resumed");
    service.insert_provider(fake.clone());

    let mut pending = escalation_fixtures::request(TaskKind::Planning);
    pending.question = "A question left pending by the previous process?".to_string();
    let now = chrono::Utc::now();
    let job = EscalationJob {
        id: "left-pending".to_string(),
        fingerprint: sail_mind::escalation::policy::fingerprint(pending.kind, &pending.question),
        kind: pending.kind,
        origin: pending.origin,
        provider: "fake".to_string(),
        model: "fake-model".to_string(),
        budget: 500,
        state: JobState::Pending,
        attempts: 0,
        question: pending.question,
        justification: pending.justification,
        expected_benefit: pending.expected_benefit,
        answer: None,
        error: None,
        input_tokens: 0,
        output_tokens: 0,
        cost_micros: 0,
        correlation_id: None,
        created_at: now,
        finished_at: None,
        outcome: None,
    };
    store.insert_job(&job).expect("insert");
    assert_eq!(service.resume().expect("resume"), 1);
    let finished = service
        .wait("left-pending", Duration::from_secs(5))
        .await
        .expect("job finished");
    assert_eq!(finished.state, JobState::Done);
    assert_eq!(finished.answer.as_deref(), Some("resumed"));
    assert_eq!(fake.calls(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn feedback_is_recorded_on_the_job_and_in_the_status() {
    let (service, store) = service(service_policy());
    service.insert_provider(FakeProvider::answering("fake", "answer"));
    let submitted = service.submit(request(TaskKind::Coding)).expect("submit");
    let (_, job) = escalate(&submitted);
    service
        .wait(&job.id, Duration::from_secs(5))
        .await
        .expect("job finished");
    service
        .feedback(&job.id, Outcome::Improved)
        .expect("feedback");
    let stored = store.job(&job.id).expect("job").expect("exists");
    assert_eq!(stored.outcome, Some(Outcome::Improved));
    let status = service.status().expect("status");
    assert_eq!(status.outcomes.tally(TaskKind::Coding), (1, 0));
    assert_eq!(status.providers, vec!["fake (fake-model)".to_string()]);
    assert!(!status.recent_decisions.is_empty());
}
