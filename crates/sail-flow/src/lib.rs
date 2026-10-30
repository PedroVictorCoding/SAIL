use async_trait::async_trait;
use tracing::info;

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

pub struct FlowSubsystem {
    bus: Option<EventBus>,
}

impl FlowSubsystem {
    pub fn new() -> Self {
        Self { bus: None }
    }
}

impl Default for FlowSubsystem {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Subsystem for FlowSubsystem {
    fn id(&self) -> SubsystemId {
        SubsystemId::Flow
    }

    async fn init(&mut self, bus: &EventBus) -> Result<()> {
        self.bus = Some(bus.clone());
        info!("SAIL Flow initialized (workflow & trigger orchestration engine)");
        Ok(())
    }

    async fn start(&mut self) -> Result<()> {
        info!("SAIL Flow subsystem started");
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
        match &event.payload {
            EventPayload::ResearchTriggerDispatched { workflow_id, query } => {
                info!(
                    workflow_id = %workflow_id,
                    query = %query,
                    "SAIL Flow triggered workflow from Discovery"
                );

                if let Some(bus) = &self.bus {
                    // Dispatch task to Agents
                    let task = SailEvent::new(
                        self.id(),
                        EventPayload::TaskDispatched {
                            task_id: format!("task-{}", workflow_id),
                            workflow_id: Some(workflow_id.clone()),
                            goal: query.clone(),
                        },
                    )
                    .with_target(SubsystemId::Agents);

                    let _ = bus.publish(task);

                    // Flow also directly updates Canvas with workflow status
                    let canvas_update = SailEvent::new(
                        self.id(),
                        EventPayload::CanvasRender {
                            canvas_id: format!("flow-canvas-{}", workflow_id),
                            artifact_type: "ui_card".to_string(),
                            content: format!("Workflow {workflow_id} active: {query}"),
                        },
                    )
                    .with_target(SubsystemId::Canvas);

                    let _ = bus.publish(canvas_update);
                }
            }
            EventPayload::TaskCompleted {
                task_id,
                success,
                output,
            } => {
                info!(
                    task_id = %task_id,
                    success = %success,
                    output = %output,
                    "SAIL Flow received task completion event from Agents"
                );
            }
            _ => {}
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        info!("SAIL Flow subsystem shutting down");
        Ok(())
    }
}

/// Execute one previously authorized local operation, without invoking external connectors.
pub fn execute_voice_operation<T>(authorized_tool: &str, operation: impl FnOnce(&str) -> T) -> T {
    tracing::info!(
        tool = authorized_tool,
        "Executing authorized voice workflow operation"
    );
    operation(authorized_tool)
}
