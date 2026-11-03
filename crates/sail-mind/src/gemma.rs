//! Compatibility facade. All inference uses Mind's shared typed service and budgets.
use crate::{config::LocalModelConfig, service::MindService};
use sail_common::error::{Result, SailError};
use sail_protocol::{InferenceEventPayload, InferenceRequest, Message, Role, ToolChoice};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GemmaConfig {
    pub model_path: PathBuf,
    pub server_url: String,
    pub llama_server_binary: PathBuf,
    pub threads: usize,
    pub ctx_size: usize,
    pub port: u16,
    pub mmproj_path: Option<PathBuf>,
}
impl Default for GemmaConfig {
    fn default() -> Self {
        let config = LocalModelConfig::default();
        let port = reqwest::Url::parse(&config.endpoint)
            .ok()
            .and_then(|u| u.port_or_known_default())
            .unwrap_or(8081);
        Self {
            model_path: config.model_path,
            server_url: config.endpoint,
            llama_server_binary: config.server_binary,
            threads: config.threads,
            ctx_size: config.context_tokens,
            port,
            mmproj_path: config.projector,
        }
    }
}
#[derive(Clone)]
pub struct GemmaRunner {
    service: sail_protocol::Result<MindService>,
}
fn legacy(error: sail_protocol::InferenceError) -> SailError {
    SailError::Inference(error.to_string())
}
impl GemmaRunner {
    pub fn new(config: GemmaConfig) -> Self {
        Self::with_optional_scheduler(config, None)
    }
    /// Same as [`GemmaRunner::new`], but admission is decided by `scheduler` instead of the
    /// process-global broker. Tests and embedders need host measurements they control; the
    /// daemon keeps using [`sail_common::resources::global`].
    pub fn with_scheduler(
        config: GemmaConfig,
        scheduler: std::sync::Arc<sail_common::resources::ResourceScheduler>,
    ) -> Self {
        Self::with_optional_scheduler(config, Some(scheduler))
    }
    fn with_optional_scheduler(
        config: GemmaConfig,
        scheduler: Option<std::sync::Arc<sail_common::resources::ResourceScheduler>>,
    ) -> Self {
        let service = LocalModelConfig::from_env().and_then(|mut local| {
            local.endpoint = config.server_url;
            local.model_path = config.model_path;
            local.server_binary = config.llama_server_binary;
            local.projector = config.mmproj_path;
            local.threads = config.threads;
            local.context_tokens = config.ctx_size;
            local.managed = local.managed && local.model_path.is_file();
            match scheduler {
                Some(scheduler) => {
                    let limits = sail_protocol::ResourceLimits::default();
                    let backend = crate::local::LocalGemmaBackend::with_scheduler(
                        local,
                        limits.max_parallel,
                        scheduler.clone(),
                    )?;
                    MindService::with_scheduler(
                        vec![std::sync::Arc::new(backend)],
                        limits,
                        scheduler,
                    )
                }
                None => MindService::new(local, sail_protocol::ResourceLimits::default()),
            }
        });
        Self { service }
    }
    pub fn from_service(service: MindService) -> Self {
        Self {
            service: Ok(service),
        }
    }
    pub fn service(&self) -> sail_protocol::Result<MindService> {
        self.service.clone()
    }
    pub async fn ensure_server_running(&mut self) -> Result<bool> {
        self.service()
            .map_err(legacy)?
            .start()
            .await
            .map_err(legacy)?;
        Ok(true)
    }
    pub async fn complete(&self, prompt: &str) -> Result<String> {
        let request = InferenceRequest {
            messages: vec![
                Message::text(Role::System, "You are SAIL, a concise local assistant."),
                Message::text(Role::User, prompt),
            ],
            ..InferenceRequest::text(prompt)
        };
        let response = self
            .service()
            .map_err(legacy)?
            .infer(request, CancellationToken::new(), None)
            .await
            .map_err(legacy)?;
        Ok(response.message.content.text().unwrap_or_default().into())
    }
    pub async fn health(&self) -> Value {
        match self.service() {
            Err(error) => json!({"ready":false,"error":error}),
            Ok(service) => {
                let status = service.status().await;
                let model = status.models.first();
                json!({"ready":model.is_some_and(|s|s.state==sail_protocol::ModelState::Ready),"model":model.map(|s|&s.model),"modalities":{"vision":model.and_then(|s|s.capabilities.as_ref()).is_some_and(|c|c.vision),"text":model.and_then(|s|s.capabilities.as_ref()).is_some_and(|c|c.text)},"mind":status})
            }
        }
    }
    pub async fn stream_chat(
        &self,
        messages: Vec<Value>,
        tools: Value,
        tx: tokio::sync::mpsc::Sender<String>,
    ) -> Result<Value> {
        let service = self.service().map_err(legacy)?;
        let messages = messages
            .into_iter()
            .map(serde_json::from_value)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let tools = serde_json::from_value(tools)?;
        let mut request = InferenceRequest {
            messages,
            tools,
            ..InferenceRequest::text("")
        };
        request.tool_choice = if request.tools.is_empty() {
            ToolChoice::None
        } else {
            ToolChoice::Auto
        };
        let (events, mut rx) = tokio::sync::mpsc::channel::<sail_protocol::InferenceEvent>(32);
        let forward = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let InferenceEventPayload::TextDelta { text } = event.payload
                    && tx.send(text).await.is_err()
                {
                    break;
                }
            }
        });
        struct Forward(tokio::task::JoinHandle<()>);
        impl Drop for Forward {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let mut forward = Forward(forward);
        let response = service
            .infer_with_priority(
                request,
                CancellationToken::new(),
                Some(events),
                sail_protocol::resources::WorkPriority::Voice,
            )
            .await
            .map_err(legacy)?;
        let _ = (&mut forward.0).await;
        Ok(serde_json::to_value(response.message)?)
    }
}
