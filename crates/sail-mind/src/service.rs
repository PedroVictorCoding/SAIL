use crate::{
    backend::InferenceBackend,
    config::LocalModelConfig,
    local::{LocalGemmaBackend, emit},
    validation::{validate_limits, validate_request},
};
use sail_protocol::*;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
#[derive(Clone)]
pub struct MindService {
    inner: Arc<Inner>,
}
struct Inner {
    backends: Vec<Arc<dyn InferenceBackend>>,
    limits: ResourceLimits,
    semaphore: Arc<Semaphore>,
    state: Mutex<State>,
    lifecycle: CancellationToken,
}
struct State {
    accepting: bool,
    jobs: HashMap<Uuid, Job>,
    metrics: InferenceMetrics,
}
struct Job {
    cancel: CancellationToken,
    running: bool,
}
struct JobGuard {
    inner: Arc<Inner>,
    id: Uuid,
    done: bool,
    started: Option<Instant>,
}
impl JobGuard {
    fn finish(&mut self, result: &Result<InferenceResponse>) {
        let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        state.jobs.remove(&self.id);
        match result {
            Ok(response) => {
                state.metrics.completed += 1;
                state.metrics.last_first_token_ms = response.timings.first_token_ms;
            }
            Err(error) if error.code == ErrorCode::Cancelled => state.metrics.cancelled += 1,
            Err(_) => state.metrics.failed += 1,
        }
        state.metrics.total_inference_ms +=
            self.started.map_or(0, |t| t.elapsed().as_millis() as u64);
        self.done = true;
    }
}
impl Drop for JobGuard {
    fn drop(&mut self) {
        if !self.done {
            let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(job) = state.jobs.remove(&self.id) {
                job.cancel.cancel();
                state.metrics.cancelled += 1;
                tracing::info!(request_id=%self.id,"Inference caller dropped; cancelled request and released its budget");
            }
        }
    }
}
impl MindService {
    pub fn new(config: LocalModelConfig, limits: ResourceLimits) -> Result<Self> {
        validate_limits(&limits)?;
        let backend = Arc::new(LocalGemmaBackend::new(config, limits.max_parallel)?);
        Self::with_backends(vec![backend], limits)
    }
    pub fn from_env() -> Result<Self> {
        Self::new(LocalModelConfig::from_env()?, ResourceLimits::default())
    }
    pub fn with_backends(
        backends: Vec<Arc<dyn InferenceBackend>>,
        limits: ResourceLimits,
    ) -> Result<Self> {
        validate_limits(&limits)?;
        if backends.is_empty() {
            return Err(InferenceError::new(
                ErrorCode::InvalidConfiguration,
                "At least one explicit local model backend is required",
            ));
        }
        let mut names = HashMap::new();
        for (index, backend) in backends.iter().enumerate() {
            for name in std::iter::once(backend.name().to_owned()).chain(backend.aliases()) {
                if let Some(previous) = names.insert(name, index)
                    && previous != index
                {
                    return Err(InferenceError::new(
                        ErrorCode::InvalidConfiguration,
                        "Model names/aliases must be unique",
                    ));
                }
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                backends,
                lifecycle: CancellationToken::new(),
                semaphore: Arc::new(Semaphore::new(limits.max_parallel)),
                limits,
                state: Mutex::new(State {
                    accepting: true,
                    jobs: HashMap::new(),
                    metrics: InferenceMetrics::default(),
                }),
            }),
        })
    }
    pub async fn start(&self) -> Result<()> {
        for backend in &self.inner.backends {
            tokio::select! {biased;_ = self.inner.lifecycle.cancelled()=>return Err(InferenceError::new(ErrorCode::ShuttingDown,"Mind startup cancelled by shutdown")),result=backend.start()=>{result?;}}
        }
        Ok(())
    }
    pub fn cancel(&self, id: Uuid) -> bool {
        let state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(job) = state.jobs.get(&id) {
            job.cancel.cancel();
            true
        } else {
            false
        }
    }
    /// Cheap admission signal covering active and queued inference, without backend probes.
    pub fn has_work(&self) -> bool {
        !self
            .inner
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .jobs
            .is_empty()
    }

    pub async fn status(&self) -> MindStatus {
        let mut models = Vec::new();
        for backend in &self.inner.backends {
            models.push(backend.status().await);
        }
        let state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut metrics = state.metrics.clone();
        metrics.active = state.jobs.values().filter(|j| j.running).count();
        metrics.queued = state.jobs.len() - metrics.active;
        let hardware = models.iter().find_map(|m| m.hardware.clone());
        MindStatus {
            version: VERSION,
            accepting_requests: state.accepting,
            models,
            metrics,
            limits: self.inner.limits.clone(),
            hardware,
        }
    }
    pub async fn infer(
        &self,
        request: InferenceRequest,
        cancel: CancellationToken,
        events: Option<mpsc::Sender<InferenceEvent>>,
    ) -> Result<InferenceResponse> {
        if let Err(error) = validate_request(&request, &self.inner.limits) {
            self.reject();
            return Err(error);
        }
        let backend = if let Some(model) = &request.model {
            self.inner
                .backends
                .iter()
                .find(|b| b.name() == model || b.aliases().contains(model))
        } else {
            self.inner.backends.first()
        };
        let Some(backend) = backend else {
            self.reject();
            return Err(InferenceError::new(
                ErrorCode::UnknownModel,
                "Requested model is not explicitly registered",
            ));
        };
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
            if !state.accepting {
                state.metrics.rejected += 1;
                return Err(InferenceError::new(
                    ErrorCode::ShuttingDown,
                    "Mind is shutting down",
                ));
            }
            if state.jobs.contains_key(&request.request_id) {
                state.metrics.rejected += 1;
                return Err(InferenceError::new(
                    ErrorCode::InvalidRequest,
                    "Request ID is already active",
                ));
            }
            if state.jobs.len() >= self.inner.limits.max_parallel + self.inner.limits.max_queued {
                state.metrics.rejected += 1;
                return Err(InferenceError::new(
                    ErrorCode::Busy,
                    "Mind inference queue is full",
                ));
            }
            state.jobs.insert(
                request.request_id,
                Job {
                    cancel: cancel.clone(),
                    running: false,
                },
            );
            state.metrics.submitted += 1;
        }
        let mut guard = JobGuard {
            inner: self.inner.clone(),
            id: request.request_id,
            done: false,
            started: None,
        };
        let queued = Instant::now();
        let permit = tokio::select! {biased;_ = cancel.cancelled()=>Err(InferenceError::new(ErrorCode::Cancelled,"Cancelled while queued")),result=tokio::time::timeout(Duration::from_millis(self.inner.limits.queue_timeout_ms),self.inner.semaphore.clone().acquire_owned())=>match result{Ok(Ok(permit))=>Ok(permit),_=>Err(InferenceError::new(ErrorCode::QueueTimeout,"Inference queue deadline exceeded"))}};
        let result = match permit {
            Err(error) => Err(error),
            Ok(_permit) => {
                guard.started = Some(Instant::now());
                self.inner
                    .state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .jobs
                    .get_mut(&request.request_id)
                    .expect("registered job")
                    .running = true;
                let queue_ms = queued.elapsed().as_millis() as u64;
                let operation = async {
                    emit(
                        events.as_ref(),
                        request.request_id,
                        InferenceEventPayload::Started {
                            model: backend.name().into(),
                        },
                    )
                    .await?;
                    let mut response = backend
                        .infer(&request, events.as_ref(), &cancel, &self.inner.limits)
                        .await?;
                    response.timings.queue_ms = queue_ms;
                    emit(
                        events.as_ref(),
                        request.request_id,
                        InferenceEventPayload::Completed {
                            response: response.clone(),
                        },
                    )
                    .await?;
                    Ok(response)
                };
                tokio::select! {biased;_ = cancel.cancelled()=>Err(InferenceError::new(ErrorCode::Cancelled,"Inference cancelled")),result=tokio::time::timeout(Duration::from_millis(self.inner.limits.inference_timeout_ms),operation)=>result.unwrap_or_else(|_|Err(InferenceError::new(ErrorCode::DeadlineExceeded,"Inference deadline exceeded")))}
            }
        };
        if let Err(error) = &result {
            tracing::warn!(request_id=%request.request_id,code=?error.code,error=%error,"Local inference failed");
            if let Some(events) = events {
                let _ = events.try_send(InferenceEvent {
                    version: VERSION,
                    request_id: request.request_id,
                    payload: InferenceEventPayload::Failed {
                        error: error.clone(),
                    },
                });
            }
        }
        guard.finish(&result);
        result
    }
    fn reject(&self) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .metrics
            .rejected += 1;
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.lifecycle.cancel();
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
            state.accepting = false;
            for job in state.jobs.values() {
                job.cancel.cancel();
            }
        }
        // Backend shutdown cannot strand tasks waiting on permits: all tokens above are cancelled.
        for backend in &self.inner.backends {
            backend.shutdown().await?;
        }
        Ok(())
    }
}
