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
/// Extract visual designs, infographics, and structured data for the companion Canvas panel.
/// Plain dialogue is never treated as a visual canvas artifact. Content is always treated as untrusted.
pub fn voice_artifacts(text: &str) -> Vec<VoiceArtifact> {
    let mut artifacts = Vec::new();
    let mut parts = text.split("```");
    parts.next();
    while let Some(fenced) = parts.next() {
        if let Some((language, content)) = fenced.split_once('\n') {
            let lang = language.trim().to_lowercase();
            let kind = match lang.as_str() {
                "mermaid" | "diagram" | "chart" => Some("mermaid"),
                "json" | "geojson" | "topojson" => Some("json"),
                "svg" => Some("svg"),
                _ => None,
            };
            if let Some(kind) = kind {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    artifacts.push(VoiceArtifact {
                        artifact_type: kind.into(),
                        content: trimmed.chars().take(16000).collect(),
                    });
                }
            }
        }
        parts.next();
        if artifacts.len() >= 8 {
            break;
        }
    }
    artifacts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_dialogue_never_creates_canvas_artifacts() {
        let dialogue = "Hello! How can I help you today? The system is running normally.";
        assert!(voice_artifacts(dialogue).is_empty());
    }

    #[test]
    fn designs_tooling_and_infographics_create_canvas_artifacts() {
        let text = "Here is the workflow:\n```mermaid\nflowchart TD\nA-->B\n```\nAnd structured stats:\n```json\n{\"cpu\":12,\"mem\":45}\n```";
        let artifacts = voice_artifacts(text);
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].artifact_type, "mermaid");
        assert_eq!(artifacts[1].artifact_type, "json");
    }
}
