mod logging;
mod status;
use std::sync::Arc;
use tokio::signal;
use tracing::{error, info};

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
use sail_forge::{
    ForgeNightlyRunner, ForgeOrchestrator, ForgeSubsystem, GhCliClient, LocalModelReviewer,
    NightlyConfig, OrchestratorConfig, ReviewBroker, ReviewPolicyConfig, TaskCheckpointStore,
};
use sail_guardian::GuardianSubsystem;
use sail_mind::MindSubsystem;
use sail_ui::UiSubsystem;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    logging::init()?;
    // Persistent data lives under one root (`/srv/sail-data` when mounted). Move data left at
    // older defaults before any store opens.
    let data_root = sail_common::storage::log_data_root();
    sail_forge::nightly::migrate_legacy_forge_data();
    let guardian_dir = sail_common::storage::data_dir("guardian");
    for (old, name) in [
        (".local/guardian_learned_rules.json", "learned_rules.json"),
        (
            ".local/guardian_safety_findings.json",
            "safety_findings.json",
        ),
    ] {
        if let Err(error) = sail_common::storage::migrate_legacy(
            std::path::Path::new(old),
            &guardian_dir.join(name),
        ) {
            tracing::error!(%old, %error, "Guardian data migration failed; the old file is untouched");
        }
    }
    tracing::debug!(root = %data_root.path.display(), "data migration checked");

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
    // One resident model shared by every subsystem (`sail_common::models`).
    let model_path = sail_common::models::primary_model_path()
        .to_string_lossy()
        .into_owned();
    let mut mind = MindSubsystem::try_new(model_path)?;
    let model_service = mind.model_service()?;
    let mut arena = ArenaSubsystem::open_default();
    let integrity = arena.store().check_integrity()?;
    if !integrity.is_empty() {
        return Err(std::io::Error::other(format!(
            "Arena integrity check failed: {}",
            integrity.join("; ")
        ))
        .into());
    }
    let mut bridge = BridgeSubsystem::new();
    let mut canvas = CanvasSubsystem::new();
    let mut dialogue = DialogueSubsystem::new().with_model_services(mind.voice_services());
    let mut discovery = DiscoverySubsystem::new();
    let mut flow = FlowSubsystem::new();
    let mut agents = AgentsSubsystem::new();
    let mut guardian = GuardianSubsystem::with_data_dir(&guardian_dir);
    let mut ui = UiSubsystem::new();

    let review_broker = Arc::new(
        ReviewBroker::new(ReviewPolicyConfig::default()).with_credentials(guardian.credentials()),
    );
    // Durable record of accepted background work; recovered when Forge starts.
    let continuity = Arc::new(sail_continuity::Continuity::open_default());
    // Pulse reminders are server-owned: the scheduler runs even when no browser is connected.
    let pulse = Arc::new(sail_pulse::Pulse::open_default());
    let _ = sail_pulse::install_global(pulse.clone());
    let _pulse_scheduler = pulse.start_scheduler();
    dialogue = dialogue.with_pulse(pulse.clone());
    let checkpoint_store = Arc::new(TaskCheckpointStore::default_store());
    let mut forge = ForgeOrchestrator::new(
        OrchestratorConfig::default(),
        checkpoint_store,
        review_broker,
        guardian.sandbox(),
    )
    .with_self_reviewer(Arc::new(LocalModelReviewer::new()))
    .with_continuity(continuity.clone());
    if let Some(memory) = sail_forge::failure_memory::FailureMemory::from_env() {
        forge = forge.with_failure_memory(Arc::new(memory));
    }
    let orchestrator = Arc::new(forge);
    if !std::env::var("SAIL_MEMORY_BACKGROUND").is_ok_and(|v| v == "0" || v == "false") {
        let admission_mind = model_service.clone();
        let admission_forge = orchestrator.clone();
        let idle = Arc::new(move || {
            !admission_mind.has_work()
                && !admission_forge.list_all_jobs().iter().any(|j| {
                    matches!(
                        j.state,
                        sail_forge::contracts::ForgeTaskState::Planning
                            | sail_forge::contracts::ForgeTaskState::Coding
                            | sail_forge::contracts::ForgeTaskState::Testing
                            | sail_forge::contracts::ForgeTaskState::ReviewPending
                    )
                })
        });
        let days = std::env::var("SAIL_MEMORY_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|n| (1..=3650).contains(n))
            .unwrap_or(30);
        let maintenance_grant =
            sail_guardian::memory::memory_grant(&sail_protocol::memory::MemoryPurpose::Maintenance)
                .expect("maintenance memory grant is static");
        let mut maintenance = sail_flow::memory::MemoryMaintenance::new(
            arena.store().clone(),
            sail_arena::knowledge::Access::from_grant(&maintenance_grant),
            maintenance_grant.scopes.clone(),
            idle,
            days,
        );
        // Opt-in until extraction quality is measured on the target host's local model.
        if std::env::var("SAIL_MEMORY_MODEL_PROPOSALS").is_ok_and(|v| v == "1" || v == "true") {
            info!("Memory model proposals enabled; the validator gates every change");
            maintenance = maintenance.with_proposer(memory_proposer(model_service.clone()));
        }
        flow = flow.with_memory(maintenance);
    }
    // Agent task plans share the Continuity store and run Dialogue's tools through Guardian.
    let plans = match sail_dialogue::plans::Plans::with_mind(continuity, model_service.clone()) {
        Ok(plans) => {
            dialogue = dialogue.with_plans(plans.clone());
            Some(plans)
        }
        Err(error) => {
            error!(%error, "Agent task planning unavailable");
            None
        }
    };
    let mut forge = ForgeSubsystem::new(orchestrator.clone());
    let nightly_config = NightlyConfig::from_env();
    if nightly_config.enabled {
        info!(
            "Forge nightly runner enabled for {} (publish: {})",
            nightly_config.repo_slug, nightly_config.publish
        );
        forge = forge.with_nightly_runner(Arc::new(ForgeNightlyRunner::new(
            orchestrator,
            Arc::new(GhCliClient::new()),
            nightly_config,
        )));
    }

    // Call init on each subsystem
    mind.init(&bus).await?;
    arena.init(&bus).await?;
    bridge.init(&bus).await?;
    canvas.init(&bus).await?;
    dialogue.init(&bus).await?;
    discovery.init(&bus).await?;
    flow.init(&bus).await?;
    agents.init(&bus).await?;
    guardian.init(&bus).await?;
    forge.init(&bus).await?;
    ui.init(&bus).await?;

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
    guardian.start().await?;
    forge.start().await?;
    ui.start().await?;
    // After Forge's start, whose recovery pass marks the previous boot's tasks interrupted.
    if let Some(plans) = &plans {
        match plans.resume() {
            Ok(0) => {}
            Ok(count) => info!(count, "Resumed agent task plans"),
            Err(error) => error!(%error, "Could not resume agent task plans"),
        }
    }

    // Wrap in Arc for event dispatchers
    let mind_arc = Arc::new(mind);
    let arena_arc = arena;
    let bridge_arc = Arc::new(bridge);
    let canvas_arc = Arc::new(canvas);
    let dialogue_arc = Arc::new(dialogue);
    let discovery_arc = Arc::new(discovery);
    let flow_arc = Arc::new(flow);
    let agents_arc = Arc::new(agents);
    let guardian_arc = Arc::new(guardian);
    let forge_arc = Arc::new(forge);
    let ui_arc = Arc::new(ui);

    // Spawn event dispatcher loops for each subsystem
    spawn_subsystem_worker(SubsystemId::Mind, mind_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Arena, arena_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Bridge, bridge_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Canvas, canvas_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Dialogue, dialogue_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Discovery, discovery_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Flow, flow_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Agents, agents_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Guardian, guardian_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Forge, forge_arc, &bus);
    spawn_subsystem_worker(SubsystemId::Ui, ui_arc, &bus);

    info!(
        "All 10 SAIL subsystems (including SAIL Guardian & SAIL Forge) bootstrapped and listening on the event bus"
    );

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

    // A second Ctrl+C always quits, even if a step below is stuck.
    tokio::spawn(async {
        if signal::ctrl_c().await.is_ok() {
            eprintln!("Second Ctrl+C: exiting without waiting for shutdown to finish.");
            std::process::exit(130);
        }
    });
    info!("Stopping models (press Ctrl+C again to force quit)...");
    // A model process wedged in the GPU driver can stop responding to signals; never let
    // it hold the terminal.
    match tokio::time::timeout(SHUTDOWN_GRACE, model_service.shutdown()).await {
        Ok(Ok(())) => info!("SAIL Core shutdown complete."),
        Ok(Err(error)) => error!(%error, "Model shutdown failed"),
        Err(_) => error!(
            "Model shutdown did not finish within {}s; exiting anyway. If the model server is stuck, run ./run.sh --stop",
            SHUTDOWN_GRACE.as_secs()
        ),
    }
    // Returning would drop the runtime, which waits for every running blocking task
    // (tool commands, memory consolidation); exit instead. Logging writes synchronously.
    std::process::exit(0)
}

/// How long Ctrl+C waits for the models to stop before SAIL exits anyway.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

fn spawn_subsystem_worker(
    subsystem_id: SubsystemId,
    subsystem: Arc<dyn Subsystem>,
    bus: &EventBus,
) {
    let mut rx = bus.subscribe_filtered(subsystem_id);
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            tracing::debug!(subsystem=%subsystem_id,event_id=%event.id,source=%event.source,target=?event.target,payload=%sail_common::diagnostics::log_json(&serde_json::to_value(&event.payload).unwrap_or_default(),4096),"Dispatching subsystem event");
            if let Err(err) = subsystem.handle_event(&event).await {
                error!(subsystem = %subsystem_id, error = %err, "Error processing event");
            }
        }
    });
}

/// Local-model proposer for background memory consolidation. Runs on Flow's blocking worker,
/// so it may block on the runtime; it is bounded by a timeout and never retried here.
fn memory_proposer(service: sail_mind::MindService) -> sail_flow::memory::Proposer {
    let handle = tokio::runtime::Handle::current();
    Arc::new(move |prompt: &str| {
        let mut request = sail_protocol::InferenceRequest::text(prompt);
        request.generation = sail_protocol::Generation {
            max_tokens: 768,
            temperature: 0.0,
        };
        let service = service.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let guard = cancel.clone();
        handle.block_on(async move {
            match tokio::time::timeout(
                std::time::Duration::from_secs(180),
                service.infer(request, cancel, None),
            )
            .await
            {
                Ok(Ok(response)) => response
                    .message
                    .content
                    .text()
                    .map(str::to_string)
                    .ok_or_else(|| "model reply had no text".to_string()),
                Ok(Err(error)) => Err(error.to_string()),
                Err(_) => {
                    guard.cancel();
                    Err("memory proposal timed out".to_string())
                }
            }
        })
    })
}
