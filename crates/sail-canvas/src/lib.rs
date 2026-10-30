use async_trait::async_trait;
use tracing::info;

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

pub struct CanvasSubsystem {
    bus: Option<EventBus>,
}

impl CanvasSubsystem {
    pub fn new() -> Self {
        Self { bus: None }
    }
}

impl Default for CanvasSubsystem {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Subsystem for CanvasSubsystem {
    fn id(&self) -> SubsystemId {
        SubsystemId::Canvas
    }

    async fn init(&mut self, bus: &EventBus) -> Result<()> {
        self.bus = Some(bus.clone());
        info!("SAIL Canvas initialized (document/diagram rendering engine)");
        Ok(())
    }

    async fn start(&mut self) -> Result<()> {
        info!("SAIL Canvas subsystem started");
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
        if let EventPayload::CanvasRender {
            canvas_id,
            artifact_type,
            content,
        } = &event.payload
        {
            info!(
                canvas_id = %canvas_id,
                artifact_type = %artifact_type,
                content_len = content.len(),
                "SAIL Canvas rendering visual artifact directly to client"
            );
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        info!("SAIL Canvas subsystem shutting down");
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VoiceArtifact {
    pub artifact_type: String,
    pub content: String,
}
/// Extract structured output for the companion panel. Content is always treated as untrusted.
pub fn voice_artifacts(text: &str) -> Vec<VoiceArtifact> {
    let mut artifacts = Vec::new();
    let mut parts = text.split("```");
    parts.next();
    while let Some(fenced) = parts.next() {
        if let Some((language, content)) = fenced.split_once('\n') {
            let kind = match language.trim() {
                "mermaid" => "mermaid",
                "json" => "json",
                _ => "text",
            };
            artifacts.push(VoiceArtifact {
                artifact_type: kind.into(),
                content: content.chars().take(16000).collect(),
            });
        }
        parts.next();
        if artifacts.len() >= 8 {
            break;
        }
    }
    if artifacts.is_empty() {
        artifacts.push(VoiceArtifact {
            artifact_type: "text".into(),
            content: text.chars().take(16000).collect(),
        });
    }
    artifacts
}
