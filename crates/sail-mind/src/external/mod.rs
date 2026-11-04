//! Opt-in external text providers. Local inference never falls through to this path.
pub mod ledger;
#[cfg(test)]
mod tests;

use crate::backend::InferenceBackend;
use async_trait::async_trait;
use chrono::Utc;
use ledger::UsageLedger;
use sail_common::{
    resources::{ResourceClaim, ResourceScheduler},
    types::SubsystemId,
};
use sail_guardian::{
    Capability, CredentialStore, GuardianExecutionBroker,
    egress::{PreparedText, prepare_text},
};
use sail_protocol::{
    external::{AttemptOutcome, EgressReceipt},
    *,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Implemented by trusted Bridge connectors. No tools, history or memory travel here.
#[async_trait]
pub trait ExternalTextTransport: Send + Sync {
    fn endpoint(&self) -> &str;
    async fn send(
        &self,
        text: &str,
        model: &str,
        generation: &Generation,
        response_limit: usize,
    ) -> std::result::Result<ExternalReply, ExternalFailure>;
}

pub struct ExternalReply {
    pub text: String,
    pub model: String,
    pub usage: Usage,
    pub finish_reason: String,
}

/// Provider errors contain no response body, prompt, URL or credential.
pub enum ExternalFailure {
    RateLimited { retry_after_seconds: Option<u64> },
    Rejected,
    Unavailable,
    Protocol,
}

pub struct ExternalBackend {
    provider: String,
    model: String,
    daily_cap: u32,
    transport: Arc<dyn ExternalTextTransport>,
    ledger: Arc<UsageLedger>,
    broker: Arc<GuardianExecutionBroker>,
    vault: Arc<CredentialStore>,
    entity_names: Vec<String>,
    scheduler: Arc<ResourceScheduler>,
}

/// Explicit construction is the opt-in. No discovery of installed credentials or CLIs.
pub struct ExternalProviderConfig {
    pub provider: String,
    pub model: String,
    pub daily_cap: u32,
    pub entity_names: Vec<String>,
}

impl ExternalBackend {
    pub fn new(
        config: ExternalProviderConfig,
        transport: Arc<dyn ExternalTextTransport>,
        ledger: Arc<UsageLedger>,
        broker: Arc<GuardianExecutionBroker>,
        vault: Arc<CredentialStore>,
        scheduler: Arc<ResourceScheduler>,
    ) -> Result<Self> {
        if config.provider.is_empty()
            || config.provider.len() > 64
            || !config
                .provider
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            || config.model.is_empty()
            || config.model.len() > 128
            || config.daily_cap == 0
            || config.daily_cap > 10_000
        {
            return Err(error(
                ErrorCode::InvalidConfiguration,
                "invalid external provider configuration",
            ));
        }
        Ok(Self {
            provider: config.provider,
            model: config.model,
            daily_cap: config.daily_cap,
            entity_names: config.entity_names,
            transport,
            ledger,
            broker,
            vault,
            scheduler,
        })
    }

    fn prepare(&self, request: &InferenceRequest, limits: &ResourceLimits) -> Result<PreparedText> {
        crate::validation::validate_limits(limits)?;
        crate::validation::validate_request(request, limits)?;
        if request
            .model
            .as_ref()
            .is_some_and(|m| m != &self.provider && m != &self.model)
        {
            return Err(error(
                ErrorCode::UnknownModel,
                "external request names a different provider",
            ));
        }
        prepare_text(request, &self.vault, &self.entity_names).map_err(|_| {
            error(
                ErrorCode::InvalidRequest,
                "external explicit-text input rejected by Guardian",
            )
        })
    }

    pub fn approval_capability(
        &self,
        request: &InferenceRequest,
        limits: &ResourceLimits,
    ) -> Result<Capability> {
        Ok(self.prepare(request, limits)?.capability(
            &self.provider,
            self.transport.endpoint(),
            &self.model,
        ))
    }

    /// A denied call creates the ordinary Guardian approval; callers retry the identical
    /// request with the resulting token. Quota is not reserved until authorization succeeds.
    pub async fn infer_authorized(
        &self,
        request: &InferenceRequest,
        token: Option<Uuid>,
        cancel: &CancellationToken,
        limits: &ResourceLimits,
    ) -> Result<InferenceResponse> {
        if cancel.is_cancelled() {
            return Err(error(
                ErrorCode::Cancelled,
                "external request cancelled before dispatch",
            ));
        }
        let prepared = self.prepare(request, limits)?;
        let capability =
            prepared.capability(&self.provider, self.transport.endpoint(), &self.model);
        // The future is created inside Guardian and owns the exact immutable prepared text.
        self.broker
            .authorize_and_execute(
                SubsystemId::Mind,
                capability,
                token,
                Some(request.request_id),
                || self.dispatch(request, prepared, cancel, limits),
            )
            .map_err(|denied| {
                error(
                    ErrorCode::ForbiddenEndpoint,
                    format!("external inference denied: {denied}"),
                )
            })?
            .await
    }

    async fn dispatch(
        &self,
        request: &InferenceRequest,
        prepared: PreparedText,
        cancel: &CancellationToken,
        limits: &ResourceLimits,
    ) -> Result<InferenceResponse> {
        // Voice may interrupt this attempt without cancelling the parent durable task.
        let dispatch_cancel = cancel.child_token();
        let _lease = self
            .scheduler
            .try_admit(
                ResourceClaim::inference(resources::WorkPriority::Background),
                dispatch_cancel.clone(),
            )
            .map_err(|_| {
                error(
                    ErrorCode::Busy,
                    "external inference paused by resource admission",
                )
            })?;
        if cancel.is_cancelled() {
            return Err(error(
                ErrorCode::Cancelled,
                "external request cancelled before reservation",
            ));
        }
        let receipt = EgressReceipt {
            version: external::VERSION,
            request_id: request.request_id,
            provider: self.provider.clone(),
            model: self.model.clone(),
            purpose: "explicit_text".into(),
            request_sha256: prepared.sha256().into(),
            input_bytes: prepared.text().len(),
            redactions: prepared.redactions(),
            reserved_at: Utc::now().timestamp(),
            finished_at: None,
            outcome: AttemptOutcome::Reserved,
            input_tokens: None,
            output_tokens: None,
        };
        self.ledger
            .reserve(&receipt, self.daily_cap)
            .map_err(|e| error(ErrorCode::ResourceLimit, e))?;
        let mut guard = AttemptGuard {
            ledger: self.ledger.clone(),
            id: request.request_id,
            done: false,
            dispatched: false,
        };
        if cancel.is_cancelled() {
            return Err(error(
                ErrorCode::Cancelled,
                "external request cancelled before transmission",
            ));
        }
        self.ledger
            .dispatch(request.request_id)
            .map_err(|e| error(ErrorCode::BackendUnavailable, e))?;
        guard.dispatched = true;
        let started = Instant::now();
        let result = tokio::select! {
            biased;
            _ = dispatch_cancel.cancelled() => return Err(error(
                if cancel.is_cancelled() { ErrorCode::Cancelled } else { ErrorCode::Busy },
                "external request interrupted; delivery may have occurred; reconcile its receipt before retrying")),
            result = tokio::time::timeout(Duration::from_millis(limits.inference_timeout_ms),
                self.transport.send(prepared.text(), &self.model, &request.generation, limits.max_response_bytes)) => result,
        };
        let reply = match result {
            Err(_) => {
                return Err(error(
                    ErrorCode::DeadlineExceeded,
                    "external provider deadline exceeded; delivery may have occurred",
                ));
            }
            Ok(Err(ExternalFailure::RateLimited {
                retry_after_seconds,
            })) => {
                let seconds = retry_after_seconds
                    .unwrap_or(3600)
                    .clamp(1, i64::MAX as u64);
                guard.finish(
                    AttemptOutcome::RateLimited,
                    None,
                    None,
                    Some(Utc::now().timestamp().saturating_add(seconds as i64)),
                )?;
                return Err(error(
                    ErrorCode::ResourceLimit,
                    "external provider reported a usage limit",
                ));
            }
            Ok(Err(ExternalFailure::Rejected)) => {
                guard.finish(AttemptOutcome::Failed, None, None, None)?;
                return Err(error(
                    ErrorCode::BackendUnavailable,
                    "external provider rejected the request",
                ));
            }
            Ok(Err(ExternalFailure::Protocol)) => {
                return Err(error(
                    ErrorCode::BackendProtocol,
                    "invalid external provider response",
                ));
            }
            Ok(Err(ExternalFailure::Unavailable)) => {
                return Err(error(
                    ErrorCode::BackendUnavailable,
                    "external transport failed; delivery may have occurred",
                ));
            }
            Ok(Ok(reply)) => reply,
        };
        if reply.text.is_empty()
            || reply.text.len() > limits.max_response_bytes
            || reply.model.is_empty()
            || reply.model.len() > 256
        {
            return Err(error(
                ErrorCode::BackendProtocol,
                "external response exceeds the text contract",
            ));
        }
        guard.finish(
            AttemptOutcome::Completed,
            reply.usage.input_tokens,
            reply.usage.output_tokens,
            None,
        )?;
        Ok(InferenceResponse {
            version: VERSION,
            request_id: request.request_id,
            model: format!("{}:{}", self.provider, reply.model),
            message: Message::text(Role::Assistant, self.vault.redact_text(&reply.text)),
            finish_reason: reply.finish_reason,
            usage: reply.usage,
            timings: Timings {
                inference_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                ..Default::default()
            },
        })
    }
}

struct AttemptGuard {
    ledger: Arc<UsageLedger>,
    id: Uuid,
    done: bool,
    dispatched: bool,
}
impl AttemptGuard {
    fn finish(
        &mut self,
        outcome: AttemptOutcome,
        input: Option<u32>,
        output: Option<u32>,
        cooldown: Option<i64>,
    ) -> Result<()> {
        self.ledger
            .finish(self.id, outcome, input, output, cooldown)
            .map_err(|e| error(ErrorCode::BackendUnavailable, e))?;
        self.done = true;
        Ok(())
    }
}
impl Drop for AttemptGuard {
    fn drop(&mut self) {
        if !self.done {
            // Failure to write leaves Reserved/Dispatched, also non-replayable.
            let _ = self.ledger.finish(
                self.id,
                if self.dispatched {
                    AttemptOutcome::Uncertain
                } else {
                    AttemptOutcome::Cancelled
                },
                None,
                None,
                None,
            );
        }
    }
}

fn error(code: ErrorCode, message: impl Into<String>) -> InferenceError {
    InferenceError::new(code, message)
}

#[async_trait]
impl InferenceBackend for ExternalBackend {
    fn is_external(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        &self.provider
    }
    fn aliases(&self) -> Vec<String> {
        vec![]
    }
    async fn start(&self) -> Result<ModelStatus> {
        Ok(self.status().await)
    }
    async fn status(&self) -> ModelStatus {
        ModelStatus {
            version: VERSION,
            state: ModelState::Ready,
            model: self.provider.clone(),
            capabilities: Some(ModelCapabilities {
                model: self.model.clone(),
                text: true,
                vision: false,
                tools: false,
                // The endpoint has not reported a context window; do not invent one.
                context_tokens: 0,
            }),
            requested_backend: ComputeBackend::Cpu,
            observed_backend: None,
            owned_process: false,
            process_id: None,
            last_error: None,
            active_profile: None,
            hardware: None,
            load: None,
        }
    }
    async fn infer(
        &self,
        request: &InferenceRequest,
        events: Option<&mpsc::Sender<InferenceEvent>>,
        cancel: &CancellationToken,
        limits: &ResourceLimits,
    ) -> Result<InferenceResponse> {
        let response = self.infer_authorized(request, None, cancel, limits).await?;
        if let Some(events) = events {
            let _ = events.try_send(InferenceEvent {
                version: VERSION,
                request_id: request.request_id,
                payload: InferenceEventPayload::Completed {
                    response: response.clone(),
                },
            });
        }
        Ok(response)
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
