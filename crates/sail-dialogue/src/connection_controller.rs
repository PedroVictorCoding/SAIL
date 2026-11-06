//! Connection-level integration wrapper for the ConversationController.
//!
//! Wraps the existing connection state, providing turn and generation
//! tracking, unified stop handling, and stale event rejection.

use crate::conversation_controller::{ConversationController, Generation, StopReason};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Integration wrapper that attaches the conversation controller to an active connection.
#[derive(Clone)]
pub struct ConnectionController {
    controller: ConversationController,
    outbound_tx: mpsc::Sender<Value>,
}

impl ConnectionController {
    /// Create a new connection controller and attach the outbound channel.
    pub async fn new(outbound_tx: mpsc::Sender<Value>) -> Self {
        let controller = ConversationController::new();
        controller.set_outbound_channel(outbound_tx.clone()).await;
        Self {
            controller,
            outbound_tx,
        }
    }

    /// Start a new conversation turn, invalidating all previous responses.
    pub async fn start_turn(&self) -> (u64, u64, CancellationToken) {
        let (turn_id, generation, token) = self.controller.start_new_turn().await;
        tracing::info!(turn = turn_id.0, gen = generation.0, "Connection controller started new turn");
        (turn_id.0, generation.0, token)
    }

    /// Issue a unified stop command.
    pub async fn stop(&self, reason: StopReason) {
        let outcome = self.controller.stop(reason).await;
        tracing::info!(
            reason = ?reason,
            inference_cancelled = outcome.inference_cancelled,
            tools_stopped = outcome.tools_stopped.len(),
            already_completed = outcome.already_completed.len(),
            "Connection controller executed stop"
        );
    }

    /// Validate that an incoming event belongs to the current generation.
    pub async fn is_event_valid(&self, generation: u64) -> bool {
        self.controller.validate_event(Generation(generation)).await
    }

    /// Get the current active generation number.
    pub async fn current_generation(&self) -> u64 {
        self.controller.current_generation().await.0
    }

    /// Get the current active turn ID.
    pub async fn current_turn(&self) -> u64 {
        self.controller.current_turn().await.0
    }

    /// Mark the connection as reconnecting.
    pub async fn handle_reconnect(&self) {
        self.controller.set_session_reconnecting().await;
        let metadata = self.controller.recovery_metadata().await;
        let _ = self
            .outbound_tx
            .send(serde_json::json!({
                "type": "session_reconnected",
                "channel": "dialogue:controller",
                "latest_turn": metadata.latest_turn.0,
                "latest_generation": metadata.latest_generation.0,
                "active_tools": metadata.active_tools,
            }))
            .await;
        self.controller.set_session_connected().await;
    }

    /// Check if this speech chunk should be played, returning false if stale.
    pub async fn should_play_audio(&self, chunk_generation: u64) -> bool {
        let current = self.current_generation().await;
        if chunk_generation != current {
            tracing::debug!(
                chunk_gen = chunk_generation,
                current_gen = current,
                "Rejected stale audio chunk"
            );
            return false;
        }
        true
    }

    /// Emit generation metadata with every outbound event.
    pub async fn wrap_event(&self, mut event: Value) -> Value {
        let generation = self.current_generation().await;
        event["generation"] = serde_json::json!(generation);
        event
    }
}
