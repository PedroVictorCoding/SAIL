pub mod aistudio;
pub mod browser;
pub mod reader;
pub mod spotify;
pub mod studio_audio;

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
    registry: std::sync::Arc<sail_agents::tools::ToolRegistry>,
    scheduler: Option<std::sync::Arc<sail_agents::tools::ToolScheduler>>,
}

impl BridgeSubsystem {
    pub fn new() -> Self {
        let registry = std::sync::Arc::new(sail_agents::tools::ToolRegistry::default());
        sail_agents::tools::register_builtin_time(&registry)
            .expect("valid built-in time descriptor");
        studio_audio::register_studio_audio_tool(&registry)
            .expect("valid studio audio tool descriptor");
        Self {
            bus: None,
            registry,
            scheduler: None,
        }
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
        let guardian = sail_guardian::global_broker()
            .map_err(|error| sail_common::error::SailError::ToolExecution(error.to_string()))?;
        self.scheduler = Some(std::sync::Arc::new(sail_agents::tools::ToolScheduler::new(
            self.registry.clone(),
            guardian,
            4,
        )));
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
                argument_bytes = args.to_string().len(),
                "SAIL Bridge executing tool"
            );

            if let (Some(bus), Some(scheduler)) = (&self.bus, &self.scheduler) {
                let observation = scheduler
                    .execute(
                        sail_agents::tools::ToolContext {
                            request_id: uuid::Uuid::parse_str(execution_id).unwrap_or(event.id),
                            correlation_id: event.correlation_id.unwrap_or(event.id),
                            actor: event.source,
                            cancel: tokio_util::sync::CancellationToken::new(),
                        },
                        tool,
                        args.clone(),
                        None,
                        true,
                    )
                    .await;
                let response = SailEvent::new(
                    self.id(),
                    EventPayload::ToolResult {
                        execution_id: execution_id.clone(),
                        success: observation.success,
                        output: serde_json::to_string(&observation)
                            .unwrap_or_else(|_| "observation serialization failed".into()),
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
pub mod external;
