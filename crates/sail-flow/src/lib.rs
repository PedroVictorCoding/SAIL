pub mod hermes;
pub mod market;
pub mod memory;
pub mod proposals;
pub mod skills;
pub mod workflows;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};

use sail_common::{
    bus::EventBus,
    error::Result,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};
use sail_guardian::safety_review::SafetyReviewRunSummary;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyReviewScheduleConfig {
    pub enabled: bool,
    pub interval_seconds: u64, // Default: 86400 (daily)
}

impl Default for SafetyReviewScheduleConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_seconds: 86400,
        }
    }
}

pub struct FlowSubsystem {
    bus: Option<EventBus>,
    memory: Option<memory::MemoryMaintenance>,
    config: SafetyReviewScheduleConfig,
    last_evaluation: Arc<Mutex<Option<DateTime<Utc>>>>,
    shutdown_flag: Arc<tokio::sync::Notify>,
}

impl FlowSubsystem {
    pub fn new() -> Self {
        Self {
            bus: None,
            memory: None,
            config: SafetyReviewScheduleConfig::default(),
            last_evaluation: Arc::new(Mutex::new(None)),
            shutdown_flag: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub fn with_config(config: SafetyReviewScheduleConfig) -> Self {
        Self {
            bus: None,
            memory: None,
            config,
            last_evaluation: Arc::new(Mutex::new(None)),
            shutdown_flag: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub fn with_memory(mut self, memory: memory::MemoryMaintenance) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Trigger an immediate manual or scheduled run of the daily learned-rules safety review.
    pub fn trigger_safety_review(
        trigger: &str,
        bus: Option<&EventBus>,
    ) -> Option<SafetyReviewRunSummary> {
        let broker = match sail_guardian::global_broker() {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    "Cannot run safety review: Guardian broker unavailable ({})",
                    e
                );
                return None;
            }
        };

        let summary = broker.safety_review().run_review(trigger);

        if let Some(bus) = bus {
            let event = SailEvent::new(
                SubsystemId::Flow,
                EventPayload::SafetyReviewCompleted {
                    run_id: summary.run_id.to_string(),
                    trigger: summary.trigger.clone(),
                    findings_count: summary.findings_count,
                    high_or_critical_count: summary.high_or_critical_count,
                    summary: format!(
                        "Evaluated {} rules: {} findings ({} high/critical)",
                        summary.rules_evaluated,
                        summary.findings_count,
                        summary.high_or_critical_count
                    ),
                },
            );
            let _ = bus.publish(event);
        }

        Some(summary)
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
        info!(
            "SAIL Flow initialized (workflow, daily safety review & trigger orchestration engine)"
        );
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

        if let Some(memory) = self.memory.clone() {
            let shutdown = self.shutdown_flag.clone();
            tokio::spawn(async move {
                let mut ticks = tokio::time::interval(Duration::from_secs(60));
                ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _=shutdown.notified()=>break,
                        _=ticks.tick()=>{
                            let memory=memory.clone();
                            match tokio::task::spawn_blocking(move ||memory.tick(Utc::now().timestamp_millis())).await {
                                Ok(Ok(_))=>{},Ok(Err(error))=>warn!(%error,"Memory maintenance failed"),Err(error)=>warn!(%error,"Memory worker failed"),
                            }
                        }
                    }
                }
            });
        }

        // Spawn background scheduled evaluation worker with catch-up logic
        if self.config.enabled {
            let bus_opt = self.bus.clone();
            let interval = Duration::from_secs(self.config.interval_seconds.max(1));
            let last_eval = self.last_evaluation.clone();
            let notify_shutdown = self.shutdown_flag.clone();

            tokio::spawn(async move {
                // Initial catch-up run on startup if never evaluated
                tokio::time::sleep(Duration::from_millis(500)).await;
                let should_run = {
                    let last = last_eval.lock().unwrap();
                    last.is_none()
                };

                if should_run {
                    info!("SAIL Flow executing initial safety review catch-up pass");
                    if let Some(summary) = Self::trigger_safety_review("catchup", bus_opt.as_ref())
                    {
                        *last_eval.lock().unwrap() = Some(summary.completed_at);
                    }
                }

                loop {
                    tokio::select! {
                        _ = notify_shutdown.notified() => {
                            info!("Safety review background worker stopped");
                            break;
                        }
                        _ = tokio::time::sleep(interval) => {
                            info!("SAIL Flow executing scheduled daily safety review");
                            if let Some(summary) = Self::trigger_safety_review("scheduled", bus_opt.as_ref()) {
                                *last_eval.lock().unwrap() = Some(summary.completed_at);
                            }
                        }
                    }
                }
            });
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
        self.shutdown_flag.notify_waiters();
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
