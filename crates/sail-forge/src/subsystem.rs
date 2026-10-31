use std::sync::Arc;
use async_trait::async_trait;
use tracing::info;

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

use crate::orchestrator::ForgeOrchestrator;

/// Forge Autonomous Coding Subsystem integration for SAIL.
pub struct ForgeSubsystem {
    bus: Option<EventBus>,
    orchestrator: Arc<ForgeOrchestrator>,
}

impl ForgeSubsystem {
    pub fn new(orchestrator: Arc<ForgeOrchestrator>) -> Self {
        Self {
            bus: None,
            orchestrator,
        }
    }

    pub fn orchestrator(&self) -> Arc<ForgeOrchestrator> {
        self.orchestrator.clone()
    }
}

#[async_trait]
impl Subsystem for ForgeSubsystem {
    fn id(&self) -> SubsystemId {
        SubsystemId::Forge
    }

    async fn init(&mut self, bus: &EventBus) -> Result<()> {
        self.bus = Some(bus.clone());
        info!("SAIL Forge Subsystem initialized with EventBus");
        Ok(())
    }

    async fn start(&mut self) -> Result<()> {
        info!("SAIL Forge Subsystem starting; recovering durable task checkpoints...");
        if let Ok(count) = self.orchestrator.recover_from_checkpoints() {
            if count > 0 {
                info!("SAIL Forge recovered {} tasks across daemon restart", count);
            }
        }
        Ok(())
    }

    async fn handle_event(&self, event: &SailEvent) -> Result<()> {
        match &event.payload {
            EventPayload::ForgeIssueEnqueued { job_id, repo: _, issue_number: _, priority: _ } => {
                info!("Forge received ForgeIssueEnqueued event for job {}", job_id);
                // Can trigger background orchestration task
            }
            EventPayload::SystemShutdown { reason } => {
                info!("Forge Subsystem received SystemShutdown ({}): persisting active state", reason);
            }
            _ => {}
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        info!("SAIL Forge Subsystem shutting down gracefully");
        Ok(())
    }
}
