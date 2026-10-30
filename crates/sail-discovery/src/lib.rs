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
}

impl DiscoverySubsystem {
    pub fn new() -> Self {
        Self { bus: None }
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
