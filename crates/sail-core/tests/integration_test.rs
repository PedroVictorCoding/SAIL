use std::sync::Arc;
use tokio::time::{Duration, sleep};

use sail_agents::AgentsSubsystem;
use sail_arena::ArenaSubsystem;
use sail_bridge::BridgeSubsystem;
use sail_canvas::CanvasSubsystem;
use sail_common::{
    bus::EventBus,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};
use sail_dialogue::DialogueSubsystem;
use sail_discovery::DiscoverySubsystem;
use sail_flow::FlowSubsystem;
use sail_mind::MindSubsystem;

fn spawn_subsystem_worker(
    subsystem_id: SubsystemId,
    subsystem: Arc<dyn Subsystem>,
    bus: &EventBus,
) {
    let mut rx = bus.subscribe_filtered(subsystem_id);
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let _ = subsystem.handle_event(&event).await;
        }
    });
}

async fn next_business_event(
    rx: &mut sail_common::FilteredReceiver,
) -> sail_common::Result<SailEvent> {
    loop {
        let event = rx.recv().await?;
        if !matches!(event.payload, EventPayload::SystemReady { .. }) {
            return Ok(event);
        }
    }
}

#[tokio::test]
async fn test_full_sail_subsystem_ecosystem_loop() {
    let _guardian = sail_guardian::GuardianSubsystem::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mock = tokio::spawn(async move {
        let app=axum::Router::new()
            .route("/health",axum::routing::get(||async{axum::Json(serde_json::json!({"status":"ok"}))}))
            .route("/props",axum::routing::get(||async{axum::Json(serde_json::json!({"modalities":{"tools":true,"vision":false},"n_ctx":8192}))}))
            .route("/apply-template",axum::routing::post(||async{axum::Json(serde_json::json!({"prompt":"fixture"}))}))
            .route("/tokenize",axum::routing::post(||async{axum::Json(serde_json::json!({"tokens":[1,2]}))}))
            .route("/v1/chat/completions",axum::routing::post(|axum::Json(value):axum::Json<serde_json::Value>|async move {
                let content=format!("Mock inference: {}",value["messages"].as_array().unwrap().iter().find(|m|m["role"]=="user").unwrap()["content"].as_str().unwrap());
                let event=serde_json::json!({"choices":[{"delta":{"content":content},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":8}});
                ([("content-type","text/event-stream")],format!("data: {event}\n\ndata: [DONE]\n\n"))
            }));
        axum::serve(listener, app).await.unwrap();
    });
    let bus = EventBus::new();

    let mut mind =
        MindSubsystem::new("missing-test-model.gguf").with_gemma_config(sail_mind::GemmaConfig {
            model_path: "missing-test-model.gguf".into(),
            server_url: format!("http://{addr}/v1/chat/completions"),
            ..Default::default()
        });
    let mut arena = ArenaSubsystem::new();
    let mut bridge = BridgeSubsystem::new();
    let mut canvas = CanvasSubsystem::new();
    let mut dialogue = DialogueSubsystem::new().with_addr("127.0.0.1:0".parse().unwrap());
    let mut discovery = DiscoverySubsystem::new();
    let mut flow = FlowSubsystem::new();
    let mut agents = AgentsSubsystem::new();

    // Init
    mind.init(&bus).await.unwrap();
    arena.init(&bus).await.unwrap();
    bridge.init(&bus).await.unwrap();
    canvas.init(&bus).await.unwrap();
    dialogue.init(&bus).await.unwrap();
    discovery.init(&bus).await.unwrap();
    flow.init(&bus).await.unwrap();
    agents.init(&bus).await.unwrap();

    // Start
    mind.start().await.unwrap();
    arena.start().await.unwrap();
    bridge.start().await.unwrap();
    canvas.start().await.unwrap();
    dialogue.start().await.unwrap();
    discovery.start().await.unwrap();
    flow.start().await.unwrap();
    agents.start().await.unwrap();

    let arena_arc = Arc::new(arena);
    let mind_arc = Arc::new(mind);
    let bridge_arc = Arc::new(bridge);
    let canvas_arc = Arc::new(canvas);
    let dialogue_arc = Arc::new(dialogue);
    let discovery_arc = Arc::new(discovery);
    let flow_arc = Arc::new(flow);
    let agents_arc = Arc::new(agents);

    // Spawn workers
    spawn_subsystem_worker(SubsystemId::Mind, mind_arc.clone(), &bus);
    spawn_subsystem_worker(SubsystemId::Arena, arena_arc.clone(), &bus);
    spawn_subsystem_worker(SubsystemId::Bridge, bridge_arc.clone(), &bus);
    spawn_subsystem_worker(SubsystemId::Canvas, canvas_arc.clone(), &bus);
    spawn_subsystem_worker(SubsystemId::Dialogue, dialogue_arc.clone(), &bus);
    spawn_subsystem_worker(SubsystemId::Discovery, discovery_arc.clone(), &bus);
    spawn_subsystem_worker(SubsystemId::Flow, flow_arc.clone(), &bus);
    spawn_subsystem_worker(SubsystemId::Agents, agents_arc.clone(), &bus);

    let mut rx_dialogue = bus.subscribe_filtered(SubsystemId::Dialogue);
    let mut rx_canvas = bus.subscribe_filtered(SubsystemId::Canvas);
    let mut rx_flow = bus.subscribe_filtered(SubsystemId::Flow);

    // 1. Simulate Dialogue turn from User
    let user_utterance = SailEvent::new(
        SubsystemId::Dialogue,
        EventPayload::DialogueInput {
            session_id: "test-turn-1".to_string(),
            speaker: "user".to_string(),
            content: "Explain the architecture".to_string(),
            is_final: true,
        },
    )
    .with_target(SubsystemId::Agents);

    bus.publish(user_utterance).unwrap();

    // Verify Dialogue receives the response delta from Agents
    let response_delta = tokio::time::timeout(
        Duration::from_secs(2),
        next_business_event(&mut rx_dialogue),
    )
    .await
    .expect("Timed out waiting for Dialogue response delta")
    .expect("Failed to receive event");

    match response_delta.payload {
        EventPayload::DialogueResponseDelta {
            session_id,
            delta,
            is_finished,
        } => {
            assert_eq!(session_id, "req-test-turn-1");
            assert!(delta.contains("Explain the architecture"));
            assert!(is_finished);
        }
        other => panic!("Unexpected event received by Dialogue: {other:?}"),
    }

    // Verify Canvas received the visual render artifact from Agents
    let canvas_render =
        tokio::time::timeout(Duration::from_secs(2), next_business_event(&mut rx_canvas))
            .await
            .expect("Timed out waiting for Canvas render")
            .expect("Failed to receive event");

    match canvas_render.payload {
        EventPayload::CanvasRender {
            canvas_id,
            artifact_type,
            content,
        } => {
            assert_eq!(canvas_id, "canvas-req-test-turn-1");
            assert_eq!(artifact_type, "markdown");
            assert!(content.contains("Agent Response"));
        }
        other => panic!("Unexpected event received by Canvas: {other:?}"),
    }

    // 2. Simulate Proactive Discovery Loop
    // Arena sends knowledge gap to Discovery
    let gap = SailEvent::new(
        SubsystemId::Arena,
        EventPayload::KnowledgeGapDetected {
            topic: "local_rag_benchmarks".to_string(),
            priority: 0.9,
            hypothesis: "Sparse-dense hybrid search reduces retrieval hallucination by 40%"
                .to_string(),
        },
    )
    .with_target(SubsystemId::Discovery);

    bus.publish(gap).unwrap();

    // Flow should receive ResearchTriggerDispatched, then task execution occurs
    let flow_event =
        tokio::time::timeout(Duration::from_secs(2), next_business_event(&mut rx_flow))
            .await
            .expect("Timed out waiting for Flow trigger")
            .expect("Failed to receive event");

    match flow_event.payload {
        EventPayload::ResearchTriggerDispatched { workflow_id, query } => {
            assert_eq!(workflow_id, "research-local_rag_benchmarks");
            assert!(query.contains("Sparse-dense hybrid search"));
        }
        other => panic!("Unexpected event received by Flow: {other:?}"),
    }

    // Allow agents to execute bridge tool and report task completion to flow
    sleep(Duration::from_millis(150)).await;

    // Check Arena memory store has recorded the interaction
    assert!(
        !arena_arc.is_empty(),
        "Arena should have stored memory records"
    );
    mock.abort();
}
