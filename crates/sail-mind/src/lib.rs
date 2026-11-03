pub mod backend;
pub mod bench;
pub mod config;
pub mod embedding;
pub mod gemma;
pub mod hardware;
pub mod local;
pub mod profile;
pub mod service;
mod validation;
pub use bench::{BenchmarkRunner, BenchmarkScenario};
pub use config::{CpuMoe, LocalModelConfig};
pub use hardware::HardwareProbe;
pub use profile::InferenceProfileManager;
pub use sail_protocol as protocol;
pub use service::MindService;
pub use tokio_util::sync::CancellationToken;
pub mod voice;
pub use voice::VoiceModelServices;

use async_trait::async_trait;
use std::path::PathBuf;
use tracing::{info, warn};

pub use gemma::{GemmaConfig, GemmaRunner};

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

pub struct MindSubsystem {
    bus: Option<EventBus>,
    runner: GemmaRunner,
    model_path: PathBuf,
    voice: VoiceModelServices,
}

impl MindSubsystem {
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        let p = model_path.into();
        let config = GemmaConfig {
            model_path: p.clone(),
            ..Default::default()
        };
        let runner = GemmaRunner::new(config);
        let mut voice = VoiceModelServices::new();
        if let Ok(service) = runner.service() {
            voice = voice.with_inference(service);
        }
        Self {
            bus: None,
            runner,
            model_path: p,
            voice,
        }
    }

    pub fn try_new(model_path: impl Into<PathBuf>) -> sail_protocol::Result<Self> {
        let path = model_path.into();
        let mut config = LocalModelConfig::from_env()?;
        config.model_path = path.clone();
        let service = MindService::new(config, Default::default())?;
        Ok(Self {
            bus: None,
            model_path: path,
            runner: GemmaRunner::from_service(service.clone()),
            voice: VoiceModelServices::new().with_inference(service),
        })
    }
    pub fn model_service(&self) -> sail_protocol::Result<MindService> {
        self.runner.service()
    }
    pub fn with_gemma_config(mut self, config: GemmaConfig) -> Self {
        self.model_path = config.model_path.clone();
        self.runner = GemmaRunner::new(config);
        if let Ok(service) = self.runner.service() {
            self.voice = self.voice.clone().with_inference(service);
        }
        self
    }
    /// Same as [`MindSubsystem::with_gemma_config`], but admission is decided by `scheduler`
    /// instead of the process-global broker. Tests and embedders need host measurements they
    /// control; the daemon keeps using the shared broker.
    pub fn with_gemma_config_and_scheduler(
        mut self,
        config: GemmaConfig,
        scheduler: std::sync::Arc<sail_common::resources::ResourceScheduler>,
    ) -> Self {
        self.model_path = config.model_path.clone();
        self.runner = GemmaRunner::with_scheduler(config, scheduler);
        if let Ok(service) = self.runner.service() {
            self.voice = self.voice.clone().with_inference(service);
        }
        self
    }
    pub fn voice_services(&self) -> VoiceModelServices {
        self.voice.clone()
    }

    pub fn model_path(&self) -> &PathBuf {
        &self.model_path
    }
}

#[async_trait]
impl Subsystem for MindSubsystem {
    fn id(&self) -> SubsystemId {
        SubsystemId::Mind
    }

    async fn init(&mut self, bus: &EventBus) -> Result<()> {
        self.bus = Some(bus.clone());
        let service = self
            .model_service()
            .map_err(|e| sail_common::SailError::Config(e.to_string()))?;
        let bus = bus.clone();
        tokio::spawn(async move {
            match service.start().await {
                Ok(()) => {
                    info!("SAIL Mind local inference is ready");
                    let _ = bus.publish(SailEvent::new(
                        SubsystemId::Mind,
                        EventPayload::SystemReady {
                            subsystem: SubsystemId::Mind,
                        },
                    ));
                }
                Err(error) => {
                    warn!(%error,"Local Mind startup unavailable; status remains visible and inference can retry")
                }
            }
        });

        Ok(())
    }

    async fn start(&mut self) -> Result<()> {
        info!("SAIL Mind inference subsystem started (Qwen 3.6 integration active)");

        Ok(())
    }

    async fn handle_event(&self, event: &SailEvent) -> Result<()> {
        if let EventPayload::InferenceRequested {
            request_id,
            prompt,
            model_preference,
        } = &event.payload
        {
            info!(
                request_id = %request_id,
                prompt_len = prompt.len(),
                "SAIL Mind generating response via local model (Qwen 3.6)"
            );

            let service = self
                .model_service()
                .map_err(|e| sail_common::SailError::Inference(e.to_string()))?;
            let mut request = sail_protocol::InferenceRequest::text(prompt.clone());
            request.model = model_preference.clone();
            let response = match service.infer(request, CancellationToken::new(), None).await {
                Ok(response) => response,
                Err(error) => {
                    if let Some(bus) = &self.bus {
                        let _ = bus.publish(
                            SailEvent::new(
                                self.id(),
                                EventPayload::InferenceFailed {
                                    request_id: request_id.clone(),
                                    error,
                                },
                            )
                            .with_target(event.source),
                        );
                    }
                    return Ok(());
                }
            };
            let content = response
                .message
                .content
                .text()
                .unwrap_or_default()
                .to_owned();
            let tokens_used = response.usage.output_tokens.unwrap_or(0) as usize;

            if let Some(bus) = &self.bus {
                let response = SailEvent::new(
                    self.id(),
                    EventPayload::InferenceCompleted {
                        request_id: request_id.clone(),
                        content,
                        tokens_used,
                    },
                )
                .with_target(event.source);

                let _ = bus.publish(response);
            }
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        info!("SAIL Mind subsystem shutting down");
        if let Ok(service) = self.model_service() {
            service
                .shutdown()
                .await
                .map_err(|e| sail_common::SailError::Inference(e.to_string()))?;
        }
        Ok(())
    }
}
