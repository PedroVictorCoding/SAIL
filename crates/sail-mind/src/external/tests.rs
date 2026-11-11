//! Runtime tests live in Mind so mutation runs for this package exercise its boundary,
//! independently of Bridge's HTTP integration tests.
use super::*;
use sail_common::resources::{AdmissionPolicy, GIB, HostProbe};
use sail_guardian::GuardianSubsystem;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct IdleHost;
impl HostProbe for IdleHost {
    fn sample(&self) -> std::io::Result<resources::HostSample> {
        Ok(resources::HostSample {
            available_ram_bytes: 16 * GIB,
            total_ram_bytes: 32 * GIB,
            swap_used_bytes: 0,
            swap_total_bytes: GIB,
            logical_cpus: 12,
            memory_pressure_avg10: Some(0.0),
        })
    }
}

#[derive(Clone)]
enum Reply {
    Text { text: String, model: String },
    Limited,
    Rejected,
    Unavailable,
    Protocol,
}
struct Transport {
    reply: Mutex<Reply>,
    calls: AtomicUsize,
    delay: Duration,
}
#[async_trait]
impl ExternalTextTransport for Transport {
    fn endpoint(&self) -> &str {
        "https://example.org/v1/chat/completions"
    }
    async fn send(
        &self,
        text: &str,
        _model: &str,
        _generation: &Generation,
        _limit: usize,
    ) -> std::result::Result<ExternalReply, ExternalFailure> {
        assert!(!text.contains("secret-api-key"));
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        match self.reply.lock().unwrap().clone() {
            Reply::Text { text, model } => Ok(ExternalReply {
                text,
                model,
                usage: Usage {
                    input_tokens: Some(12),
                    output_tokens: Some(8),
                },
                finish_reason: "stop".into(),
            }),
            Reply::Limited => Err(ExternalFailure::RateLimited {
                retry_after_seconds: Some(120),
            }),
            Reply::Rejected => Err(ExternalFailure::Rejected),
            Reply::Unavailable => Err(ExternalFailure::Unavailable),
            Reply::Protocol => Err(ExternalFailure::Protocol),
        }
    }
}
struct Fixture {
    guardian: GuardianSubsystem,
    backend: Arc<ExternalBackend>,
    ledger: Arc<UsageLedger>,
    transport: Arc<Transport>,
    scheduler: Arc<ResourceScheduler>,
    directory: std::path::PathBuf,
}
impl Fixture {
    fn new(delay: Duration) -> Self {
        let guardian = GuardianSubsystem::new();
        let directory = std::env::temp_dir().join(format!("hermes-runtime-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let ledger = Arc::new(UsageLedger::open(&directory.join("usage.db")).unwrap());
        let transport = Arc::new(Transport {
            reply: Mutex::new(Reply::Text {
                text: "answer".into(),
                model: "model-v1".into(),
            }),
            calls: AtomicUsize::new(0),
            delay,
        });
        let scheduler = Arc::new(ResourceScheduler::new(
            Arc::new(IdleHost),
            AdmissionPolicy::default(),
        ));
        let backend = Arc::new(
            ExternalBackend::new(
                ExternalProviderConfig {
                    provider: "test".into(),
                    model: "model".into(),
                    daily_cap: 10000,
                    entity_names: vec![],
                },
                transport.clone(),
                ledger.clone(),
                guardian.broker(),
                guardian.credentials(),
                scheduler.clone(),
            )
            .unwrap(),
        );
        Self {
            guardian,
            backend,
            ledger,
            transport,
            scheduler,
            directory,
        }
    }
    fn approve(&self, request: &InferenceRequest, limits: &ResourceLimits) -> Uuid {
        let capability = self.backend.approval_capability(request, limits).unwrap();
        let session = self.guardian.approval().create_session_for(
            Some(request.request_id),
            SubsystemId::Mind,
            capability,
            "owner test approval",
        );
        self.guardian
            .approval()
            .resolve_session(session.session_id, true)
            .unwrap()
            .token_id
    }
    async fn run(
        &self,
        request: &InferenceRequest,
        limits: &ResourceLimits,
    ) -> Result<InferenceResponse> {
        self.backend
            .infer_authorized(
                request,
                Some(self.approve(request, limits)),
                &CancellationToken::new(),
                limits,
            )
            .await
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn registration_and_explicit_model_selection_never_change_the_default_route() {
    let f = Fixture::new(Duration::ZERO);
    assert_eq!(f.backend.name(), "test");
    assert!(f.backend.aliases().is_empty());
    assert!(f.backend.is_external());
    assert!(
        crate::MindService::with_backends(vec![f.backend.clone()], ResourceLimits::default())
            .is_err()
    );
    for name in [None, Some("test"), Some("model")] {
        let mut request = InferenceRequest::text("explain ownership");
        request.model = name.map(str::to_owned);
        assert!(f.run(&request, &ResourceLimits::default()).await.is_ok());
    }
    let mut request = InferenceRequest::text("explain ownership");
    request.model = Some("other".into());
    assert_eq!(
        f.backend
            .approval_capability(&request, &ResourceLimits::default())
            .unwrap_err()
            .code,
        ErrorCode::UnknownModel
    );
    assert_eq!(f.transport.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn runtime_requires_approval_and_commits_usage_and_measured_timings_before_returning() {
    let f = Fixture::new(Duration::from_millis(15));
    let request = InferenceRequest::text("explain ownership");
    let limits = ResourceLimits::default();
    let cancel = CancellationToken::new();
    assert_eq!(
        f.backend
            .infer_authorized(&request, None, &cancel, &limits)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ForbiddenEndpoint
    );
    assert_eq!(f.transport.calls.load(Ordering::SeqCst), 0);
    assert!(f.ledger.receipt(request.request_id).unwrap().is_none());
    let response = f.run(&request, &limits).await.unwrap();
    assert_eq!(response.model, "test:model-v1");
    assert_eq!(response.message.content.text(), Some("answer"));
    assert!(response.timings.inference_ms >= 15);
    let receipt = f.ledger.receipt(request.request_id).unwrap().unwrap();
    assert_eq!(receipt.outcome, AttemptOutcome::Completed);
    assert_eq!(
        (receipt.input_tokens, receipt.output_tokens),
        (Some(12), Some(8))
    );
    assert!(receipt.finished_at.is_some());
    assert!(f.run(&request, &limits).await.is_err());
    assert_eq!(f.transport.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn runtime_enforces_each_response_boundary_including_exact_maxima() {
    let f = Fixture::new(Duration::ZERO);
    let limits = ResourceLimits {
        max_response_bytes: 64,
        ..Default::default()
    };
    for (text, model, accepted) in [
        ("x".repeat(63), "m".into(), true),
        ("x".repeat(64), "m".into(), true),
        ("x".repeat(65), "m".into(), false),
        ("x".repeat(66), "m".into(), false),
        (String::new(), "m".into(), false),
        ("x".into(), String::new(), false),
        ("x".into(), "m".repeat(256), true),
        ("x".into(), "m".repeat(257), false),
        ("x".into(), "m".repeat(258), false),
    ] {
        *f.transport.reply.lock().unwrap() = Reply::Text { text, model };
        let request = InferenceRequest::text("explain ownership");
        let result = f.run(&request, &limits).await;
        assert_eq!(result.is_ok(), accepted);
        let receipt = f.ledger.receipt(request.request_id).unwrap().unwrap();
        assert_eq!(
            receipt.outcome,
            if accepted {
                AttemptOutcome::Completed
            } else {
                AttemptOutcome::Uncertain
            }
        );
    }
}

#[tokio::test]
async fn provider_failures_keep_honest_terminal_outcomes_and_cooldowns() {
    for (reply, code, outcome) in [
        (
            Reply::Limited,
            ErrorCode::ResourceLimit,
            AttemptOutcome::RateLimited,
        ),
        (
            Reply::Rejected,
            ErrorCode::BackendUnavailable,
            AttemptOutcome::Failed,
        ),
        (
            Reply::Unavailable,
            ErrorCode::BackendUnavailable,
            AttemptOutcome::Uncertain,
        ),
        (
            Reply::Protocol,
            ErrorCode::BackendProtocol,
            AttemptOutcome::Uncertain,
        ),
    ] {
        let f = Fixture::new(Duration::ZERO);
        *f.transport.reply.lock().unwrap() = reply;
        let request = InferenceRequest::text("explain ownership");
        let limits = ResourceLimits::default();
        assert_eq!(f.run(&request, &limits).await.unwrap_err().code, code);
        assert_eq!(
            f.ledger
                .receipt(request.request_id)
                .unwrap()
                .unwrap()
                .outcome,
            outcome
        );
        if outcome == AttemptOutcome::RateLimited {
            assert_eq!(
                f.run(&InferenceRequest::text("retry"), &limits)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::ResourceLimit
            );
            assert_eq!(f.transport.calls.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn cancellation_deadline_and_dropped_callers_do_not_replay_or_cancel_parent_tasks() {
    for action in ["voice", "cancel", "drop", "deadline"] {
        let f = Fixture::new(Duration::from_secs(30));
        let request = InferenceRequest::text("explain ownership");
        let cancel = CancellationToken::new();
        let limits = ResourceLimits {
            inference_timeout_ms: if action == "deadline" { 1 } else { 30000 },
            ..Default::default()
        };
        let token = f.approve(&request, &limits);
        {
            let run = f
                .backend
                .infer_authorized(&request, Some(token), &cancel, &limits);
            tokio::pin!(run);
            tokio::select! {
                _=&mut run=>panic!("call completed before test interruption"),
                _=async { while f.transport.calls.load(Ordering::SeqCst)==0 { tokio::task::yield_now().await; } }=>{}
            }
            match action {
                "voice" => {
                    let _voice = f.scheduler.voice_turn();
                    assert_eq!(run.await.unwrap_err().code, ErrorCode::Busy);
                    assert!(!cancel.is_cancelled());
                }
                "cancel" => {
                    cancel.cancel();
                    assert_eq!(run.await.unwrap_err().code, ErrorCode::Cancelled);
                }
                "deadline" => assert_eq!(run.await.unwrap_err().code, ErrorCode::DeadlineExceeded),
                "drop" => {}
                _ => unreachable!(),
            }
        }
        assert_eq!(
            f.ledger
                .receipt(request.request_id)
                .unwrap()
                .unwrap()
                .outcome,
            AttemptOutcome::Uncertain
        );
        assert!(f.run(&request, &ResourceLimits::default()).await.is_err());
        assert_eq!(f.transport.calls.load(Ordering::SeqCst), 1);
    }
    let f = Fixture::new(Duration::ZERO);
    let request = InferenceRequest::text("explain ownership");
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        f.backend
            .infer_authorized(&request, None, &cancel, &ResourceLimits::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    assert_eq!(f.transport.calls.load(Ordering::SeqCst), 0);
    assert!(f.ledger.receipt(request.request_id).unwrap().is_none());
}
