pub mod gemma;
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
        Self {
            bus: None,
            runner: GemmaRunner::new(config),
            model_path: p,
            voice: VoiceModelServices::new(),
        }
    }

    pub fn with_gemma_config(mut self, config: GemmaConfig) -> Self {
        self.model_path = config.model_path.clone();
        self.runner = GemmaRunner::new(config);
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
        if self.model_path.exists() {
            info!(path = ?self.model_path, "SAIL Mind detected local Gemma 4 weights");
            if let Err(error) = self.runner.ensure_server_running().await {
                warn!(%error,"Local model startup failed; see .local/llama-server.log");
            }
        } else {
            warn!(path = ?self.model_path, "SAIL Mind model weights file not found; inference unavailable until a local server is configured");
        }
        Ok(())
    }

    async fn start(&mut self) -> Result<()> {
        info!("SAIL Mind inference subsystem started (Gemma 4 integration active)");
        if let Some(bus) = &self.bus {
            let event = SailEvent::new(
                self.id(),
                EventPayload::SystemReady {
                    subsystem: self.id(),
                },
            );
            let _ = bus.publish(event);
        }
        Ok(())
    }

    async fn handle_event(&self, event: &SailEvent) -> Result<()> {
        if let EventPayload::InferenceRequested {
            request_id,
            prompt,
            model_preference: _,
        } = &event.payload
        {
            info!(
                request_id = %request_id,
                prompt_len = prompt.len(),
                "SAIL Mind generating response via Gemma 4"
            );

            let content = self.runner.complete(prompt).await?;
            let tokens_used = content.split_whitespace().count();

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
        Ok(())
    }
}
