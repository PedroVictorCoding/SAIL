use async_trait::async_trait;
use tracing::info;

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

pub struct DiscoverySubsystem {
    bus: Option<EventBus>,
    memory: Option<sail_arena::memory::MemoryClient>,
}

impl DiscoverySubsystem {
    pub fn new() -> Self {
        Self {
            bus: None,
            memory: None,
        }
    }
    pub fn with_memory(mut self, client: sail_arena::memory::MemoryClient) -> Self {
        if client.principal() == SubsystemId::Discovery {
            self.memory = Some(client);
        }
        self
    }
}

impl Default for DiscoverySubsystem {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Subsystem for DiscoverySubsystem {
    fn id(&self) -> SubsystemId {
        SubsystemId::Discovery
    }

    async fn init(&mut self, bus: &EventBus) -> Result<()> {
        self.bus = Some(bus.clone());
        info!("SAIL Discovery initialized (curiosity & autonomous research engine)");
        Ok(())
    }

    async fn start(&mut self) -> Result<()> {
        info!("SAIL Discovery subsystem started");
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
        if let EventPayload::KnowledgeGapDetected {
            topic,
            priority,
            hypothesis,
        } = &event.payload
        {
            info!(
                topic = %topic,
                priority = %priority,
                hypothesis = %hypothesis,
                "SAIL Discovery detected knowledge gap from Arena; triggering Flow"
            );

            if let Some(bus) = &self.bus {
                if let Some(memory) = &self.memory {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as i64;
                    if let Ok(context) = memory.context(event.id, topic, now) {
                        if !context.is_empty() {
                            let _=bus.publish(SailEvent::new(self.id(),EventPayload::CanvasRender{canvas_id:format!("research-memory-{}",event.id),artifact_type:"text".into(),content:format!("Owner-approved research context (claims retain their status):\n{context}")}).with_target(SubsystemId::Canvas));
                        }
                    }
                }
                let trigger = SailEvent::new(
                    self.id(),
                    EventPayload::ResearchTriggerDispatched {
                        workflow_id: format!("research-{topic}"),
                        query: format!("Deep research on {topic}: {hypothesis}"),
                    },
                )
                .with_target(SubsystemId::Flow);

                let _ = bus.publish(trigger);
            }
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        info!("SAIL Discovery subsystem shutting down");
        Ok(())
    }
}
