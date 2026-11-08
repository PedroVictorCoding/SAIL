//! Agent task planning (`docs/SAIL_AGENT_PLANNING.md`): a goal becomes a plan Mind drafts
//! and SAIL validates against the tool registry; the owner approves plans with effects;
//! steps run through the [`ToolScheduler`] (Guardian, approvals, audit unchanged), each
//! step's result is checked, a failed step leads to a bounded re-plan, and the plan is a
//! Continuity task, so it survives restarts without repeating completed effects.
//!
//! The planner's output is untrusted text. Nothing it proposes runs before
//! [`validate_draft`] accepts it, and tool output is data that can never add steps.

use crate::tools::{ToolContext, ToolRegistry, ToolScheduler};
use async_trait::async_trait;
use sail_common::types::SubsystemId;
use sail_continuity::{
    Continuity, ContinuityError, EffectGate, Lease, Outcome, Resolution, ResumePolicy, TaskFilter,
    TaskSpec, TaskState,
};
use sail_protocol::execution::{ToolDescriptor, ToolEffect, ToolObservation};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const MAX_STEPS: usize = 32;
/// How often a background re-plan waits for live voice to go quiet before giving up.
const MAX_VOICE_WAITS: u32 = 20;
const VOICE_QUIET: std::time::Duration = std::time::Duration::from_secs(5);
pub const MAX_REPLANS: u32 = 3;
pub const OWNER: &str = "sail-agents";
pub const KIND: &str = "agents.plan";
/// Serialized input per step, and stored result data per step. Keeps a full plan well under
/// Continuity's 64 KiB checkpoint limit.
const MAX_INPUT_BYTES: usize = 1024;
const MAX_RECEIPT_BYTES: usize = 512;
const MAX_HISTORY: usize = 20;

/// How a step's success is judged. Pointers are JSON pointers (RFC 6901) into the tool's
/// output. A step never succeeds unless the tool itself reported success.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StepCheck {
    ToolSucceeded,
    Exists { pointer: String },
    Equals { pointer: String, value: Value },
    Contains { pointer: String, text: String },
}
impl StepCheck {
    pub fn passes(&self, observation: &ToolObservation) -> bool {
        if !observation.success {
            return false;
        }
        // Tools whose output schema is a string may return JSON text; check inside it.
        let parsed = observation
            .data
            .as_str()
            .and_then(|text| serde_json::from_str::<Value>(text).ok());
        let data = parsed.as_ref().unwrap_or(&observation.data);
        let at = |pointer: &str| data.pointer(pointer).filter(|v| !v.is_null());
        match self {
            Self::ToolSucceeded => true,
            Self::Exists { pointer } => at(pointer).is_some(),
            Self::Equals { pointer, value } => at(pointer) == Some(value),
            Self::Contains { pointer, text } => at(pointer)
                .and_then(Value::as_str)
                .is_some_and(|s| s.contains(text.as_str())),
        }
    }
    fn errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let (pointer, extra) = match self {
            Self::ToolSucceeded => return errors,
            Self::Exists { pointer } => (pointer, 0),
            Self::Equals { pointer, value } => (pointer, value.to_string().len()),
            Self::Contains { pointer, text } => (pointer, text.len()),
        };
        if !(pointer.is_empty() || pointer.starts_with('/')) || pointer.len() > 200 {
            errors.push(format!("invalid check pointer {pointer:?}"));
        }
        if extra > 200 {
            errors.push("check value too long".into());
        }
        errors
    }
}
fn tool_succeeded() -> StepCheck {
    StepCheck::ToolSucceeded
}
fn empty_object() -> Value {
    json!({})
}

/// A plan as the planner proposes it, before validation.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanDraft {
    pub steps: Vec<StepDraft>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StepDraft {
    pub id: String,
    pub intent: String,
    pub tool: String,
    #[serde(default = "empty_object")]
    pub input: Value,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default = "tool_succeeded")]
    pub check: StepCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Running,
    Done,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepReceipt {
    pub success: bool,
    pub check_passed: bool,
    /// Tool output, or a note when it was larger than the stored bound.
    pub data: Value,
    pub error: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub intent: String,
    pub tool: String,
    pub input: Value,
    pub depends_on: Vec<String>,
    pub check: StepCheck,
    /// The tool changes something (not read-only): needs plan approval and the effect gate.
    pub effectful: bool,
    pub state: StepState,
    pub receipt: Option<StepReceipt>,
}
impl Step {
    /// Identity of what the step does, for approvals and the effect journal.
    fn fingerprint(&self) -> String {
        let bytes = serde_json::to_vec(&json!([self.tool, self.input])).unwrap_or_default();
        ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    AwaitingApproval,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub goal: String,
    /// Binds Guardian approvals and audit records to this plan.
    pub correlation_id: Uuid,
    pub revision: u32,
    pub state: PlanState,
    pub steps: Vec<Step>,
    pub replans: u32,
    /// Fingerprints of effectful steps the owner approved.
    pub approved: Vec<String>,
    /// What happened, newest last: drafts, approvals, failures, re-plans.
    pub history: Vec<String>,
    pub reason: Option<String>,
}
impl Plan {
    fn note(&mut self, line: impl Into<String>) {
        let line: String = line.into().chars().take(300).collect();
        self.history.push(line);
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }
    }
    fn unapproved_effects(&self) -> bool {
        self.steps.iter().any(|s| {
            s.effectful
                && s.state == StepState::Pending
                && !self.approved.contains(&s.fingerprint())
        })
    }
}

/// A plan and the durable task's own state (which may say it needs the owner's attention).
#[derive(Debug, Clone, Serialize)]
pub struct PlanView {
    pub plan: Plan,
    pub task_state: TaskState,
    pub task_reason: Option<String>,
}

impl PlanView {
    /// A plan ended by the store rather than by its runner (cancelled by the owner while
    /// the daemon was down, or while it needed attention) shows the store's final state.
    fn new(mut plan: Plan, task: &sail_continuity::Task) -> Self {
        if !matches!(
            plan.state,
            PlanState::Succeeded | PlanState::Failed | PlanState::Cancelled
        ) {
            match task.state {
                TaskState::Cancelled => plan.state = PlanState::Cancelled,
                TaskState::Failed => plan.state = PlanState::Failed,
                _ => {}
            }
        }
        Self {
            plan,
            task_state: task.state,
            task_reason: task.reason.clone(),
        }
    }
}

/// Check a draft against the registry. `done` are steps already completed (on a re-plan);
/// new steps may depend on them. Returns every problem found, not just the first.
pub fn validate_draft(
    draft: &PlanDraft,
    registry: &ToolRegistry,
    done: &[Step],
    allowed: Option<&HashSet<String>>,
) -> Result<Vec<Step>, Vec<String>> {
    let mut errors = Vec::new();
    if draft.steps.is_empty() {
        errors.push("the plan has no steps".into());
    }
    if draft.steps.len() + done.len() > MAX_STEPS {
        errors.push(format!("at most {MAX_STEPS} steps"));
    }
    let done_ids: HashSet<&str> = done.iter().map(|s| s.id.as_str()).collect();
    let mut ids = HashSet::new();
    for step in &draft.steps {
        if step.id.is_empty()
            || step.id.len() > 48
            || !step
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        {
            errors.push(format!(
                "step id {:?} must be 1-48 of a-z, 0-9, _ or -",
                step.id
            ));
        }
        if !ids.insert(step.id.as_str()) || done_ids.contains(step.id.as_str()) {
            errors.push(format!("duplicate step id {:?}", step.id));
        }
        if step.intent.trim().is_empty() || step.intent.chars().count() > 300 {
            errors.push(format!(
                "step {:?}: intent must be 1-300 characters",
                step.id
            ));
        }
        if allowed.is_some_and(|a| !a.contains(&step.tool)) {
            errors.push(format!(
                "step {:?}: tool {} is not allowed for this task",
                step.id, step.tool
            ));
        } else if registry.descriptor(&step.tool).is_none() {
            errors.push(format!("step {:?}: unknown tool {}", step.id, step.tool));
        } else if let Err(error) = registry.validate_input(&step.tool, &step.input) {
            errors.push(format!(
                "step {:?}: input for {}: {error}",
                step.id, step.tool
            ));
        }
        if serde_json::to_vec(&step.input).map_or(true, |b| b.len() > MAX_INPUT_BYTES) {
            errors.push(format!(
                "step {:?}: input larger than {MAX_INPUT_BYTES} bytes",
                step.id
            ));
        }
        for dep in &step.depends_on {
            if dep == &step.id {
                errors.push(format!("step {:?} depends on itself", step.id));
            } else if !done_ids.contains(dep.as_str()) && !draft.steps.iter().any(|s| &s.id == dep)
            {
                errors.push(format!(
                    "step {:?} depends on unknown step {dep:?}",
                    step.id
                ));
            }
        }
        errors.extend(
            step.check
                .errors()
                .into_iter()
                .map(|e| format!("step {:?}: {e}", step.id)),
        );
    }
    if has_cycle(&draft.steps) {
        errors.push("the steps' dependencies form a cycle".into());
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(draft
        .steps
        .iter()
        .map(|s| Step {
            id: s.id.clone(),
            intent: s.intent.trim().into(),
            tool: s.tool.clone(),
            input: s.input.clone(),
            depends_on: s.depends_on.clone(),
            check: s.check.clone(),
            effectful: registry
                .descriptor(&s.tool)
                .is_none_or(|d| d.security_effect != ToolEffect::ReadOnly),
            state: StepState::Pending,
            receipt: None,
        })
        .collect())
}

fn has_cycle(steps: &[StepDraft]) -> bool {
    let local: HashSet<&str> = steps.iter().map(|s| s.id.as_str()).collect();
    let mut remaining: HashMap<&str, usize> = steps
        .iter()
        .map(|s| {
            let deps = s
                .depends_on
                .iter()
                .filter(|d| local.contains(d.as_str()))
                .count();
            (s.id.as_str(), deps)
        })
        .collect();
    let mut resolved = HashSet::new();
    loop {
        let ready: Vec<&str> = remaining
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(id, _)| *id)
            .collect();
        if ready.is_empty() {
            return !remaining.is_empty();
        }
        for id in ready {
            remaining.remove(id);
            resolved.insert(id);
        }
        for step in steps {
            if let Some(n) = remaining.get_mut(step.id.as_str()) {
                *n = step
                    .depends_on
                    .iter()
                    .filter(|d| local.contains(d.as_str()) && !resolved.contains(d.as_str()))
                    .count();
            }
        }
    }
}

/// The JSON object in a model reply (which may wrap it in prose or a code fence).
pub fn parse_draft(text: &str) -> Result<PlanDraft, String> {
    let start = text
        .find('{')
        .ok_or("no JSON object in the planner's reply")?;
    let end = text
        .rfind('}')
        .ok_or("no JSON object in the planner's reply")?;
    if end < start {
        return Err("no JSON object in the planner's reply".into());
    }
    serde_json::from_str(&text[start..=end]).map_err(|e| format!("plan JSON: {e}"))
}

/// What the planner is asked: the goal, the tools it may use, and on a re-plan the steps
/// already done and why the last one failed.
#[derive(Debug, Clone)]
pub struct PlanRequest {
    pub goal: String,
    pub tools: Vec<ToolDescriptor>,
    pub completed: Vec<Step>,
    pub failure: Option<String>,
    pub validation_errors: Vec<String>,
    /// The owner is waiting for this draft (a new goal). Re-plans while a plan runs are
    /// background work.
    pub interactive: bool,
}
impl PlanRequest {
    /// The prompt a language-model planner receives.
    pub fn prompt(&self) -> String {
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|t| {
                json!({"tool": t.id, "description": t.description, "input_schema": t.input_schema,
                "changes_something": t.security_effect != ToolEffect::ReadOnly})
            })
            .collect();
        let mut prompt = format!(
            "Plan how to achieve the goal using only these tools.\n\nGoal: {}\n\nTools: {}\n\n\
             Reply with one JSON object and nothing else: {{\"steps\": [{{\"id\": \"short_id\", \
             \"intent\": \"what this step is for\", \"tool\": \"tool id\", \"input\": {{...}}, \
             \"depends_on\": [\"id\"], \"check\": {{\"kind\": \"tool_succeeded\"}}}}]}}. \
             Checks: tool_succeeded; exists, equals or contains with a JSON pointer into the \
             tool's output. Use as few steps as possible; a step's input cannot use another \
             step's output.",
            self.goal,
            Value::Array(tools)
        );
        if !self.completed.is_empty() {
            let done: Vec<Value> = self
                .completed
                .iter()
                .map(|s| {
                    json!({"id": s.id, "intent": s.intent, "tool": s.tool,
                    "result": s.receipt.as_ref().map(|r| r.data.clone())})
                })
                .collect();
            prompt.push_str(&format!(
                "\n\nAlready done (keep them; new steps may depend on their ids): {}",
                Value::Array(done)
            ));
        }
        if let Some(failure) = &self.failure {
            prompt.push_str(&format!(
                "\n\nThe previous plan failed: {failure}. Plan only the remaining work, differently."
            ));
        }
        if !self.validation_errors.is_empty() {
            prompt.push_str(&format!(
                "\n\nYour last plan was rejected: {}. Fix these problems.",
                self.validation_errors.join("; ")
            ));
        }
        prompt
    }
}

/// Drafts plans. Its output is untrusted text, validated before use.
#[async_trait]
pub trait Planner: Send + Sync {
    /// A short name of the model that planned, shown with the plan.
    fn name(&self) -> String;
    async fn plan(&self, request: &PlanRequest) -> Result<String, String>;
}

/// Plans with the resident local model through Mind. A new goal is drafted at interactive
/// priority, since the owner is waiting; a re-plan is background work that waits while
/// live voice has priority, so a conversation pauses a plan instead of failing it.
pub struct MindPlanner {
    mind: sail_mind::MindService,
}
impl MindPlanner {
    pub fn new(mind: sail_mind::MindService) -> Self {
        Self { mind }
    }
}
#[async_trait]
impl Planner for MindPlanner {
    fn name(&self) -> String {
        "local model".into()
    }
    async fn plan(&self, request: &PlanRequest) -> Result<String, String> {
        use sail_protocol::resources::WorkPriority;
        use sail_protocol::{Generation, InferenceRequest, Message, Role};
        let mut inference = InferenceRequest::text(request.prompt());
        inference.messages.insert(
            0,
            Message::text(
                Role::System,
                "You are SAIL's task planner. You output only a JSON plan.",
            ),
        );
        inference.generation = Generation {
            // Mind's output limit; larger requests are rejected outright.
            max_tokens: sail_protocol::ResourceLimits::default().max_output_tokens,
            temperature: 0.1,
            reasoning_budget: None,
        };
        let priority = if request.interactive {
            WorkPriority::Interactive
        } else {
            WorkPriority::Background
        };
        let mut voice_waits = 0;
        let response = loop {
            match self
                .mind
                .infer_with_priority(inference.clone(), CancellationToken::new(), None, priority)
                .await
            {
                Err(error)
                    if priority == WorkPriority::Background
                        && error.message.contains("live voice")
                        && voice_waits < MAX_VOICE_WAITS =>
                {
                    voice_waits += 1;
                    sail_common::resources::global()
                        .voice_idle(VOICE_QUIET)
                        .await;
                }
                result => break result.map_err(|e| e.to_string())?,
            }
        };
        response
            .message
            .content
            .text()
            .map(str::to_owned)
            .ok_or_else(|| "the planner returned no text".into())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("planning failed: {0}")]
    Planning(String),
    #[error("plan {0} is not awaiting approval")]
    NotAwaitingApproval(String),
    #[error(transparent)]
    Continuity(#[from] ContinuityError),
    #[error("stored plan is unreadable: {0}")]
    Corrupt(String),
}

enum StepOutcome {
    Done,
    Failed(String),
    NeedsAttention(String),
}

/// Creates, stores, runs and recovers plans.
pub struct PlanRunner {
    continuity: Arc<Continuity>,
    scheduler: Arc<ToolScheduler>,
    registry: Arc<ToolRegistry>,
    planner: Arc<dyn Planner>,
    actor: SubsystemId,
    allowed_tools: Option<HashSet<String>>,
    /// Leases this runner holds (one worker per plan).
    leases: Mutex<HashMap<String, Lease>>,
    /// Cancels a running plan, including the step in progress.
    running: Mutex<HashMap<String, CancellationToken>>,
}

impl PlanRunner {
    pub fn new(
        continuity: Arc<Continuity>,
        scheduler: Arc<ToolScheduler>,
        registry: Arc<ToolRegistry>,
        planner: Arc<dyn Planner>,
        actor: SubsystemId,
    ) -> Self {
        Self {
            continuity,
            scheduler,
            registry,
            planner,
            actor,
            allowed_tools: None,
            leases: Mutex::new(HashMap::new()),
            running: Mutex::new(HashMap::new()),
        }
    }
    /// Restrict plans to these tools (the purpose's grant).
    pub fn with_allowed_tools(mut self, tools: impl IntoIterator<Item = String>) -> Self {
        self.allowed_tools = Some(tools.into_iter().collect());
        self
    }

    fn tools(&self) -> Vec<ToolDescriptor> {
        self.registry
            .descriptors()
            .into_iter()
            .filter(|d| {
                self.allowed_tools
                    .as_ref()
                    .is_none_or(|a| a.contains(&d.id))
            })
            .collect()
    }

    /// Ask the planner, validate, and ask once more with the errors if needed.
    async fn draft(
        &self,
        goal: &str,
        done: &[Step],
        failure: Option<String>,
    ) -> Result<Vec<Step>, String> {
        let mut request = PlanRequest {
            goal: goal.into(),
            tools: self.tools(),
            completed: done.to_vec(),
            interactive: done.is_empty() && failure.is_none(),
            failure,
            validation_errors: Vec::new(),
        };
        for attempt in 0..2 {
            let text = self.planner.plan(&request).await?;
            let errors = match parse_draft(&text) {
                Ok(draft) => {
                    match validate_draft(&draft, &self.registry, done, self.allowed_tools.as_ref())
                    {
                        Ok(steps) => return Ok(steps),
                        Err(errors) => errors,
                    }
                }
                Err(error) => vec![error],
            };
            if attempt == 1 {
                return Err(format!("the plan was invalid: {}", errors.join("; ")));
            }
            request.validation_errors = errors;
        }
        unreachable!()
    }

    /// Plan a goal and store it. Plans with effectful steps wait for [`Self::approve`];
    /// read-only plans are ready for [`Self::run`].
    pub async fn create(&self, goal: &str) -> Result<Plan, PlanError> {
        let goal = goal.trim();
        if goal.is_empty() || goal.chars().count() > 2000 {
            return Err(PlanError::Planning(
                "the goal must be 1-2000 characters".into(),
            ));
        }
        let steps = self
            .draft(goal, &[], None)
            .await
            .map_err(PlanError::Planning)?;
        let mut plan = Plan {
            id: format!("plan-{}", Uuid::new_v4().simple()),
            goal: goal.into(),
            correlation_id: Uuid::new_v4(),
            revision: 1,
            state: PlanState::Running,
            steps,
            replans: 0,
            approved: Vec::new(),
            history: Vec::new(),
            reason: None,
        };
        plan.note(format!(
            "Planned by {} ({} steps)",
            self.planner.name(),
            plan.steps.len()
        ));
        if plan.unapproved_effects() {
            plan.state = PlanState::AwaitingApproval;
        }
        let title: String = goal.chars().take(120).collect();
        let spec = TaskSpec::new(OWNER, KIND, title)
            .with_id(plan.id.clone())
            .with_resume(ResumePolicy::Restart)
            .with_checkpoint(
                serde_json::to_value(&plan).map_err(|e| PlanError::Corrupt(e.to_string()))?,
            );
        self.continuity.submit(spec)?;
        let lease = self.continuity.claim(&plan.id)?;
        self.continuity.progress(
            &lease,
            stage(&plan),
            &plan.history.last().cloned().unwrap_or_default(),
        )?;
        self.leases.lock().unwrap().insert(plan.id.clone(), lease);
        Ok(plan)
    }

    pub fn get(&self, id: &str) -> Result<PlanView, PlanError> {
        let task = self.continuity.get(id)?;
        let plan = serde_json::from_value(task.checkpoint.clone())
            .map_err(|e| PlanError::Corrupt(e.to_string()))?;
        Ok(PlanView::new(plan, &task))
    }

    /// Recent plans, newest first, finished ones included. Unreadable records are skipped.
    pub fn list(&self, limit: usize) -> Result<Vec<PlanView>, PlanError> {
        let tasks = self.continuity.list(&TaskFilter {
            owner: Some(OWNER.into()),
            include_terminal: true,
            limit: Some(limit),
        })?;
        Ok(tasks
            .into_iter()
            .filter(|task| task.kind == KIND)
            .filter_map(|task| {
                Some(PlanView::new(
                    serde_json::from_value(task.checkpoint.clone()).ok()?,
                    &task,
                ))
            })
            .collect())
    }

    fn lease(&self, id: &str) -> Result<Lease, PlanError> {
        if let Some(lease) = self.leases.lock().unwrap().get(id) {
            return Ok(lease.clone());
        }
        let lease = self.continuity.claim(id)?;
        self.leases.lock().unwrap().insert(id.into(), lease.clone());
        Ok(lease)
    }

    fn save(&self, lease: &Lease, plan: &Plan) -> Result<(), PlanError> {
        let value = serde_json::to_value(plan).map_err(|e| PlanError::Corrupt(e.to_string()))?;
        self.continuity.save_checkpoint(lease, &value)?;
        self.continuity.progress(
            lease,
            stage(plan),
            &plan.history.last().cloned().unwrap_or_default(),
        )?;
        Ok(())
    }

    /// The owner approves the plan's effectful steps as they stand. Guardian still checks
    /// each step when it runs.
    pub fn approve(&self, id: &str) -> Result<Plan, PlanError> {
        let mut plan = self.get(id)?.plan;
        if plan.state != PlanState::AwaitingApproval {
            return Err(PlanError::NotAwaitingApproval(id.into()));
        }
        let lease = self.lease(id)?;
        let approved: Vec<String> = plan
            .steps
            .iter()
            .filter(|s| s.effectful && s.state == StepState::Pending)
            .map(Step::fingerprint)
            .collect();
        plan.approved.extend(approved);
        plan.state = PlanState::Running;
        plan.note(format!(
            "Approved by the owner (revision {})",
            plan.revision
        ));
        self.save(&lease, &plan)?;
        Ok(plan)
    }

    /// The owner replaces the plan's steps before approving. Validated like a draft.
    pub fn edit(&self, id: &str, draft: &PlanDraft) -> Result<Plan, PlanError> {
        let mut plan = self.get(id)?.plan;
        if plan.state != PlanState::AwaitingApproval {
            return Err(PlanError::NotAwaitingApproval(id.into()));
        }
        let lease = self.lease(id)?;
        let done: Vec<Step> = plan
            .steps
            .iter()
            .filter(|s| s.state == StepState::Done)
            .cloned()
            .collect();
        let steps = validate_draft(draft, &self.registry, &done, self.allowed_tools.as_ref())
            .map_err(|e| PlanError::Planning(e.join("; ")))?;
        plan.steps = done.into_iter().chain(steps).collect();
        plan.revision += 1;
        plan.note(format!("Edited by the owner (revision {})", plan.revision));
        self.save(&lease, &plan)?;
        Ok(plan)
    }

    /// Stop the plan, including the step in progress. Durable: a plan cancelled while the
    /// daemon is down is not resumed.
    pub fn cancel(&self, id: &str, reason: &str) -> Result<(), PlanError> {
        self.continuity.request_cancel(id, reason)?;
        if let Some(token) = self.running.lock().unwrap().get(id) {
            token.cancel();
        }
        Ok(())
    }

    /// The owner's answer for a plan whose step may or may not have happened: `retry` after
    /// checking that it did not happen (the plan is queued to run again; call [`Self::run`]),
    /// or cancel the plan.
    pub fn resolve(&self, id: &str, retry: bool) -> Result<PlanView, PlanError> {
        let resolution = if retry {
            Resolution::Retry
        } else {
            Resolution::Cancel
        };
        self.continuity.resolve(id, resolution)?;
        // Any lease from before the interruption is no longer valid.
        self.leases.lock().unwrap().remove(id);
        self.get(id)
    }

    /// Run the plan until it succeeds, fails, is cancelled, or needs the owner (approval of
    /// a re-planned effect, or an effect whose outcome is in doubt).
    pub async fn run(&self, id: &str, cancel: CancellationToken) -> Result<Plan, PlanError> {
        let cancel = cancel.child_token();
        self.running
            .lock()
            .unwrap()
            .insert(id.into(), cancel.clone());
        let result = self.run_inner(id, &cancel).await;
        self.running.lock().unwrap().remove(id);
        result
    }

    async fn run_inner(&self, id: &str, cancel: &CancellationToken) -> Result<Plan, PlanError> {
        let lease = self.lease(id)?;
        let mut plan = self.get(id)?.plan;
        // A step running when the daemon stopped starts over; effectful steps pass through
        // the effect gate, so a completed effect is not repeated.
        for step in &mut plan.steps {
            if step.state == StepState::Running {
                step.state = StepState::Pending;
            }
        }
        loop {
            if cancel.is_cancelled() || self.continuity.is_cancel_requested(id)? {
                plan.state = PlanState::Cancelled;
                plan.note("Cancelled");
                self.save(&lease, &plan)?;
                self.finish(&lease, Outcome::Cancelled("cancelled by the owner".into()))?;
                return Ok(plan);
            }
            if plan.state != PlanState::Running {
                self.save(&lease, &plan)?;
                return Ok(plan);
            }
            let next = plan.steps.iter().position(|s| {
                s.state == StepState::Pending
                    && s.depends_on.iter().all(|d| {
                        plan.steps
                            .iter()
                            .any(|o| &o.id == d && o.state == StepState::Done)
                    })
            });
            let Some(index) = next else {
                if plan.steps.iter().all(|s| s.state == StepState::Done) {
                    plan.state = PlanState::Succeeded;
                    plan.note("All steps done and checked");
                    self.save(&lease, &plan)?;
                    self.finish(&lease, Outcome::Succeeded)?;
                } else {
                    plan.state = PlanState::Failed;
                    plan.reason = Some("remaining steps cannot run".into());
                    self.save(&lease, &plan)?;
                    self.finish(&lease, Outcome::Failed("remaining steps cannot run".into()))?;
                }
                return Ok(plan);
            };
            match self.run_step(&lease, &mut plan, index, cancel).await? {
                StepOutcome::Done => {}
                StepOutcome::NeedsAttention(reason) => {
                    plan.reason = Some(reason.clone());
                    plan.note(reason);
                    self.save(&lease, &plan)?;
                    return Ok(plan);
                }
                StepOutcome::Failed(_)
                    if cancel.is_cancelled() || self.continuity.is_cancel_requested(id)? =>
                {
                    // Stopped by cancellation, not a reason to re-plan: handled at the top.
                    plan.steps[index].state = StepState::Pending;
                }
                StepOutcome::Failed(reason) => {
                    plan.note(format!("Step {} failed: {reason}", plan.steps[index].id));
                    if let Err(error) = self.replan(&mut plan, &reason).await {
                        plan.state = PlanState::Failed;
                        plan.reason = Some(error.clone());
                        plan.note(error.clone());
                        self.save(&lease, &plan)?;
                        self.finish(&lease, Outcome::Failed(error))?;
                        return Ok(plan);
                    }
                    self.save(&lease, &plan)?;
                }
            }
        }
    }

    async fn run_step(
        &self,
        lease: &Lease,
        plan: &mut Plan,
        index: usize,
        cancel: &CancellationToken,
    ) -> Result<StepOutcome, PlanError> {
        plan.steps[index].state = StepState::Running;
        plan.note(format!(
            "Running {}: {}",
            plan.steps[index].id, plan.steps[index].intent
        ));
        self.save(lease, plan)?;
        let step = plan.steps[index].clone();
        let key = format!("step:{}:{}", step.id, step.fingerprint());
        if step.effectful {
            match self.continuity.begin_effect(&plan.id, &key, &step.intent) {
                Ok(EffectGate::Proceed) => {}
                Ok(EffectGate::AlreadyDone { receipt }) => {
                    // Done before a restart: keep its receipt, never repeat the effect.
                    plan.steps[index].receipt = serde_json::from_str(&receipt).ok();
                    plan.steps[index].state = StepState::Done;
                    plan.note(format!("Step {} was already done; not repeated", step.id));
                    return Ok(StepOutcome::Done);
                }
                Err(
                    ContinuityError::EffectInDoubt { .. }
                    | ContinuityError::EffectInProgress { .. },
                ) => {
                    return Ok(StepOutcome::NeedsAttention(format!(
                        "Needs you: step {} ({}) was interrupted and may or may not have happened",
                        step.id, step.intent
                    )));
                }
                Err(error) => return Err(error.into()),
            }
        }
        let context = ToolContext {
            request_id: Uuid::new_v4(),
            correlation_id: plan.correlation_id,
            actor: self.actor,
            cancel: cancel.child_token(),
        };
        let observation = self
            .scheduler
            .execute(context, &step.tool, step.input.clone(), None, false)
            .await;
        let passed = step.check.passes(&observation);
        let data = match serde_json::to_vec(&observation.data) {
            Ok(bytes) if bytes.len() <= MAX_RECEIPT_BYTES => observation.data.clone(),
            Ok(bytes) => json!({"note": "output not stored", "bytes": bytes.len()}),
            Err(_) => Value::Null,
        };
        let receipt = StepReceipt {
            success: observation.success,
            check_passed: passed,
            data,
            error: observation.error.clone(),
        };
        if step.effectful {
            if observation.success {
                let stored = serde_json::to_string(&receipt).unwrap_or_default();
                self.continuity.commit_effect(&plan.id, &key, &stored)?;
            } else {
                self.continuity.fail_effect(
                    &plan.id,
                    &key,
                    observation.error.as_deref().unwrap_or("failed"),
                )?;
            }
        }
        plan.steps[index].receipt = Some(receipt);
        if passed {
            plan.steps[index].state = StepState::Done;
            plan.note(format!("Step {} done and checked", step.id));
            Ok(StepOutcome::Done)
        } else {
            plan.steps[index].state = StepState::Failed;
            Ok(StepOutcome::Failed(match observation.error {
                Some(error) => error,
                None => "its result did not pass the step's check".into(),
            }))
        }
    }

    /// Replace the unfinished steps with a new plan from the current state. Bounded.
    async fn replan(&self, plan: &mut Plan, failure: &str) -> Result<(), String> {
        if plan.replans >= MAX_REPLANS {
            return Err(format!("stopped after {MAX_REPLANS} re-plans: {failure}"));
        }
        plan.replans += 1;
        let done: Vec<Step> = plan
            .steps
            .iter()
            .filter(|s| s.state == StepState::Done)
            .cloned()
            .collect();
        let steps = self.draft(&plan.goal, &done, Some(failure.into())).await?;
        plan.steps = done.into_iter().chain(steps).collect();
        plan.revision += 1;
        plan.note(format!(
            "Re-planned by {} (revision {}, re-plan {} of {MAX_REPLANS})",
            self.planner.name(),
            plan.revision,
            plan.replans
        ));
        if plan.unapproved_effects() {
            plan.state = PlanState::AwaitingApproval;
            plan.note("The new plan changes something: waiting for approval");
        }
        Ok(())
    }

    fn finish(&self, lease: &Lease, outcome: Outcome) -> Result<(), PlanError> {
        self.continuity.finish(lease, outcome)?;
        self.leases.lock().unwrap().remove(lease.task_id());
        Ok(())
    }

    /// After a restart: take over every unfinished plan. Returns the ids of plans that are
    /// running and should be [`Self::run`] again. Plans with an effect in doubt are left in
    /// "needs attention" by Continuity's recovery and are not taken over.
    pub fn resume_all(&self) -> Result<Vec<String>, PlanError> {
        let tasks = self.continuity.list(&TaskFilter {
            owner: Some(OWNER.into()),
            include_terminal: false,
            limit: None,
        })?;
        let mut runnable = Vec::new();
        for task in tasks {
            if task.kind != KIND
                || !matches!(task.state, TaskState::Queued | TaskState::Interrupted)
            {
                continue;
            }
            let plan: Plan = match serde_json::from_value(task.checkpoint.clone()) {
                Ok(plan) => plan,
                Err(_) => continue,
            };
            self.lease(&task.id)?;
            if plan.state == PlanState::Running {
                runnable.push(task.id);
            }
        }
        Ok(runnable)
    }
}

fn stage(plan: &Plan) -> &'static str {
    match plan.state {
        PlanState::AwaitingApproval => "awaiting_approval",
        PlanState::Running => "running",
        PlanState::Succeeded => "succeeded",
        PlanState::Failed => "failed",
        PlanState::Cancelled => "cancelled",
    }
}
