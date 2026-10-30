use tokio::sync::broadcast;
use tracing::{debug, trace};

use crate::error::{Result, SailError};
use crate::types::{SailEvent, SubsystemId};

const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<SailEvent>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CHANNEL_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    pub fn publish(&self, event: SailEvent) -> Result<usize> {
        trace!(
            source = %event.source,
            target = ?event.target,
            id = %event.id,
            "Publishing event"
        );
        self.sender
            .send(event)
            .map_err(|e| SailError::Channel(format!("No active subscribers for event: {e}")))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SailEvent> {
        self.sender.subscribe()
    }

    pub fn subscribe_filtered(
        &self,
        subsystem: SubsystemId,
    ) -> FilteredReceiver {
        FilteredReceiver {
            receiver: self.sender.subscribe(),
            subsystem,
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

pub struct FilteredReceiver {
    receiver: broadcast::Receiver<SailEvent>,
    subsystem: SubsystemId,
}

impl FilteredReceiver {
    pub async fn recv(&mut self) -> Result<SailEvent> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => {
                    // Deliver if it's a broadcast (target is None) or explicitly targeted to this subsystem
                    if event.target.is_none() || event.target == Some(self.subsystem) {
                        debug!(
                            subsystem = %self.subsystem,
                            event_id = %event.id,
                            "Filtered event accepted"
                        );
                        return Ok(event);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(subsystem = %self.subsystem, missed, "Subscriber lagged behind bus");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(SailError::Channel("Bus channel closed".into()));
                }
            }
        }
    }
}
