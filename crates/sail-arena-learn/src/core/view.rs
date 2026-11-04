//! The session view: a pure projection of the event log (#233), including teaching
//! progress from node-tagged `explain`/`quiz` calls (#242). Visited never means understood,
//! and the view has no mastery or score field.
use super::events::{AgentRole, Envelope, LearningEvent, PresentKind, TurnOutcome};
use super::judgment::Judgment;
use super::plan::LessonPlan;
use super::question::{Answer, Grade, QuestionKind, QuestionOption, QuestionSpec};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Assess,
    Teach,
}

/// A question as the learner sees it. `correct` appears only after it is answered; a
/// free-recall rubric never appears.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LearnerQuestion {
    pub id: String,
    pub kind: QuestionKind,
    pub prompt: String,
    pub options: Vec<QuestionOption>,
    pub node_id: Option<String>,
    pub answer: Option<Answer>,
    pub grade: Option<Grade>,
    pub correct: Option<Vec<String>>,
    /// The judge's verdict on a free-recall answer, once recorded (#243).
    pub judgment: Option<Judgment>,
    /// Set when this question is the follow-up to an unclear answer to that question.
    pub follow_up_of: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entry {
    Learner { text: String },
    Assistant { text: String },
    Explain { node_id: String, text: String },
    Question(Box<LearnerQuestion>),
    Notice { text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanView {
    pub revision: u64,
    pub goal: String,
    pub nodes: Vec<super::plan::PlanNode>,
    pub edges: Vec<[String; 2]>,
    pub order: Vec<String>,
    pub mermaid: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Progress {
    pub current: Option<String>,
    pub visited: Vec<String>,
    pub upcoming: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProbeSummary {
    pub summary: String,
    pub known: Vec<String>,
    pub gaps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionView {
    pub session: String,
    pub goal: String,
    pub mode: Mode,
    pub paused: bool,
    pub transcript: Vec<Entry>,
    pub plan: Option<PlanView>,
    pub progress: Progress,
    pub probe: Option<ProbeSummary>,
    /// The one unanswered question, if any.
    pub active_question: Option<String>,
    /// True when the model owes the learner a turn (also after a crash mid-turn).
    pub needs_turn: bool,
    pub last_failure: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub last_seq: u64,
}

impl SessionView {
    pub fn empty(session: &str) -> Self {
        Self {
            session: session.to_string(),
            goal: String::new(),
            mode: Mode::Assess,
            paused: false,
            transcript: Vec::new(),
            plan: None,
            progress: Progress::default(),
            probe: None,
            active_question: None,
            needs_turn: false,
            last_failure: None,
            input_tokens: 0,
            output_tokens: 0,
            last_seq: 0,
        }
    }

    /// The role that takes the next model turn.
    pub fn role(&self) -> AgentRole {
        match self.mode {
            Mode::Assess => AgentRole::Prober,
            Mode::Teach => AgentRole::Teacher,
        }
    }

    pub fn current_plan(&self) -> Option<&PlanView> {
        self.plan.as_ref()
    }

    fn question_mut(&mut self, id: &str) -> Option<&mut LearnerQuestion> {
        self.transcript.iter_mut().rev().find_map(|e| match e {
            Entry::Question(q) if q.id == id => Some(q.as_mut()),
            _ => None,
        })
    }

    fn recompute_upcoming(&mut self) {
        self.progress.upcoming = self
            .plan
            .as_ref()
            .map(|p| {
                p.order
                    .iter()
                    .filter(|id| !self.progress.visited.contains(id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
    }

    /// Applies one event. `specs` keeps the hidden answer keys for grading display.
    pub fn apply(&mut self, envelope: &Envelope, specs: &mut Vec<(String, QuestionSpec)>) {
        self.last_seq = envelope.seq;
        let event = &envelope.event;
        if event.is_input() {
            self.needs_turn = true;
        }
        match event {
            LearningEvent::SessionCreated { goal } => self.goal = goal.clone(),
            LearningEvent::LearnerMessage { text } => {
                self.transcript.push(Entry::Learner { text: text.clone() })
            }
            LearningEvent::AssistantMessage { text, .. } => {
                if !text.trim().is_empty() {
                    self.transcript
                        .push(Entry::Assistant { text: text.clone() });
                }
            }
            LearningEvent::ToolResult { .. } => {}
            LearningEvent::QuestionAsked {
                call_id,
                spec,
                follow_up_of,
            } => {
                specs.push((call_id.clone(), spec.clone()));
                self.active_question = Some(call_id.clone());
                self.transcript
                    .push(Entry::Question(Box::new(LearnerQuestion {
                        id: call_id.clone(),
                        kind: spec.kind,
                        prompt: spec.prompt.clone(),
                        options: spec.options.clone(),
                        node_id: spec.node_id.clone(),
                        answer: None,
                        grade: None,
                        correct: None,
                        judgment: None,
                        follow_up_of: follow_up_of.clone(),
                    })));
            }
            LearningEvent::Answered {
                call_id,
                answer,
                grade,
            } => {
                if self.active_question.as_deref() == Some(call_id) {
                    self.active_question = None;
                }
                let correct = specs
                    .iter()
                    .find(|(id, _)| id == call_id)
                    .map(|(_, s)| s.correct.clone())
                    .filter(|c| !c.is_empty());
                if let Some(q) = self.question_mut(call_id) {
                    q.answer = Some(answer.clone());
                    q.grade = Some(*grade);
                    q.correct = correct;
                }
            }
            LearningEvent::AnswerJudged { call_id, judgment } => {
                if let Some(q) = self.question_mut(call_id) {
                    q.judgment = Some(judgment.clone());
                }
            }
            LearningEvent::ProbeDone {
                summary,
                known,
                gaps,
            } => {
                self.mode = Mode::Teach;
                self.probe = Some(ProbeSummary {
                    summary: summary.clone(),
                    known: known.clone(),
                    gaps: gaps.clone(),
                });
                self.transcript.push(Entry::Notice {
                    text: format!("Assessment finished: {summary}"),
                });
            }
            LearningEvent::PlanUpdated { revision, plan } => {
                self.set_plan(*revision, plan);
                self.transcript.push(Entry::Notice {
                    text: format!(
                        "Lesson plan revision {revision}: {} steps",
                        plan.nodes.len()
                    ),
                });
            }
            LearningEvent::ConceptPresented {
                node_id,
                kind,
                text,
            } => {
                self.progress.current = Some(node_id.clone());
                if !self.progress.visited.contains(node_id) {
                    self.progress.visited.push(node_id.clone());
                }
                self.recompute_upcoming();
                if *kind == PresentKind::Explain {
                    self.transcript.push(Entry::Explain {
                        node_id: node_id.clone(),
                        text: text.clone(),
                    });
                }
            }
            LearningEvent::TurnSettled {
                outcome,
                input_tokens,
                output_tokens,
                ..
            } => {
                self.needs_turn = false;
                self.input_tokens += input_tokens;
                self.output_tokens += output_tokens;
                self.last_failure = match outcome {
                    TurnOutcome::Failed { message } => Some(message.clone()),
                    TurnOutcome::LoopLimit => {
                        Some("The tutor stopped after too many steps; try again.".into())
                    }
                    _ => None,
                };
            }
            LearningEvent::Paused => self.paused = true,
            LearningEvent::Resumed => self.paused = false,
        }
        if self.paused || self.active_question.is_some() {
            self.needs_turn = false;
        }
    }

    fn set_plan(&mut self, revision: u64, plan: &LessonPlan) {
        // Visited nodes survive a re-plan only if their ids do (#242).
        self.progress.visited.retain(|id| plan.contains(id));
        if let Some(current) = &self.progress.current
            && !plan.contains(current)
        {
            self.progress.current = None;
        }
        self.plan = Some(PlanView {
            revision,
            goal: plan.goal.clone(),
            nodes: plan.nodes.clone(),
            edges: plan.edges.clone(),
            order: plan.order(),
            mermaid: plan.to_mermaid(),
        });
        self.recompute_upcoming();
    }
}

/// Folds the whole log. Deterministic: the same events always give the same view.
pub fn project_view(session: &str, events: &[Envelope]) -> SessionView {
    let mut view = SessionView::empty(session);
    let mut specs = Vec::new();
    for envelope in events {
        view.apply(envelope, &mut specs);
    }
    view
}

/// The latest accepted plan, for the agent layer.
pub fn latest_plan(events: &[Envelope]) -> Option<(u64, LessonPlan)> {
    events.iter().rev().find_map(|e| match &e.event {
        LearningEvent::PlanUpdated { revision, plan } => Some((*revision, plan.clone())),
        _ => None,
    })
}

/// The spec of a question by call id, for grading.
pub fn question_spec<'a>(events: &'a [Envelope], call_id: &str) -> Option<&'a QuestionSpec> {
    events.iter().find_map(|e| match &e.event {
        LearningEvent::QuestionAsked {
            call_id: id, spec, ..
        } if id == call_id => Some(spec),
        _ => None,
    })
}

/// A free-recall answer that still needs judging before the tutor replies.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingJudgment {
    pub call_id: String,
    /// The tool that asked the question (`probe`, `quiz` or `ask_user_question`).
    pub tool: String,
    pub spec: QuestionSpec,
    pub answer: Answer,
    /// Set when the answered question was itself a follow-up; no second follow-up is asked.
    pub follow_up_of: Option<String>,
}

/// The latest answer, if it is a free-recall answer with a rubric that has not been judged.
/// Answers without a rubric (open clarifying questions, or sessions from before #243) are
/// never judged: there is nothing to judge them against.
pub fn pending_judgment(events: &[Envelope]) -> Option<PendingJudgment> {
    let (call_id, answer) = events
        .iter()
        .rev()
        .find_map(|e| match &e.event {
            LearningEvent::Answered {
                call_id,
                answer,
                grade,
            } => Some((call_id, answer, *grade)),
            _ => None,
        })
        .filter(|(_, _, grade)| *grade == Grade::NeedsJudgment)
        .map(|(call_id, answer, _)| (call_id, answer))?;
    if judgment_of(events, call_id).is_some() {
        return None;
    }
    let (spec, follow_up_of) = events.iter().find_map(|e| match &e.event {
        LearningEvent::QuestionAsked {
            call_id: id,
            spec,
            follow_up_of,
        } if id == call_id => Some((spec, follow_up_of)),
        _ => None,
    })?;
    spec.rubric.as_ref()?;
    let tool = events.iter().find_map(|e| match &e.event {
        LearningEvent::AssistantMessage { tool_calls, .. } => tool_calls
            .iter()
            .find(|c| &c.id == call_id)
            .map(|c| c.name.clone()),
        _ => None,
    })?;
    Some(PendingJudgment {
        call_id: call_id.clone(),
        tool,
        spec: spec.clone(),
        answer: answer.clone(),
        follow_up_of: follow_up_of.clone(),
    })
}

/// The recorded judgment of a question, if any.
pub fn judgment_of<'a>(events: &'a [Envelope], call_id: &str) -> Option<&'a Judgment> {
    events.iter().find_map(|e| match &e.event {
        LearningEvent::AnswerJudged {
            call_id: id,
            judgment,
        } if id == call_id => Some(judgment),
        _ => None,
    })
}

/// The recorded answer for a question, if any.
pub fn recorded_answer(events: &[Envelope], call_id: &str) -> Option<(Answer, Grade)> {
    events.iter().find_map(|e| match &e.event {
        LearningEvent::Answered {
            call_id: id,
            answer,
            grade,
        } if id == call_id => Some((answer.clone(), *grade)),
        _ => None,
    })
}
