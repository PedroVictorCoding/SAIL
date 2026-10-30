use sail_common::{EventBus, EventPayload, SailEvent, SubsystemId};

#[tokio::test]
async fn test_event_bus_publish_and_filter() {
    let bus = EventBus::new();
    let mut rx_mind = bus.subscribe_filtered(SubsystemId::Mind);
    let mut rx_dialogue = bus.subscribe_filtered(SubsystemId::Dialogue);

    // Publish an event specifically targeting Mind
    let event = SailEvent::new(
        SubsystemId::Agents,
        EventPayload::InferenceRequested {
            request_id: "req-1".to_string(),
            prompt: "Hello from Agent".to_string(),
            model_preference: Some("gemma4".to_string()),
        },
    )
    .with_target(SubsystemId::Mind);

    let sent = bus.publish(event).expect("Failed to publish");
    assert!(sent > 0);

    // Mind should receive it
    let received = rx_mind.recv().await.expect("Mind failed to receive");
    assert_eq!(received.source, SubsystemId::Agents);
    assert_eq!(received.target, Some(SubsystemId::Mind));

    // Publish a broadcast event (target = None)
    let broadcast_event = SailEvent::new(
        SubsystemId::Core,
        EventPayload::SystemReady {
            subsystem: SubsystemId::Core,
        },
    );
    bus.publish(broadcast_event).expect("Failed to broadcast");

    let dialogue_received = rx_dialogue.recv().await.expect("Dialogue should receive broadcast");
    assert_eq!(dialogue_received.source, SubsystemId::Core);
}
