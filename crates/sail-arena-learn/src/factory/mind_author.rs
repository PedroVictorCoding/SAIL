//! The factory's model, over SAIL Mind: [`MindAuthor`] implements
//! [`AuthorModel`](super::contracts::AuthorModel) and [`MindGate`] implements
//! [`ModelGate`](super::jobs::ModelGate).
//!
//! Every authoring call runs at [`WorkPriority::Background`]. Mind's scheduler then refuses
//! it while a voice turn is active and cancels it the moment one starts, so the owner never
//! waits on the factory. Errors are sorted into the four [`AuthorModelError`] kinds,
//! because the queue treats "the owner needed the model" (wait and resume) very differently
//! from "the model returned garbage" (count a failure).
use super::contracts::{AuthorModel, AuthorModelError, AuthorPrompt};
use super::jobs::ModelGate;
use sail_common::resources::ResourceScheduler;
use sail_protocol::resources::WorkPriority;
use sail_protocol::{
    Content, ContentPart, ErrorCode, Generation, InferenceError, InferenceRequest,
    InferenceResponse, Message, Role, ToolChoice, VERSION,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Sampling temperature for authoring. Low: the replies are structured JSON, and
/// variety comes from the prompts and fixtures, not from sampling noise.
pub const AUTHOR_TEMPERATURE: f32 = 0.3;
/// Largest reply the factory accepts from one call. A lesson draft is a few KB of JSON;
/// anything near this is a runaway generation.
pub const MAX_REPLY_BYTES: usize = 256 * 1024;

/// One inference call. [`sail_mind::MindService`] implements it; tests use a fake.
#[async_trait::async_trait]
pub trait Infer: Send + Sync {
    async fn infer(
        &self,
        request: InferenceRequest,
        cancel: CancellationToken,
    ) -> Result<InferenceResponse, InferenceError>;
}

#[async_trait::async_trait]
impl Infer for sail_mind::MindService {
    async fn infer(
        &self,
        request: InferenceRequest,
        cancel: CancellationToken,
    ) -> Result<InferenceResponse, InferenceError> {
        self.infer_with_priority(request, cancel, None, WorkPriority::Background)
            .await
    }
}

/// [`AuthorModel`] over Mind's resident model.
pub struct MindAuthor {
    mind: Arc<dyn Infer>,
    model: Option<String>,
}

impl MindAuthor {
    /// `model` picks a registered model by name; `None` uses Mind's default.
    pub fn new(mind: Arc<dyn Infer>, model: Option<String>) -> Self {
        Self { mind, model }
    }

    fn request(&self, prompt: &AuthorPrompt) -> InferenceRequest {
        InferenceRequest {
            version: VERSION,
            request_id: uuid::Uuid::new_v4(),
            model: self.model.clone(),
            messages: vec![
                Message::text(Role::System, prompt.system.clone()),
                Message::text(Role::User, prompt.user.clone()),
            ],
            tools: vec![],
            tool_choice: ToolChoice::None,
            generation: Generation {
                max_tokens: prompt.max_output_tokens,
                temperature: AUTHOR_TEMPERATURE,
                reasoning_budget: prompt.reasoning_budget,
            },
        }
    }
}

/// Sort a Mind error. `outer_cancelled` tells our own cancellation (job cancelled, daemon
/// stopping) apart from the scheduler cancelling the call for a voice turn.
fn classify(e: &InferenceError, outer_cancelled: bool) -> AuthorModelError {
    let msg = e.message.clone();
    match e.code {
        ErrorCode::Cancelled if outer_cancelled => {
            AuthorModelError::Preempted("the job was stopped".into())
        }
        ErrorCode::Cancelled => AuthorModelError::Preempted(format!("foreground work: {msg}")),
        ErrorCode::ResourceLimit if msg.to_ascii_lowercase().contains("voice") => {
            AuthorModelError::Preempted(msg)
        }
        ErrorCode::ResourceLimit | ErrorCode::Busy | ErrorCode::QueueTimeout => {
            AuthorModelError::NotAdmitted(msg)
        }
        ErrorCode::BackendUnavailable | ErrorCode::ShuttingDown | ErrorCode::UnknownModel => {
            AuthorModelError::Unavailable(msg)
        }
        _ => AuthorModelError::Failed(format!("{:?}: {msg}", e.code)),
    }
}

fn reply_text(response: &InferenceResponse) -> String {
    match &response.message.content {
        Content::Text(text) => text.clone(),
        Content::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
    }
}

#[async_trait::async_trait]
impl AuthorModel for MindAuthor {
    async fn complete(
        &self,
        prompt: &AuthorPrompt,
        cancel: CancellationToken,
    ) -> Result<String, AuthorModelError> {
        if cancel.is_cancelled() {
            return Err(AuthorModelError::Preempted("the job was stopped".into()));
        }
        // A child token: the scheduler may cancel it for voice without touching the job's.
        let call = cancel.child_token();
        let response = self
            .mind
            .infer(self.request(prompt), call)
            .await
            .map_err(|e| classify(&e, cancel.is_cancelled()))?;
        if response.finish_reason == "length" {
            return Err(AuthorModelError::Failed(format!(
                "the reply hit the {}-token limit before it finished",
                prompt.max_output_tokens
            )));
        }
        let text = reply_text(&response);
        if text.len() > MAX_REPLY_BYTES {
            return Err(AuthorModelError::Failed("the reply is too large".into()));
        }
        if text.trim().is_empty() {
            return Err(AuthorModelError::Failed("the reply is empty".into()));
        }
        Ok(text)
    }
}

/// Foreground activity besides voice that the factory must not compete with (an open
/// Learn turn, a Dialogue reply). Wired by the daemon; `|| false` when there is none.
pub type ForegroundBusy = Arc<dyn Fn() -> bool + Send + Sync>;

/// [`ModelGate`] over the host scheduler and Mind's queue.
pub struct MindGate {
    scheduler: Arc<ResourceScheduler>,
    mind_busy: ForegroundBusy,
    foreground: ForegroundBusy,
}

impl MindGate {
    /// `mind_busy` is usually `move || mind.has_work()`: any inference in flight, whatever
    /// its priority, means the factory waits rather than queueing behind the owner.
    pub fn new(
        scheduler: Arc<ResourceScheduler>,
        mind_busy: ForegroundBusy,
        foreground: ForegroundBusy,
    ) -> Self {
        Self {
            scheduler,
            mind_busy,
            foreground,
        }
    }
}

impl ModelGate for MindGate {
    fn admit_background(&self) -> Result<(), String> {
        if self.scheduler.voice_active() {
            return Err("a voice turn is in progress".into());
        }
        if (self.foreground)() {
            return Err("the owner is using SAIL".into());
        }
        if (self.mind_busy)() {
            return Err("the model is serving another request".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Fake {
        reply: Mutex<Result<InferenceResponse, InferenceError>>,
        seen: Mutex<Option<InferenceRequest>>,
        cancel_outer: Option<CancellationToken>,
    }

    #[async_trait::async_trait]
    impl Infer for Fake {
        async fn infer(
            &self,
            request: InferenceRequest,
            _cancel: CancellationToken,
        ) -> Result<InferenceResponse, InferenceError> {
            if let Some(outer) = &self.cancel_outer {
                outer.cancel();
            }
            *self.seen.lock().unwrap() = Some(request);
            self.reply.lock().unwrap().clone()
        }
    }

    fn response(text: &str, finish: &str) -> InferenceResponse {
        serde_json::from_value(serde_json::json!({
            "version": VERSION,
            "request_id": uuid::Uuid::new_v4(),
            "model": "m",
            "message": {"role": "assistant", "content": text},
            "finish_reason": finish,
            "usage": {},
            "timings": {"queue_ms": 0, "inference_ms": 0}
        }))
        .unwrap()
    }

    fn error(code: ErrorCode, message: &str) -> InferenceError {
        InferenceError {
            version: VERSION,
            code,
            message: message.into(),
            retryable: true,
        }
    }

    fn prompt() -> AuthorPrompt {
        AuthorPrompt {
            system: "sys".into(),
            user: "user".into(),
            schema: "lesson",
            max_output_tokens: 900,
            reasoning_budget: None,
        }
    }

    fn author(reply: Result<InferenceResponse, InferenceError>) -> (MindAuthor, Arc<Fake>) {
        let fake = Arc::new(Fake {
            reply: Mutex::new(reply),
            seen: Mutex::new(None),
            cancel_outer: None,
        });
        (MindAuthor::new(fake.clone(), None), fake)
    }

    #[tokio::test]
    async fn sends_system_and_user_with_the_prompt_budget() {
        let (a, fake) = author(Ok(response("{\"ok\":1}", "stop")));
        let text = a
            .complete(&prompt(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(text, "{\"ok\":1}");
        let req = fake.seen.lock().unwrap().clone().unwrap();
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, Role::System);
        assert_eq!(req.generation.max_tokens, 900);
        assert!(req.tools.is_empty());
    }

    #[tokio::test]
    async fn a_cut_off_or_empty_reply_is_a_failure() {
        let (a, _) = author(Ok(response("{\"half\":", "length")));
        assert!(matches!(
            a.complete(&prompt(), CancellationToken::new()).await,
            Err(AuthorModelError::Failed(_))
        ));
        let (a, _) = author(Ok(response("  ", "stop")));
        assert!(matches!(
            a.complete(&prompt(), CancellationToken::new()).await,
            Err(AuthorModelError::Failed(_))
        ));
    }

    #[tokio::test]
    async fn errors_are_sorted_into_wait_or_fail() {
        let cases = [
            (ErrorCode::Cancelled, "voice preempted", "preempted"),
            (ErrorCode::ResourceLimit, "voice priority", "preempted"),
            (ErrorCode::ResourceLimit, "insufficient RAM", "not_admitted"),
            (ErrorCode::Busy, "queue full", "not_admitted"),
            (ErrorCode::BackendUnavailable, "down", "unavailable"),
            (ErrorCode::DeadlineExceeded, "slow", "failed"),
            (ErrorCode::BackendProtocol, "bad", "failed"),
        ];
        for (code, msg, want) in cases {
            let (a, _) = author(Err(error(code, msg)));
            let got = match a.complete(&prompt(), CancellationToken::new()).await {
                Err(AuthorModelError::Preempted(_)) => "preempted",
                Err(AuthorModelError::NotAdmitted(_)) => "not_admitted",
                Err(AuthorModelError::Unavailable(_)) => "unavailable",
                Err(AuthorModelError::Failed(_)) => "failed",
                Ok(_) => "ok",
            };
            assert_eq!(got, want, "{code:?} {msg}");
        }
    }

    #[tokio::test]
    async fn our_own_cancellation_is_reported_as_stopped() {
        let outer = CancellationToken::new();
        let fake = Arc::new(Fake {
            reply: Mutex::new(Err(error(ErrorCode::Cancelled, "Inference cancelled"))),
            seen: Mutex::new(None),
            cancel_outer: Some(outer.clone()),
        });
        let a = MindAuthor::new(fake, None);
        match a.complete(&prompt(), outer).await {
            Err(AuthorModelError::Preempted(why)) => assert!(why.contains("stopped")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn gate_waits_for_voice_foreground_and_busy_model() {
        struct Probe;
        impl sail_common::resources::HostProbe for Probe {
            fn sample(&self) -> std::io::Result<sail_protocol::resources::HostSample> {
                Ok(sail_protocol::resources::HostSample {
                    available_ram_bytes: u64::MAX / 4,
                    total_ram_bytes: u64::MAX / 4,
                    swap_used_bytes: 0,
                    swap_total_bytes: 0,
                    logical_cpus: 8,
                    memory_pressure_avg10: None,
                })
            }
        }
        let scheduler = Arc::new(ResourceScheduler::new(
            Arc::new(Probe),
            sail_common::resources::AdmissionPolicy::default(),
        ));
        let busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let b = busy.clone();
        let gate = MindGate::new(
            scheduler.clone(),
            Arc::new(move || b.load(std::sync::atomic::Ordering::SeqCst)),
            Arc::new(|| false),
        );
        assert!(gate.admit_background().is_ok());
        {
            let _turn = scheduler.voice_turn();
            assert!(gate.admit_background().is_err());
        }
        assert!(gate.admit_background().is_ok());
        busy.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(gate.admit_background().is_err());
    }
}
