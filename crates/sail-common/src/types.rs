use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubsystemId {
    Core,
    Dialogue,
    Arena,
    Canvas,
    Flow,
    Discovery,
    Agents,
    Mind,
    Bridge,
    Guardian,
    Forge,
    Ui,
}

impl std::fmt::Display for SubsystemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                SubsystemId::Core => "sail-core",
                SubsystemId::Dialogue => "sail-dialogue",
                SubsystemId::Arena => "sail-arena",
                SubsystemId::Canvas => "sail-canvas",
                SubsystemId::Flow => "sail-flow",
                SubsystemId::Discovery => "sail-discovery",
                SubsystemId::Agents => "sail-agents",
                SubsystemId::Mind => "sail-mind",
                SubsystemId::Bridge => "sail-bridge",
                SubsystemId::Guardian => "sail-guardian",
                SubsystemId::Forge => "sail-forge",
                SubsystemId::Ui => "sail-ui",
            }
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SailEvent {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub source: SubsystemId,
    pub target: Option<SubsystemId>,
    pub correlation_id: Option<Uuid>,
    pub payload: EventPayload,
}

impl SailEvent {
    pub fn new(source: SubsystemId, payload: EventPayload) -> Self {
        Self {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            source,
            target: None,
            correlation_id: None,
            payload,
        }
    }

    pub fn with_target(mut self, target: SubsystemId) -> Self {
        self.target = Some(target);
        self
    }

    pub fn with_correlation_id(mut self, correlation_id: Uuid) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum EventPayload {
    // System Lifecycle
    SystemReady {
        subsystem: SubsystemId,
    },
    SystemShutdown {
        reason: String,
    },

    // Dialogue Events (User <-> Dialogue <-> Agents)
    DialogueInput {
        session_id: String,
        speaker: String,
        content: String,
        is_final: bool,
    },
    DialogueInterruption {
        session_id: String,
    },
    DialogueResponseDelta {
        session_id: String,
        delta: String,
        is_finished: bool,
    },

    // Discovery Events (Arena -> Discovery -> Flow)
    KnowledgeGapDetected {
        topic: String,
        priority: f32,
        hypothesis: String,
    },
    ResearchTriggerDispatched {
        workflow_id: String,
        query: String,
    },

    // Flow Events (Flow <-> Agents, Flow -> Canvas)
    WorkflowStarted {
        workflow_id: String,
        name: String,
    },
    TaskDispatched {
        task_id: String,
        workflow_id: Option<String>,
        goal: String,
    },
    TaskCompleted {
        task_id: String,
        success: bool,
        output: String,
    },

    // Agent Events (Agents -> Arena, Mind, Bridge, Canvas)
    InferenceRequested {
        request_id: String,
        prompt: String,
        model_preference: Option<String>,
    },
    InferenceFailed {
        request_id: String,
        error: sail_protocol::InferenceError,
    },
    InferenceCompleted {
        request_id: String,
        content: String,
        tokens_used: usize,
    },
    ToolExecute {
        execution_id: String,
        tool: String,
        args: serde_json::Value,
    },
    ToolResult {
        execution_id: String,
        success: bool,
        output: String,
    },
    MemoryRecordStored {
        record_id: String,
        collection: String,
        summary: String,
    },
    CanvasRender {
        canvas_id: String,
        artifact_type: String, // "mermaid", "markdown", "ui_card"
        content: String,
    },

    // Guardian Access Control & Audit Events
    AccessEvaluationRequested {
        request_id: String,
        requester: SubsystemId,
        capability: String,
        resource: String,
        justification: String,
    },
    AccessDecisionGranted {
        request_id: String,
        capability: String,
        resource: String,
    },
    AccessDecisionDenied {
        request_id: String,
        capability: String,
        resource: String,
        reason: String,
    },
    UserApprovalPrompted {
        request_id: String,
        risk_level: String, // "low", "medium", "high", "critical"
        description: String,
    },
    UserApprovalResolved {
        request_id: String,
        approved: bool,
    },
    AuditReceiptCreated {
        receipt_id: String,
        hash: String,
        action: String,
        decision: String,
    },

    // Forge Autonomous Coding Agent Events
    ForgeIssueEnqueued {
        job_id: String,
        repo: String,
        issue_number: u64,
        priority: Option<String>,
    },
    ForgeIssueStatusUpdated {
        job_id: String,
        state: String,
        stage: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_file: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        logs: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        live_output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u32>,
    },
    ForgeReviewCompleted {
        job_id: String,
        passed: bool,
        cost_usd: f64,
        finding_count: usize,
    },
    ForgeJobFinished {
        job_id: String,
        success: bool,
        branch: String,
        pr_draft_ready: bool,
        summary: String,
    },

    // Guardian Safety Review Events
    SafetyReviewCompleted {
        run_id: String,
        trigger: String,
        findings_count: usize,
        high_or_critical_count: usize,
        summary: String,
    },

    // Adaptive Ambient UI Events
    UiAmbientModeChanged {
        mode: String,
    },
}
