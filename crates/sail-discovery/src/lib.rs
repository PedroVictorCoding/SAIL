//! SAIL Discovery: noticing what deserves attention.
//!
//! - [`detect`]: deterministic detectors that turn observations into attention signals;
//! - [`agenda`]: the durable agenda of topics, investigations, commitments and open questions;
//! - [`initiative`]: the loop that ties them to Pulse's attention inbox.
//!
//! See `docs/SAIL_PROACTIVE.md`.
pub mod agenda;
pub mod curiosity;
pub mod detect;
pub mod github;
pub mod initiative;

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

static GLOBAL: std::sync::OnceLock<std::sync::Arc<initiative::Initiative>> =
    std::sync::OnceLock::new();

/// Make one initiative engine available to Dialogue. The first installation wins.
pub fn install_global(initiative: std::sync::Arc<initiative::Initiative>) -> bool {
    GLOBAL.set(initiative).is_ok()
}

pub fn global() -> Option<std::sync::Arc<initiative::Initiative>> {
    GLOBAL.get().cloned()
}

/// `$SAIL_DATA/discovery/agenda.db`.
pub fn default_agenda_path() -> std::path::PathBuf {
    sail_common::storage::data_path("discovery").join("agenda.db")
}
