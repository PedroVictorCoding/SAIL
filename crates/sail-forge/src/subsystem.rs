use async_trait::async_trait;
use std::sync::Arc;
use tracing::{error, info};

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

use crate::nightly::ForgeNightlyRunner;
use crate::orchestrator::ForgeOrchestrator;

/// Forge Autonomous Coding Subsystem integration for SAIL.
pub struct ForgeSubsystem {
    bus: Option<EventBus>,
    orchestrator: Arc<ForgeOrchestrator>,
    nightly: Option<Arc<ForgeNightlyRunner>>,
}

impl ForgeSubsystem {
    pub fn new(orchestrator: Arc<ForgeOrchestrator>) -> Self {
        crate::orchestrator::register_global_orchestrator(orchestrator.clone());
        Self {
            bus: None,
            orchestrator,
            nightly: None,
        }
    }

    /// Attach the unattended overnight runner; its scheduler starts with the subsystem.
    pub fn with_nightly_runner(mut self, runner: Arc<ForgeNightlyRunner>) -> Self {
        self.nightly = Some(runner);
        self
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
        self.orchestrator.set_event_bus(bus.clone());
        info!("SAIL Forge Subsystem initialized with EventBus");
        Ok(())
    }

    async fn start(&mut self) -> Result<()> {
        info!("SAIL Forge Subsystem starting; recovering durable task checkpoints...");
        if let Ok(recovered_ids) = self.orchestrator.recover_from_checkpoints() {
            if !recovered_ids.is_empty() {
                info!(
                    "SAIL Forge recovered {} tasks across daemon restart: {:?}",
                    recovered_ids.len(),
                    recovered_ids
                );
                for job_id in recovered_ids {
                    let orch = self.orchestrator.clone();
                    tokio::spawn(async move {
                        info!("Resuming recovered task {} in background worker", job_id);
                        if let Err(e) = orch.run_job_lifecycle(&job_id, None).await {
                            error!("Error running recovered job {}: {}", job_id, e);
                        }
                    });
                }
            }
        }
        if let Some(runner) = &self.nightly {
            runner.clone().spawn_scheduler();
        }
        Ok(())
    }

    async fn handle_event(&self, event: &SailEvent) -> Result<()> {
        match &event.payload {
            EventPayload::ForgeIssueEnqueued {
                job_id,
                repo: _,
                issue_number: _,
                priority: _,
            } => {
                if self.orchestrator.is_managed(job_id) {
                    return Ok(());
                }
                info!(
                    "Forge received ForgeIssueEnqueued event for job {}; dispatching worker",
                    job_id
                );
                let orch = self.orchestrator.clone();
                let jid = job_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = orch.run_job_lifecycle(&jid, None).await {
                        error!("Error executing enqueued job {}: {}", jid, e);
                    }
                });
            }
            EventPayload::SystemShutdown { reason } => {
                info!(
                    "Forge Subsystem received SystemShutdown ({}): active state persisted to disk",
                    reason
                );
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
