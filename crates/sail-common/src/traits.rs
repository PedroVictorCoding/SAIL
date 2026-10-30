use async_trait::async_trait;

use crate::bus::EventBus;
use crate::error::Result;
use crate::types::{SailEvent, SubsystemId};

#[async_trait]
pub trait Subsystem: Send + Sync {
    /// Return the unique subsystem identifier.
    fn id(&self) -> SubsystemId;

    /// Initialize the subsystem with access to the central event bus.
    async fn init(&mut self, bus: &EventBus) -> Result<()>;

    /// Start long-running subsystem tasks (e.g. background listener loops).
    async fn start(&mut self) -> Result<()>;

    /// Handle an incoming domain event.
    async fn handle_event(&self, event: &SailEvent) -> Result<()>;

    /// Graceful shutdown hook.
    async fn shutdown(&mut self) -> Result<()>;
}
