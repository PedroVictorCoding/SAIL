mod status;
use std::sync::Arc;
use tokio::signal;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sail_core=info,sail_common=info,sail_mind=info,sail_agents=info,sail_flow=info,sail_discovery=info,sail_arena=info,sail_bridge=info,sail_dialogue=info,sail_canvas=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    if std::env::args().nth(1).as_deref() == Some("mind-status") {
        let status = status::read().await?;
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    info!("==================================================");
    info!("Starting SAIL Core Assistant Runtime");
    info!("==================================================");

    // Initialize Event Bus
    let bus = EventBus::new();

    // Initialize all 8 Subsystems
    let model_path =
        std::env::var("SAIL_MODEL_PATH").unwrap_or("models/gemma4-v2-Q4_K_M.gguf".into());
    let mut mind = MindSubsystem::try_new(model_path)?;
    let model_service = mind.model_service()?;
    let mut arena = ArenaSubsystem::new();
    let mut bridge = BridgeSubsystem::new();
    let mut canvas = CanvasSubsystem::new();
    let mut dialogue = DialogueSubsystem::new().with_model_services(mind.voice_services());
    let mut discovery = DiscoverySubsystem::new();
    let mut flow = FlowSubsystem::new();
    let mut agents = AgentsSubsystem::new();

    // Call init on each subsystem
    mind.init(&bus).await?;
    arena.init(&bus).await?;
    bridge.init(&bus).await?;
    canvas.init(&bus).await?;
    dialogue.init(&bus).await?;
    discovery.init(&bus).await?;
    flow.init(&bus).await?;
    agents.init(&bus).await?;

    arena.start().await?;
    let arena = Arc::new(arena);
    dialogue = dialogue.with_arena(arena.clone());

    // Start each subsystem
    mind.start().await?;
    // Arena has been initialized and is shared with voice sessions.
    bridge.start().await?;
    canvas.start().await?;
    dialogue.start().await?;
    discovery.start().await?;
    flow.start().await?;
    agents.start().await?;

    // Wrap in Arc for event dispatchers
    let mind_arc = Arc::new(mind);
    let arena_arc = arena;
    let bridge_arc = Arc::new(bridge);
    let canvas_arc = Arc::new(canvas);
    let dialogue_arc = Arc::new(dialogue);
    let discovery_arc = Arc::new(discovery);
    let flow_arc = Arc::new(flow);
    let agents_arc = Arc::new(agents);

    // Spawn event dispatcher loops for each subsystem
    spawn_subsystem_worker(SubsystemId::Mind, mind_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Arena, arena_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Bridge, bridge_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Canvas, canvas_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Dialogue, dialogue_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Discovery, discovery_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Flow, flow_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Agents, agents_arc, &bus);

    info!("All 8 SAIL subsystems bootstrapped and listening on the event bus");

    // Publish system ready event
    let ready_event = SailEvent::new(
        SubsystemId::Core,
        EventPayload::SystemReady {
            subsystem: SubsystemId::Core,
        },
    );
    bus.publish(ready_event)?;

    info!("SAIL Core is operational. Waiting for Ctrl+C to terminate...");

    // Wait for shutdown signal
    match signal::ctrl_c().await {
        Ok(()) => {
            info!("Received shutdown signal. Stopping SAIL Core...");
        }
        Err(err) => {
            error!("Error listening for shutdown signal: {err}");
        }
    }

    let shutdown_event = SailEvent::new(
        SubsystemId::Core,
        EventPayload::SystemShutdown {
            reason: "User requested shutdown".to_string(),
        },
    );
    let _ = bus.publish(shutdown_event);

    model_service.shutdown().await?;
    info!("SAIL Core shutdown complete.");
    Ok(())
}

fn spawn_subsystem_worker(
    subsystem_id: SubsystemId,
    subsystem: Arc<dyn Subsystem>,
    bus: &EventBus,
) {
    let mut rx = bus.subscribe_filtered(subsystem_id);
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            if let Err(err) = subsystem.handle_event(&event).await {
                error!(subsystem = %subsystem_id, error = %err, "Error processing event");
            }
        }
    });
}
