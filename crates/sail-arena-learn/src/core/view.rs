//! The session view: a pure projection of the event log (#233), including teaching
//! progress from node-tagged `explain`/`quiz` calls (#242). Visited never means understood,
//! and the view has no mastery or score field.
use super::artifacts::Artifact;
use super::events::{AgentRole, Envelope, LearningEvent, PresentKind, TurnOutcome};
use super::graph::{
    GraphContextProp, GraphProposition, GraphProvenance, GraphReasoningChain, GraphSummary,
};
use super::judgment::Judgment;
use super::plan::LessonPlan;
use super::practice::{PracticeView, ProblemSpec};
use super::question::{Answer, Grade, QuestionKind, QuestionOption, QuestionSpec};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Assess,
    Teach,
    Practice,
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
    /// Originating tool (`probe`, `recall`, `quiz`, `explain`, `ask_user_question`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Subtype for quizzes (`predict` vs `check`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question_type: Option<String>,
    /// Supporting context or code snippet below the prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    /// Explanation revealed after the question is answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dismissed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entry {
    Learner {
        text: String,
    },
    Assistant {
        text: String,
    },
    Explain {
        node_id: String,
        text: String,
    },
    Question(Box<LearnerQuestion>),
    Diagram {
        call_id: String,
        node_id: String,
        mermaid: String,
        caption: String,
        error: Option<String>,
    },
    Problem {
        problem_id: String,
        statement: String,
    },
    Notice {
        text: String,
    },
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

/// Per-turn token usage record for the session stats popover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TurnStat {
    pub turn: u32,
    pub role: AgentRole,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionView {
    pub session: String,
    pub title: String,
    pub goal: String,
    pub kind: String,
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
    pub created_ms: i64,
    pub updated_ms: i64,
    /// Session learning artifacts generated during this session.
    pub artifacts: Vec<Artifact>,
    /// Persistent learning graph summary.
    pub graph_summary: Option<GraphSummary>,
    /// Propositions passed as initial context when starting this session.
    pub graph_context: Vec<GraphContextProp>,
    /// Per-turn token usage breakdown.
    pub turns: Vec<TurnStat>,
    /// Practice state when this is a practice session (#244).
    pub practice: Option<PracticeView>,
    /// The latest opted-in review reminder (#257).
    pub review: Option<ReviewScheduled>,
    /// Approximate share of the model context budget in use (0..=1).
    pub context_fraction: f32,
    /// Prompt and completion tokens for this session.
    pub context_budget_tokens: u64,
    /// Approximate prompt-history tokens; updated incrementally with the event log.
    #[serde(skip)]
    pub(crate) context_tokens: u64,
    /// Hidden problem specs, never serialized: solutions and hints until revealed.
    #[serde(skip)]
    pub(crate) problem_specs: Vec<(String, ProblemSpec)>,
}

/// Prompt budget used for the meter. The request window keeps the actual request smaller.
pub const CONTEXT_BUDGET_TOKENS: u64 = 24_000;

fn approx_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewScheduled {
    pub days: u8,
    pub due_at_ms: i64,
    pub reminder_id: String,
    pub problem_id: Option<String>,
}

impl SessionView {
    pub fn empty(session: &str) -> Self {
        Self {
            session: session.to_string(),
            title: String::new(),
            goal: String::new(),
            kind: "teach".to_string(),
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
            created_ms: 0,
            updated_ms: 0,
            artifacts: Vec::new(),
            graph_summary: None,
            graph_context: Vec::new(),
            turns: Vec::new(),
            practice: None,
            review: None,
            context_fraction: 0.0,
            context_budget_tokens: CONTEXT_BUDGET_TOKENS,
            context_tokens: 0,
            problem_specs: Vec::new(),
        }
    }

    /// The role that takes the next model turn.
    pub fn role(&self) -> AgentRole {
        match self.mode {
            Mode::Assess => AgentRole::Prober,
            Mode::Teach => AgentRole::Teacher,
            Mode::Practice => AgentRole::Practitioner,
        }
    }

    pub fn current_plan(&self) -> Option<&PlanView> {
        self.plan.as_ref()
    }

    pub fn active_problem(&self) -> Option<&str> {
        self.practice
            .as_ref()
            .and_then(|p| p.active_problem.as_deref())
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
        if self.created_ms == 0 && envelope.at_ms > 0 {
            self.created_ms = envelope.at_ms;
        }
        if envelope.at_ms > 0 {
            self.updated_ms = envelope.at_ms;
        }
        let event = &envelope.event;
        self.context_tokens = self.context_tokens.saturating_add(approx_tokens(
            &serde_json::to_string(event).unwrap_or_default(),
        ));
        self.context_fraction =
            (self.context_tokens as f32 / CONTEXT_BUDGET_TOKENS as f32).min(1.0);
        if event.is_input() {
            self.needs_turn = true;
        }
        match event {
            LearningEvent::SessionCreated { goal } => {
                self.goal = goal.clone();
                if self.title.is_empty() {
                    self.title = goal.clone();
                }
            }
            LearningEvent::SessionRenamed { title } => {
                self.title = title.clone();
            }
            LearningEvent::GraphContextAttached { propositions } => {
                self.graph_context = propositions.clone();
            }
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
                        tool: spec.tool.clone(),
                        question_type: spec.question_type.clone(),
                        details: spec.details.clone(),
                        explanation: None,
                        dismissed: false,
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
                let matched_spec = specs.iter().find(|(id, _)| id == call_id).map(|(_, s)| s);
                let correct = matched_spec
                    .map(|s| s.correct.clone())
                    .filter(|c| !c.is_empty());
                let explanation = matched_spec.and_then(|s| s.explanation.clone());
                if let Some(q) = self.question_mut(call_id) {
                    q.answer = Some(answer.clone());
                    q.grade = Some(*grade);
                    q.correct = correct;
                    q.explanation = explanation;
                }
            }
            LearningEvent::QuestionDismissed { call_id } => {
                if self.active_question.as_deref() == Some(call_id) {
                    self.active_question = None;
                }
                if let Some(q) = self.question_mut(call_id) {
                    q.dismissed = true;
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
            LearningEvent::DiagramShown {
                call_id,
                node_id,
                mermaid,
                caption,
            } => self.transcript.push(Entry::Diagram {
                call_id: call_id.clone(),
                node_id: node_id.clone(),
                mermaid: mermaid.clone(),
                caption: caption.clone(),
                error: None,
            }),
            LearningEvent::DiagramRenderFailed { call_id, message } => {
                if let Some(Entry::Diagram { error, .. }) = self
                    .transcript
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, Entry::Diagram { call_id: id, .. } if id == call_id))
                {
                    *error = Some(message.clone());
                }
            }
            LearningEvent::PracticeStarted { topic } => {
                self.mode = Mode::Practice;
                self.kind = "practice".to_string();
                self.goal = topic.clone();
                if self.title.is_empty() {
                    self.title = topic.clone();
                }
                self.practice = Some(PracticeView::default());
                self.transcript.push(Entry::Notice {
                    text: format!("Practice session: {topic}"),
                });
            }
            LearningEvent::ProblemAsked {
                problem_id, spec, ..
            } => {
                let practice = self.practice.get_or_insert_with(PracticeView::default);
                practice.active_problem = Some(problem_id.clone());
                practice.problems.push(super::practice::ProblemView {
                    id: problem_id.clone(),
                    statement: spec.statement.clone(),
                    hints: Vec::new(),
                    hint_count: spec.hints.len(),
                    solution: None,
                    outcome: None,
                    attempts: Vec::new(),
                    mode: spec.mode.clone(),
                    target: spec.target.clone(),
                    verdict: None,
                    rung: None,
                    miss_class: None,
                    retest: None,
                });
                self.problem_specs.push((problem_id.clone(), spec.clone()));
                self.transcript.push(Entry::Problem {
                    problem_id: problem_id.clone(),
                    statement: spec.statement.clone(),
                });
            }
            LearningEvent::PracticeAttempted { problem_id, text } => {
                if let Some(practice) = self.practice.as_mut()
                    && let Some(problem) = practice.problem_mut(problem_id)
                {
                    problem.attempts.push(text.clone());
                }
            }
            LearningEvent::HintRequested { problem_id, index } => {
                let hint = self
                    .problem_specs
                    .iter()
                    .find(|(id, _)| id == problem_id)
                    .and_then(|(_, spec)| spec.hints.get(*index as usize))
                    .cloned();
                if let Some(hint) = hint
                    && let Some(practice) = self.practice.as_mut()
                    && let Some(problem) = practice.problem_mut(problem_id)
                    && !problem.hints.contains(&hint)
                {
                    problem.hints.push(hint);
                }
            }
            LearningEvent::PracticeConcluded {
                problem_id,
                outcome,
                verdict,
                rung,
                miss_class,
                retest,
            } => {
                let solution = self
                    .problem_specs
                    .iter()
                    .find(|(id, _)| id == problem_id)
                    .map(|(_, spec)| spec.solution.clone());
                if let Some(practice) = self.practice.as_mut() {
                    if let Some(problem) = practice.problem_mut(problem_id) {
                        problem.solution = solution;
                        problem.outcome = Some(*outcome);
                        problem.verdict = verdict.clone();
                        problem.rung = *rung;
                        problem.miss_class = miss_class.clone();
                        problem.retest = retest.clone();
                    }
                    if practice.active_problem.as_deref() == Some(problem_id) {
                        practice.active_problem = None;
                    }
                    practice.last_outcome = Some(*outcome);
                }
            }
            LearningEvent::PracticeReviewScheduled {
                problem_id,
                days,
                due_at_ms,
                reminder_id,
            } => {
                self.review = Some(ReviewScheduled {
                    days: *days,
                    due_at_ms: *due_at_ms,
                    reminder_id: reminder_id.clone(),
                    problem_id: problem_id.clone(),
                });
            }
            LearningEvent::PracticeReviewCancelled => self.review = None,
            LearningEvent::ContextCompacted { .. } => {}
            LearningEvent::TurnSettled {
                role,
                outcome,
                input_tokens,
                output_tokens,
            } => {
                self.needs_turn = false;
                self.input_tokens += input_tokens;
                self.output_tokens += output_tokens;
                let turn_num = self.turns.len() as u32 + 1;
                self.turns.push(TurnStat {
                    turn: turn_num,
                    role: *role,
                    input_tokens: *input_tokens,
                    output_tokens: *output_tokens,
                    at_ms: envelope.at_ms,
                });
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
        let practice_open = self
            .practice
            .as_ref()
            .is_some_and(|p| p.active_problem.is_some());
        let attempt_arrived = matches!(event, LearningEvent::PracticeAttempted { .. });
        if self.paused || self.active_question.is_some() || (practice_open && !attempt_arrived) {
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

    /// Exports the session transcript, plan, questions, and practice problems to Markdown.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        let heading = if self.title.trim().is_empty() {
            &self.goal
        } else {
            &self.title
        };
        md.push_str(&format!("# {heading}\n\n"));
        md.push_str(&format!("- **Goal**: {}\n", self.goal));
        md.push_str(&format!("- **Kind**: {}\n", self.kind));
        md.push_str(&format!("- **Phase**: {:?}\n\n", self.mode));

        if let Some(plan) = &self.plan {
            md.push_str("## Lesson Plan\n\n");
            for id in &plan.order {
                if let Some(node) = plan.nodes.iter().find(|n| &n.id == id) {
                    let mark = if self.progress.visited.contains(id) {
                        "x"
                    } else {
                        " "
                    };
                    md.push_str(&format!("- [{mark}] **{}** (`{}`)\n", node.title, node.id));
                }
            }
            md.push('\n');
        }

        md.push_str("## Transcript\n\n");
        for entry in &self.transcript {
            match entry {
                Entry::Learner { text } => {
                    md.push_str(&format!("### Learner\n\n{text}\n\n"));
                }
                Entry::Assistant { text } => {
                    md.push_str(&format!("### Tutor\n\n{text}\n\n"));
                }
                Entry::Explain { node_id, text } => {
                    md.push_str(&format!("### Explanation (`{node_id}`)\n\n{text}\n\n"));
                }
                Entry::Diagram {
                    node_id,
                    mermaid,
                    caption,
                    ..
                } => {
                    md.push_str(&format!(
                        "### Diagram: {caption} (`{node_id}`)\n\n```mermaid\n{mermaid}\n```\n\n"
                    ));
                }
                Entry::Question(q) => {
                    let label = q.tool.as_deref().unwrap_or("question").to_uppercase();
                    md.push_str(&format!("### {label}: {}\n\n", q.prompt));
                    for opt in &q.options {
                        let chosen = q
                            .answer
                            .as_ref()
                            .is_some_and(|a| a.choices.contains(&opt.id));
                        let bullet = if chosen { "- [x]" } else { "- [ ]" };
                        md.push_str(&format!("{bullet} {}\n", opt.text));
                    }
                    if let Some(ans) = &q.answer {
                        if let Some(t) = &ans.text {
                            md.push_str(&format!("\n**Answer**: {t}\n"));
                        }
                        if let Some(th) = &ans.thinking {
                            md.push_str(&format!("\n> {th}\n"));
                        }
                    }
                    md.push('\n');
                }
                Entry::Problem {
                    problem_id,
                    statement,
                } => {
                    md.push_str(&format!("### Problem (`{problem_id}`)\n\n{statement}\n\n"));
                    if let Some(practice) = &self.practice
                        && let Some(prob) = practice.problems.iter().find(|p| p.id == *problem_id)
                    {
                        for attempt in &prob.attempts {
                            md.push_str(&format!("**Thinking**:\n> {attempt}\n\n"));
                        }
                        if let Some(outcome) = &prob.outcome {
                            md.push_str(&format!("**Outcome**: `{outcome:?}`\n\n"));
                        }
                        if let Some(sol) = &prob.solution {
                            md.push_str(&format!("**Model solution**:\n{sol}\n\n"));
                        }
                    }
                }
                Entry::Notice { text } => {
                    md.push_str(&format!("*{text}*\n\n"));
                }
            }
        }
        md
    }

    /// Extracts propositions and reasoning chains from this session's plan and explanations.
    pub fn extract_graph_items(
        &self,
        at_ms: i64,
    ) -> (Vec<GraphProposition>, Vec<GraphReasoningChain>) {
        let mut props = Vec::new();
        let mut chains = Vec::new();
        let label = if self.title.trim().is_empty() {
            self.goal.clone()
        } else {
            self.title.clone()
        };
        let prov = GraphProvenance {
            kind: "teaching".into(),
            session_id: Some(self.session.clone()),
            file_name: None,
            label: Some(label.clone()),
            created_at: at_ms,
        };

        if let Some(plan) = &self.plan {
            let mut node_name_by_id = std::collections::BTreeMap::new();
            for node in &plan.nodes {
                let name = node.title.trim().to_lowercase();
                node_name_by_id.insert(node.id.clone(), name.clone());
                let explained = self.transcript.iter().rev().find_map(|e| match e {
                    Entry::Explain { node_id, text } if node_id == &node.id => Some(text.clone()),
                    _ => None,
                });
                let statement =
                    explained.unwrap_or_else(|| format!("{} (part of {})", node.title, self.goal));
                props.push(GraphProposition {
                    id: format!("prop-{}-{}", self.session, node.id),
                    name,
                    statement,
                    aliases: vec![node.id.clone()],
                    created_at: at_ms,
                    provenance: vec![prov.clone()],
                });
            }
            for [from, to] in &plan.edges {
                if let (Some(p_name), Some(c_name)) =
                    (node_name_by_id.get(from), node_name_by_id.get(to))
                {
                    chains.push(GraphReasoningChain {
                        id: format!("chain-{}-{}-{}", self.session, from, to),
                        premises: vec![p_name.clone()],
                        conclusion: c_name.clone(),
                        created_at: at_ms,
                        source_id: self.session.clone(),
                        source_kind: "teaching".into(),
                        source_label: label.clone(),
                    });
                }
            }
        } else {
            for (idx, entry) in self.transcript.iter().enumerate() {
                if let Entry::Explain { node_id, text } = entry {
                    let name = node_id.replace('_', " ").to_lowercase();
                    props.push(GraphProposition {
                        id: format!("prop-{}-{}", self.session, idx),
                        name,
                        statement: text.clone(),
                        aliases: Vec::new(),
                        created_at: at_ms,
                        provenance: vec![prov.clone()],
                    });
                }
            }
            for pair in props.windows(2) {
                chains.push(GraphReasoningChain {
                    id: format!("chain-{}-{}", self.session, uuid::Uuid::new_v4()),
                    premises: vec![pair[0].name.clone()],
                    conclusion: pair[1].name.clone(),
                    created_at: at_ms,
                    source_id: self.session.clone(),
                    source_kind: "teaching".into(),
                    source_label: label.clone(),
                });
            }
        }
        (props, chains)
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
