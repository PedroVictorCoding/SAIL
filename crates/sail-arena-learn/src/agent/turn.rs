//! The engine: sessions, answers, and the turn runner (#240) with the probe → teach
//! handoff (#241).
//!
//! A turn is an application-level run, not an HTTP request. It may make several model
//! calls and run tools until a question waits for the learner, the model finishes with a
//! plain message, or the loop bound is hit. Each assistant message and all of its tool
//! effects are appended in one atomic batch. A browser disconnect never cancels a turn;
//! only `abort` does.
//!
//! When the latest answer is free recall with a rubric, the turn first runs the judge
//! (#243) and records its verdict. An `unclear` verdict asks the judge's follow-up question
//! straight away (once per question) and the tutor is not called.
use super::judge::{self, JudgeError};
use super::{args, prompts, tools};
use crate::core::events::{
    AgentRole, Envelope, LearningEvent, NewEvent, PresentKind, ToolCallRecord, TurnOutcome,
};
use crate::core::messages::{to_llm_messages, window};
use crate::core::plan::LessonPlan;
use crate::core::ports::{EventStore, Model, SessionSummary, StoreError};
use crate::core::question::{Answer, Grade, QuestionKind, QuestionSpec};
use crate::core::view::{
    PendingJudgment, SessionView, latest_plan, pending_judgment, project_view, question_spec,
    recorded_answer,
};
use crate::core::{Invalid, MAX_SHORT, MAX_TEXT, bounded};
use sail_protocol::{Generation, InferenceRequest, ToolChoice};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tools::Tool;
use tracing::{debug, warn};

/// Model calls per turn (the handoff to the teacher counts too).
pub const MAX_CALLS: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum TurnError {
    #[error("a turn is already running for this session")]
    Busy,
    #[error("answer the open question first")]
    QuestionOpen,
    #[error("the session is paused")]
    Paused,
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<Invalid> for TurnError {
    fn from(e: Invalid) -> Self {
        TurnError::Invalid(e.0)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AnswerOutcome {
    pub grade: Grade,
    /// True when this answer had already been recorded; nothing changed.
    pub duplicate: bool,
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// A change notification for live views: an event was stored (`seq`), or a turn ended
/// (`seq: None`). Receivers re-read the view; nothing in it is authoritative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub session: String,
    pub seq: Option<u64>,
}

pub struct Engine {
    store: Arc<dyn EventStore>,
    model: Arc<dyn Model>,
    running: Mutex<HashMap<String, CancellationToken>>,
    updates: broadcast::Sender<Update>,
    clock: Clock,
}

fn system_clock() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// Releases the per-session liveness claim when a turn ends, however it ends.
struct Claim<'a> {
    engine: &'a Engine,
    session: String,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.engine
            .running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.session);
        let _ = self.engine.updates.send(Update {
            session: self.session.clone(),
            seq: None,
        });
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeDoneArgs {
    summary: String,
    #[serde(default)]
    known: Vec<String>,
    #[serde(default)]
    gaps: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExplainArgs {
    node_id: String,
    text: String,
}

/// What one tool call did to the turn.
enum Effect {
    Continue,
    Question,
    Handoff,
}

/// State a batch of tool calls is validated against.
struct TurnState {
    plan: Option<(u64, LessonPlan)>,
    handed_off: bool,
}

impl Engine {
    pub fn new(store: Arc<dyn EventStore>, model: Arc<dyn Model>) -> Self {
        Self::with_clock(store, model, Arc::new(system_clock))
    }

    pub fn with_clock(store: Arc<dyn EventStore>, model: Arc<dyn Model>, clock: Clock) -> Self {
        Self {
            store,
            model,
            running: Mutex::new(HashMap::new()),
            updates: broadcast::channel(256).0,
            clock,
        }
    }

    /// Change notifications from every session, for streaming views.
    pub fn subscribe(&self) -> broadcast::Receiver<Update> {
        self.updates.subscribe()
    }

    fn publish(&self, stored: &[Envelope]) {
        for envelope in stored {
            let _ = self.updates.send(Update {
                session: envelope.session.clone(),
                seq: Some(envelope.seq),
            });
        }
    }

    pub fn is_running(&self, session: &str) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(session)
    }

    pub fn create_session(&self, goal: &str) -> Result<SessionView, TurnError> {
        bounded("goal", goal, MAX_SHORT)?;
        let id = new_id("learn");
        let created = self.store.create(&id, goal.trim(), (self.clock)())?;
        self.publish(std::slice::from_ref(&created));
        self.view(&id)
    }

    pub fn sessions(&self, limit: usize) -> Result<Vec<SessionSummary>, TurnError> {
        Ok(self.store.sessions(limit)?)
    }

    pub fn events(&self, session: &str) -> Result<Vec<Envelope>, TurnError> {
        Ok(self.store.events(session)?)
    }

    pub fn view(&self, session: &str) -> Result<SessionView, TurnError> {
        Ok(project_view(session, &self.store.events(session)?))
    }

    pub fn delete(&self, session: &str) -> Result<bool, TurnError> {
        self.abort(session);
        Ok(self.store.delete(session)?)
    }

    fn append(
        &self,
        session: &str,
        expected: u64,
        events: Vec<NewEvent>,
    ) -> Result<Vec<Envelope>, TurnError> {
        let stored = self
            .store
            .append(session, expected, (self.clock)(), &events)?;
        self.publish(&stored);
        Ok(stored)
    }

    /// Records the learner's answer exactly once. A repeated submission (same question)
    /// returns the first result and changes nothing.
    pub fn answer(
        &self,
        session: &str,
        call_id: &str,
        answer: Answer,
    ) -> Result<AnswerOutcome, TurnError> {
        let events = self.store.events(session)?;
        if let Some((_, grade)) = recorded_answer(&events, call_id) {
            return Ok(AnswerOutcome {
                grade,
                duplicate: true,
            });
        }
        let spec = question_spec(&events, call_id)
            .ok_or_else(|| TurnError::Invalid("no such question".into()))?;
        let view = project_view(session, &events);
        if view.active_question.as_deref() != Some(call_id) {
            return Err(TurnError::Invalid("that question is not open".into()));
        }
        answer.validate()?;
        let grade = spec.grade(&answer)?;
        let stored = self.append(
            session,
            view.last_seq,
            vec![NewEvent::new(
                format!("answer:{call_id}"),
                LearningEvent::Answered {
                    call_id: call_id.to_string(),
                    answer,
                    grade,
                },
            )],
        );
        match stored {
            Ok(_) => Ok(AnswerOutcome {
                grade,
                duplicate: false,
            }),
            // A concurrent duplicate submission won the race: report its result.
            Err(TurnError::Store(StoreError::Conflict { .. })) => {
                let events = self.store.events(session)?;
                recorded_answer(&events, call_id)
                    .map(|(_, grade)| AnswerOutcome {
                        grade,
                        duplicate: true,
                    })
                    .ok_or(TurnError::Busy)
            }
            Err(e) => Err(e),
        }
    }

    /// A free message from the learner ("explain differently", "too easy"). `key` makes
    /// retries idempotent.
    pub fn message(&self, session: &str, key: &str, text: &str) -> Result<(), TurnError> {
        bounded("text", text, MAX_TEXT)?;
        bounded("key", key, 80)?;
        if self.is_running(session) {
            return Err(TurnError::Busy);
        }
        let view = self.view(session)?;
        if view.active_question.is_some() {
            return Err(TurnError::QuestionOpen);
        }
        self.append(
            session,
            view.last_seq,
            vec![NewEvent::new(
                format!("message:{key}"),
                LearningEvent::LearnerMessage {
                    text: text.to_string(),
                },
            )],
        )?;
        Ok(())
    }

    pub fn set_paused(&self, session: &str, paused: bool) -> Result<SessionView, TurnError> {
        let view = self.view(session)?;
        if view.paused != paused {
            if paused {
                self.abort(session);
            }
            let event = if paused {
                LearningEvent::Paused
            } else {
                LearningEvent::Resumed
            };
            self.append(
                session,
                view.last_seq,
                vec![NewEvent::new(new_id("pause"), event)],
            )?;
        }
        self.view(session)
    }

    /// Cancels a running turn. Returns false when none was running.
    pub fn abort(&self, session: &str) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session)
            .map(|t| t.cancel())
            .is_some()
    }

    fn claim(&self, session: &str) -> Result<(Claim<'_>, CancellationToken), TurnError> {
        let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner());
        if running.contains_key(session) {
            return Err(TurnError::Busy);
        }
        let token = CancellationToken::new();
        running.insert(session.to_string(), token.clone());
        Ok((
            Claim {
                engine: self,
                session: session.to_string(),
            },
            token,
        ))
    }

    fn check_can_run(&self, session: &str) -> Result<(), TurnError> {
        let view = self.view(session)?;
        if view.paused {
            return Err(TurnError::Paused);
        }
        if view.active_question.is_some() {
            return Err(TurnError::QuestionOpen);
        }
        Ok(())
    }

    /// Starts a turn in the background and returns at once. The turn outlives the caller.
    pub fn start_turn(self: &Arc<Self>, session: &str) -> Result<(), TurnError> {
        self.check_can_run(session)?;
        if self.is_running(session) {
            return Err(TurnError::Busy);
        }
        let engine = self.clone();
        let session = session.to_string();
        // Claim before spawning so a second request sees Busy immediately.
        let token = {
            let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner());
            if running.contains_key(&session) {
                return Err(TurnError::Busy);
            }
            let token = CancellationToken::new();
            running.insert(session.clone(), token.clone());
            token
        };
        tokio::spawn(async move {
            let claim = Claim {
                engine: &engine,
                session: session.clone(),
            };
            if let Err(error) = engine.run_claimed(&session, token).await {
                warn!(%error, session, "Arena Learn turn did not settle");
            }
            drop(claim);
        });
        Ok(())
    }

    /// Runs one turn to completion in the caller's task.
    pub async fn run_turn(&self, session: &str) -> Result<TurnOutcome, TurnError> {
        self.check_can_run(session)?;
        let (claim, token) = self.claim(session)?;
        let result = self.run_claimed(session, token).await;
        drop(claim);
        result
    }

    async fn run_claimed(
        &self,
        session: &str,
        cancel: CancellationToken,
    ) -> Result<TurnOutcome, TurnError> {
        let turn = new_id("turn");
        let mut n = 0usize;
        let mut key = || {
            n += 1;
            format!("{turn}:{n}")
        };
        let events = self.store.events(session)?;
        let mut view = project_view(session, &events);
        let mut role = view.role();
        let mut state = TurnState {
            plan: latest_plan(&events),
            handed_off: false,
        };
        let (mut input_tokens, mut output_tokens) = (0u64, 0u64);
        let mut calls = 0usize;
        let mut events = events;

        let outcome = 'turn: {
            if let Some(pending) = pending_judgment(&events) {
                let (judgment, usage) = match judge::judge(
                    self.model.as_ref(),
                    &pending.spec,
                    &pending.answer,
                    &cancel,
                )
                .await
                {
                    Ok(done) => done,
                    Err(JudgeError::Aborted) => break 'turn TurnOutcome::Aborted,
                    Err(JudgeError::Unavailable(message)) => {
                        break 'turn TurnOutcome::Failed { message };
                    }
                };
                input_tokens += usage.input_tokens;
                output_tokens += usage.output_tokens;
                let batch = judged_batch(&pending, judgment, role, input_tokens, output_tokens);
                // More than the judgment itself means a follow-up question was asked.
                let asked = batch.len() > 1;
                self.append(session, view.last_seq, batch)?;
                if asked {
                    return Ok(TurnOutcome::AwaitingLearner);
                }
                events = self.store.events(session)?;
                view = project_view(session, &events);
            }
            loop {
                if calls == MAX_CALLS {
                    break TurnOutcome::LoopLimit;
                }
                calls += 1;
                let messages = window(to_llm_messages(&prompts::system(role, &view), &events));
                let request = InferenceRequest {
                    version: sail_protocol::VERSION,
                    request_id: uuid::Uuid::new_v4(),
                    model: None,
                    messages,
                    tools: tools::definitions(role),
                    tool_choice: ToolChoice::Auto,
                    generation: Generation {
                        max_tokens: sail_protocol::ResourceLimits::default().max_output_tokens,
                        temperature: 0.3,
                    },
                };
                let response = tokio::select! {
                    _ = cancel.cancelled() => break TurnOutcome::Aborted,
                    r = self.model.complete(request, cancel.clone()) => r,
                };
                let response = match response {
                    Ok(r) => r,
                    Err(_) if cancel.is_cancelled() => break TurnOutcome::Aborted,
                    Err(message) => break TurnOutcome::Failed { message },
                };
                input_tokens += u64::from(response.usage.input_tokens.unwrap_or(0));
                output_tokens += u64::from(response.usage.output_tokens.unwrap_or(0));
                let text = response
                    .message
                    .content
                    .text()
                    .unwrap_or_default()
                    .to_string();
                // Fresh ids: Mind rejects duplicate tool-call ids anywhere in history.
                let calls_made: Vec<ToolCallRecord> = response
                    .message
                    .tool_calls
                    .iter()
                    .map(|c| ToolCallRecord {
                        id: new_id("call"),
                        name: c.function.name.clone(),
                        arguments: c.function.arguments.clone(),
                    })
                    .collect();
                let mut batch = vec![NewEvent::new(
                    key(),
                    LearningEvent::AssistantMessage {
                        role,
                        text: text.clone(),
                        tool_calls: calls_made.clone(),
                    },
                )];
                let mut asked = false;
                for call in &calls_made {
                    if asked || state.handed_off {
                        batch.push(NewEvent::new(
                            key(),
                            LearningEvent::ToolResult {
                                call_id: call.id.clone(),
                                content: if asked {
                                    "Skipped: a question is already waiting for the learner.".into()
                                } else {
                                    "Skipped: the assessment just ended; the teacher continues."
                                        .into()
                                },
                                ok: false,
                            },
                        ));
                        continue;
                    }
                    let mut effects = Vec::new();
                    match execute(role, call, &mut state, &mut effects) {
                        Ok(effect) => {
                            for event in effects {
                                batch.push(NewEvent::new(key(), event));
                            }
                            match effect {
                                Effect::Continue => {}
                                Effect::Question => asked = true,
                                Effect::Handoff => state.handed_off = true,
                            }
                        }
                        Err(error) => batch.push(NewEvent::new(
                            key(),
                            LearningEvent::ToolResult {
                                call_id: call.id.clone(),
                                content: format!("Error: {error}"),
                                ok: false,
                            },
                        )),
                    }
                }
                let settle = if asked {
                    Some(TurnOutcome::AwaitingLearner)
                } else if calls_made.is_empty() {
                    Some(TurnOutcome::Completed)
                } else {
                    None
                };
                if let Some(outcome) = &settle {
                    batch.push(NewEvent::new(
                        key(),
                        LearningEvent::TurnSettled {
                            role,
                            outcome: outcome.clone(),
                            input_tokens,
                            output_tokens,
                        },
                    ));
                }
                self.append(session, view.last_seq, batch)?;
                if let Some(outcome) = settle {
                    return Ok(outcome);
                }
                events = self.store.events(session)?;
                view = project_view(session, &events);
                if state.handed_off {
                    debug!(session, "assessment finished; teacher takes over");
                    role = AgentRole::Teacher;
                    state.handed_off = false;
                }
            }
        };
        // Aborted, failed or bounded: record it so nothing looks like a successful turn.
        self.append(
            session,
            view.last_seq,
            vec![NewEvent::new(
                key(),
                LearningEvent::TurnSettled {
                    role,
                    outcome: outcome.clone(),
                    input_tokens,
                    output_tokens,
                },
            )],
        )?;
        Ok(outcome)
    }
}

/// The events that record a judgment. For an `unclear` answer to a question that was not
/// itself a follow-up, they also ask the judge's follow-up question with the same rubric and
/// settle the turn; otherwise the tutor continues with the verdict in its history.
fn judged_batch(
    pending: &PendingJudgment,
    mut judgment: crate::core::judgment::Judgment,
    role: AgentRole,
    input_tokens: u64,
    output_tokens: u64,
) -> Vec<NewEvent> {
    let follow_up = judge::wants_follow_up(&judgment) && pending.follow_up_of.is_none();
    if !follow_up {
        judgment.follow_up = None;
    }
    let mut batch = vec![NewEvent::new(
        format!("judgment:{}", pending.call_id),
        LearningEvent::AnswerJudged {
            call_id: pending.call_id.clone(),
            judgment: judgment.clone(),
        },
    )];
    let Some(prompt) = judgment.follow_up.filter(|_| follow_up) else {
        return batch;
    };
    let spec = QuestionSpec {
        kind: QuestionKind::FreeRecall,
        prompt,
        options: Vec::new(),
        correct: Vec::new(),
        node_id: pending.spec.node_id.clone(),
        rubric: pending.spec.rubric.clone(),
    };
    // Recorded as a call of the same tool, so the model's history shows the question it
    // will later see answered. The text is the judge's, asked on the tutor's behalf.
    let call = ToolCallRecord {
        id: new_id("call"),
        name: pending.tool.clone(),
        arguments: serde_json::to_string(&spec).unwrap_or_default(),
    };
    let key = |part: &str| format!("followup:{}:{part}", pending.call_id);
    batch.push(NewEvent::new(
        key("message"),
        LearningEvent::AssistantMessage {
            role,
            text: String::new(),
            tool_calls: vec![call.clone()],
        },
    ));
    batch.push(NewEvent::new(
        key("question"),
        LearningEvent::QuestionAsked {
            call_id: call.id,
            spec,
            follow_up_of: Some(pending.call_id.clone()),
        },
    ));
    batch.push(NewEvent::new(
        key("settled"),
        LearningEvent::TurnSettled {
            role,
            outcome: TurnOutcome::AwaitingLearner,
            input_tokens,
            output_tokens,
        },
    ));
    batch
}

/// Validates and applies one tool call, pushing its events. Errors become tool results.
fn execute(
    role: AgentRole,
    call: &ToolCallRecord,
    state: &mut TurnState,
    out: &mut Vec<LearningEvent>,
) -> Result<Effect, Invalid> {
    let tool = Tool::from_name(&call.name)
        .filter(|t| tools::allowed(role).contains(t))
        .ok_or_else(|| {
            let available: Vec<&str> = tools::allowed(role).iter().map(|t| t.name()).collect();
            Invalid::new(format!(
                "`{}` is not available to the {}. Available: {}",
                call.name,
                tools::role_name(role),
                available.join(", ")
            ))
        })?;
    let schema = tool.schema();
    let ok = |content: String| LearningEvent::ToolResult {
        call_id: call.id.clone(),
        content,
        ok: true,
    };
    let node_in_plan = |node: &str, state: &TurnState| -> Result<(), Invalid> {
        let (_, plan) = state
            .plan
            .as_ref()
            .ok_or_else(|| Invalid::new("there is no lesson plan yet; call plan_init first"))?;
        if !plan.contains(node) {
            let ids: Vec<&str> = plan.nodes.iter().map(|n| n.id.as_str()).collect();
            return Err(Invalid::new(format!(
                "node_id `{node}` is not in the plan. Plan nodes: {}",
                ids.join(", ")
            )));
        }
        Ok(())
    };
    let decode = |raw: &str| -> Result<QuestionSpec, Invalid> {
        let (mut spec, steps) = args::decode::<QuestionSpec>(raw, &schema)?;
        if !steps.is_empty() {
            debug!(tool = tool.name(), ?steps, "healed tool arguments");
        }
        if spec.kind != QuestionKind::FreeRecall {
            // A rubric means nothing for a select question; dropping it costs no retry.
            spec.rubric = None;
        }
        // Questions that check knowledge must say what a good answer contains (#243).
        // `ask_user_question` may ask an open question with no right answer.
        if spec.kind == QuestionKind::FreeRecall
            && tool != Tool::AskUserQuestion
            && spec.rubric.as_deref().is_none_or(|r| r.trim().is_empty())
        {
            return Err(Invalid::new(format!(
                "a free_recall `{}` needs a `rubric`: the idea a good answer must contain (hidden from the learner)",
                tool.name()
            )));
        }
        spec.validate()?;
        Ok(spec)
    };
    match tool {
        Tool::Probe | Tool::AskUserQuestion => {
            let mut spec = decode(&call.arguments)?;
            spec.node_id = None;
            out.push(LearningEvent::QuestionAsked {
                call_id: call.id.clone(),
                spec,
                follow_up_of: None,
            });
            Ok(Effect::Question)
        }
        Tool::Quiz => {
            let spec = decode(&call.arguments)?;
            let node = spec
                .node_id
                .clone()
                .ok_or_else(|| Invalid::new("quiz needs `node_id`"))?;
            node_in_plan(&node, state)?;
            out.push(LearningEvent::ConceptPresented {
                node_id: node,
                kind: PresentKind::Quiz,
                text: spec.prompt.clone(),
            });
            out.push(LearningEvent::QuestionAsked {
                call_id: call.id.clone(),
                spec,
                follow_up_of: None,
            });
            Ok(Effect::Question)
        }
        Tool::Explain => {
            let (a, _) = args::decode::<ExplainArgs>(&call.arguments, &schema)?;
            bounded("text", &a.text, MAX_TEXT)?;
            node_in_plan(&a.node_id, state)?;
            out.push(LearningEvent::ConceptPresented {
                node_id: a.node_id,
                kind: PresentKind::Explain,
                text: a.text,
            });
            out.push(ok("Shown to the learner.".into()));
            Ok(Effect::Continue)
        }
        Tool::PlanInit => {
            let (plan, _) = args::decode::<LessonPlan>(&call.arguments, &schema)?;
            plan.validate()?;
            let revision = state.plan.as_ref().map_or(1, |(r, _)| r + 1);
            let order = plan.order().join(" → ");
            out.push(LearningEvent::PlanUpdated {
                revision,
                plan: plan.clone(),
            });
            out.push(ok(format!(
                "Plan revision {revision} accepted. Teaching order: {order}"
            )));
            state.plan = Some((revision, plan));
            Ok(Effect::Continue)
        }
        Tool::ProbeDone => {
            let (a, _) = args::decode::<ProbeDoneArgs>(&call.arguments, &schema)?;
            bounded("summary", &a.summary, MAX_TEXT)?;
            if a.known.len() > 40 || a.gaps.len() > 40 {
                return Err(Invalid::new(
                    "`known` and `gaps` take at most 40 items each",
                ));
            }
            for item in a.known.iter().chain(&a.gaps) {
                bounded("known/gaps item", item, MAX_SHORT)?;
            }
            out.push(ok("Assessment recorded. The teacher continues.".into()));
            out.push(LearningEvent::ProbeDone {
                summary: a.summary,
                known: a.known,
                gaps: a.gaps,
            });
            Ok(Effect::Handoff)
        }
        Tool::Problem | Tool::Conclude => Err(Invalid::new(
            "practice tools are only available in practice sessions",
        )),
    }
}
