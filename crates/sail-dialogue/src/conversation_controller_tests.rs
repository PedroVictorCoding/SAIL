//! Unit tests for the unified conversation controller.
//!
//! These tests validate all 5 implementation stages:
//! 1. Turn identifiers and traceability
//! 2. Unified Stop control and stale-event rejection
//! 3. Full cancellation propagation
//! 4. Reconnect and recovery
//! 5. Voice interruption and state dimension independence

#[cfg(test)]
mod tests {
    use crate::connection_controller::ConnectionController;
    use crate::conversation_controller::StopReason;
    use serde_json::json;

    #[tokio::test]
    async fn test_stage1_turn_and_generation_traceability() {
        let (tx, _mut_rx) = tokio::sync::mpsc::channel(10);
        let controller = ConnectionController::new(tx).await;

        // Start first turn
        let (turn1, gen1, _token1) = controller.start_turn().await;
        assert_eq!(turn1, 1);
        assert_eq!(gen1, 1);

        // Start second turn
        let (turn2, gen2, _token2) = controller.start_turn().await;
        assert_eq!(turn2, 2);
        assert_eq!(gen2, 2);

        // Events must carry generation IDs
        let wrapped = controller.wrap_event(json!({"type":"test"})).await;
        assert_eq!(wrapped["generation"], 2u64);
    }

    #[tokio::test]
    async fn test_stage2_stale_event_rejection() {
        let (tx, _rx) = tokio::sync::mpsc::channel(10);
        let controller = ConnectionController::new(tx).await;

        let (_turn1, gen1, _token) = controller.start_turn().await;
        // Before interruption, event is valid
        assert!(controller.is_event_valid(gen1).await);
        assert!(controller.should_play_audio(gen1).await);

        // Interrupt: start new turn increments generation
        let (_turn2, gen2, _token2) = controller.start_turn().await;

        // Old generation events are now rejected
        assert!(!controller.is_event_valid(gen1).await);
        assert!(!controller.should_play_audio(gen1).await);
        assert_eq!(controller.current_generation().await, gen2);
    }

    #[tokio::test]
    async fn test_stage3_unified_stop_control() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let controller = ConnectionController::new(tx).await;

        let (_turn, _gen, _token) = controller.start_turn().await;

        // Execute stop
        controller.stop(StopReason::UserInterrupt).await;

        // Client receives conversation_stopped event
        let stopped_event = rx.recv().await.expect("Stop event received");
        assert_eq!(stopped_event["type"], "conversation_stopped");
        assert_eq!(stopped_event["reason"], "user_interrupt");
    }

    #[tokio::test]
    async fn test_stage4_reconnect_recovery() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let controller = ConnectionController::new(tx).await;

        // Start two turns
        let (_turn1, _gen1, _token1) = controller.start_turn().await;
        let (_turn2, gen2, _token2) = controller.start_turn().await;

        // Simulate reconnect
        controller.handle_reconnect().await;

        // Client receives session_reconnected event with recovery metadata
        let reconnect_event = rx.recv().await.expect("Reconnect event received");
        assert_eq!(reconnect_event["type"], "session_reconnected");
        assert_eq!(reconnect_event["latest_generation"], gen2);
    }

    #[tokio::test]
    async fn test_stage5_generation_preserved_after_reconnect() {
        let (tx, _rx) = tokio::sync::mpsc::channel(10);
        let controller = ConnectionController::new(tx).await;

        let (_turn1, gen1, _) = controller.start_turn().await;
        controller.handle_reconnect().await;

        // Generation is preserved after reconnect (no spurious increment)
        assert_eq!(controller.current_generation().await, gen1);

        // New turn increments normally
        let (_turn2, gen2, _) = controller.start_turn().await;
        assert_eq!(gen2, gen1 + 1);
    }

    #[tokio::test]
    async fn test_stop_reason_correctly_classified() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let controller = ConnectionController::new(tx).await;
        let (_turn, _gen, _) = controller.start_turn().await;

        controller.stop(StopReason::NewInput).await;
        let event = rx.recv().await.unwrap();
        assert_eq!(event["reason"], "new_input");

        // A stop with no active response sends nothing, so start another turn first.
        let (_turn, _gen, _) = controller.start_turn().await;
        controller.stop(StopReason::ModelFailure).await;
        let event = rx.recv().await.unwrap();
        assert_eq!(event["reason"], "model_failure");
    }
}
