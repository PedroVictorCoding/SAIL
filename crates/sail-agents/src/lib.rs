use async_trait::async_trait;
use tracing::info;

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};

pub struct AgentsSubsystem {
    bus: Option<EventBus>,
}

impl AgentsSubsystem {
    pub fn new() -> Self {
        Self { bus: None }
    }
}

impl Default for AgentsSubsystem {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Subsystem for AgentsSubsystem {
    fn id(&self) -> SubsystemId {
        SubsystemId::Agents
    }

    async fn init(&mut self, bus: &EventBus) -> Result<()> {
        self.bus = Some(bus.clone());
        info!("SAIL Agents initialized (goal decomposition, tool orchestration & reasoning)");
        Ok(())
    }

    async fn start(&mut self) -> Result<()> {
        info!("SAIL Agents subsystem started");
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
        let bus = match &self.bus {
            Some(b) => b,
            None => return Ok(()),
        };

        match &event.payload {
            // Dialogue input turn -> Request inference from Mind
            EventPayload::DialogueInput {
                session_id,
                speaker,
                content,
                is_final,
            } => {
                info!(
                    session_id = %session_id,
                    speaker = %speaker,
                    content = %content,
                    is_final = %is_final,
                    "SAIL Agents processing user dialogue turn"
                );

                // Request inference from Mind
                let mind_request = SailEvent::new(
                    self.id(),
                    EventPayload::InferenceRequested {
                        request_id: format!("req-{session_id}"),
                        prompt: content.clone(),
                        model_preference: Some("gemma4".to_string()),
                    },
                )
                .with_target(SubsystemId::Mind)
                .with_correlation_id(event.id);

                let _ = bus.publish(mind_request);
            }

            // Task dispatched from Flow -> Coordinate execution
            EventPayload::TaskDispatched {
                task_id,
                workflow_id,
                goal,
            } => {
                info!(
                    task_id = %task_id,
                    workflow_id = ?workflow_id,
                    goal = %goal,
                    "SAIL Agents executing proactive task"
                );

                // Request tool execution from Bridge
                let bridge_tool = SailEvent::new(
                    self.id(),
                    EventPayload::ToolExecute {
                        execution_id: format!("exec-{task_id}"),
                        tool: "web_search".to_string(),
                        args: serde_json::json!({ "query": goal }),
                    },
                )
                .with_target(SubsystemId::Bridge)
                .with_correlation_id(event.id);

                let _ = bus.publish(bridge_tool);
            }

            // Inference response received from Mind
            EventPayload::InferenceCompleted {
                request_id,
                content,
                tokens_used,
            } => {
                info!(
                    request_id = %request_id,
                    tokens = %tokens_used,
                    "SAIL Agents received inference from Mind; streaming to Dialogue & Canvas"
                );

                // Stream response to Dialogue
                let stream_event = SailEvent::new(
                    self.id(),
                    EventPayload::DialogueResponseDelta {
                        session_id: request_id.clone(),
                        delta: content.clone(),
                        is_finished: true,
                    },
                )
                .with_target(SubsystemId::Dialogue);
                let _ = bus.publish(stream_event);

                // Render visual canvas explanation
                let canvas_event = SailEvent::new(
                    self.id(),
                    EventPayload::CanvasRender {
                        canvas_id: format!("canvas-{request_id}"),
                        artifact_type: "markdown".to_string(),
                        content: format!("### Agent Response\n\n{content}"),
                    },
                )
                .with_target(SubsystemId::Canvas);
                let _ = bus.publish(canvas_event);

                // Commit knowledge to Arena
                let arena_event = SailEvent::new(
                    self.id(),
                    EventPayload::MemoryRecordStored {
                        record_id: format!("mem-{request_id}"),
                        collection: "interactions".to_string(),
                        summary: format!("Turn {request_id}: {content}"),
                    },
                )
                .with_target(SubsystemId::Arena);
                let _ = bus.publish(arena_event);
            }

            EventPayload::InferenceFailed { request_id, error } => {
                let _ = bus.publish(
                    SailEvent::new(
                        self.id(),
                        EventPayload::DialogueResponseDelta {
                            session_id: request_id.clone(),
                            delta: format!("Local inference unavailable: {error}"),
                            is_finished: true,
                        },
                    )
                    .with_target(SubsystemId::Dialogue),
                );
            }
            // Tool result received from Bridge
            EventPayload::ToolResult {
                execution_id,
                success,
                output,
            } => {
                info!(
                    execution_id = %execution_id,
                    success = %success,
                    "SAIL Agents received tool result from Bridge; notifying Flow & Arena"
                );

                // Report task completion back to Flow
                let task_id = execution_id.replace("exec-", "");
                let complete_event = SailEvent::new(
                    self.id(),
                    EventPayload::TaskCompleted {
                        task_id,
                        success: *success,
                        output: output.clone(),
                    },
                )
                .with_target(SubsystemId::Flow);
                let _ = bus.publish(complete_event);
            }

            _ => {}
        }

        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        info!("SAIL Agents subsystem shutting down");
        Ok(())
    }
}

/// Voice tools are read-only, explicitly named capabilities; arbitrary commands are denied.
pub fn authorize_voice_operation(tool: &str) -> sail_common::error::Result<&'static str> {
    match tool {
        "os:status" | "os_status" => Ok("os:status"),
        "proc:list" | "proc_list" => Ok("proc:list"),
        "fs:query" | "fs_query" => Ok("fs:query"),
        "vision:frame" | "vision_frame" => Ok("vision:frame"),
        _ => Err(sail_common::error::SailError::ToolExecution(format!(
            "Voice operation {tool} is not authorized"
        ))),
    }
}
