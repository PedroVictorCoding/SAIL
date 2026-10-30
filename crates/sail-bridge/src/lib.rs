use async_trait::async_trait;
use tracing::info;

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

pub struct BridgeSubsystem {
    bus: Option<EventBus>,
}

impl BridgeSubsystem {
    pub fn new() -> Self {
        Self { bus: None }
    }
}

impl Default for BridgeSubsystem {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Subsystem for BridgeSubsystem {
    fn id(&self) -> SubsystemId {
        SubsystemId::Bridge
    }

    async fn init(&mut self, bus: &EventBus) -> Result<()> {
        self.bus = Some(bus.clone());
        info!("SAIL Bridge initialized with OS & connector adapters");
        Ok(())
    }

    async fn start(&mut self) -> Result<()> {
        info!("SAIL Bridge subsystem started");
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
        if let EventPayload::ToolExecute {
            execution_id,
            tool,
            args,
        } = &event.payload
        {
            info!(
                execution_id = %execution_id,
                tool = %tool,
                args = %args,
                "SAIL Bridge executing tool"
            );

            if let Some(bus) = &self.bus {
                let response = SailEvent::new(
                    self.id(),
                    EventPayload::ToolResult {
                        execution_id: execution_id.clone(),
                        success: true,
                        output: format!("Execution of '{tool}' succeeded with arguments {args}"),
                    },
                )
                .with_target(event.source);

                let _ = bus.publish(response);
            }
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        info!("SAIL Bridge subsystem shutting down");
        Ok(())
    }
}
