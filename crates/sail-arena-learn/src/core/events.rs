//! The learning event log (#233). Session state is a projection of these events; the
//! browser never owns transcript truth.
use super::judgment::Judgment;
use super::plan::LessonPlan;
use super::practice::{PracticeOutcome, ProblemSpec};
use super::question::{Answer, Grade, QuestionSpec};
use serde::{Deserialize, Serialize};

/// Bumped when an event changes shape. Stores refuse envelopes from a newer version.
/// 2: `AnswerJudged` and `QuestionAsked.follow_up_of` (#243).
/// 3: practice, diagrams, compaction and review scheduling (#244, #252, #255, #257).
pub const SCHEMA_VERSION: u16 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Prober,
    Teacher,
    Practitioner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentKind {
    Explain,
    Quiz,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallRecord {
    pub id: String,
    pub name: String,
    /// Raw arguments as the model sent them (healed copies are not stored).
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnOutcome {
    /// A question is waiting for the learner.
    AwaitingLearner,
    /// The model finished with a message and no question.
    Completed,
    Aborted,
    /// The model was unavailable or failed. Nothing was fabricated in its place.
    Failed {
        message: String,
    },
    /// The tool loop hit its bound.
    LoopLimit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LearningEvent {
    SessionCreated {
        goal: String,
    },
    LearnerMessage {
        text: String,
    },
    AssistantMessage {
        role: AgentRole,
        text: String,
        #[serde(default)]
        tool_calls: Vec<ToolCallRecord>,
    },
    /// The result of a non-question tool call (or of a rejected call).
    ToolResult {
        call_id: String,
        content: String,
        ok: bool,
    },
    /// A question tool call. Its result is the learner's answer, recorded later.
    QuestionAsked {
        call_id: String,
        spec: QuestionSpec,
        /// Set when the engine asked this as the follow-up to an `unclear` judgment of
        /// another question (its call id).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        follow_up_of: Option<String>,
    },
    Answered {
        call_id: String,
        answer: Answer,
        grade: Grade,
    },
    /// The judge's verdict on a free-recall answer (#243). Recorded before the tutor
    /// replies, so the reply and the learner's view agree on it.
    AnswerJudged {
        call_id: String,
        judgment: Judgment,
    },
    ProbeDone {
        summary: String,
        #[serde(default)]
        known: Vec<String>,
        #[serde(default)]
        gaps: Vec<String>,
    },
    PlanUpdated {
        revision: u64,
        plan: LessonPlan,
    },
    ConceptPresented {
        node_id: String,
        kind: PresentKind,
        /// Explanation text for `explain`; the question prompt for `quiz`.
        text: String,
    },
    /// A teacher-requested diagram. The browser renders it with Mermaid strict mode.
    DiagramShown {
        call_id: String,
        node_id: String,
        mermaid: String,
        caption: String,
    },
    /// The browser could not render a diagram; the next turn sees this as a tool error.
    DiagramRenderFailed {
        call_id: String,
        message: String,
    },
    /// Starts a practice-only session. `topic` is both the goal shown to the learner and
    /// the graph topic practice may draw on.
    PracticeStarted {
        topic: String,
    },
    /// A problem tool call. Its result is the learner's attempt, recorded later.
    ProblemAsked {
        call_id: String,
        problem_id: String,
        spec: ProblemSpec,
    },
    /// A learner attempt. Session evidence only; it changes no plan or graph state.
    PracticeAttempted {
        problem_id: String,
        text: String,
    },
    /// The learner asked for the hint at `index` (0-based). Recorded per problem.
    HintRequested {
        problem_id: String,
        index: u32,
    },
    /// The practitioner concluded a problem; this reveals its solution.
    PracticeConcluded {
        problem_id: String,
        outcome: PracticeOutcome,
    },
    /// The learner cancelled the opt-in review reminder (#257).
    PracticeReviewCancelled,
    /// Opt-in spaced practice reminder, scheduled through Pulse (#257).
    PracticeReviewScheduled {
        problem_id: Option<String>,
        days: u8,
        due_at_ms: i64,
        reminder_id: String,
    },
    /// A compaction cut (#252). Events up to `up_to_seq` are represented by `digest`.
    ContextCompacted {
        up_to_seq: u64,
        digest: String,
    },
    TurnSettled {
        role: AgentRole,
        outcome: TurnOutcome,
        input_tokens: u64,
        output_tokens: u64,
    },
    Paused,
    Resumed,
}

impl LearningEvent {
    /// Events after which the model owes the learner a turn.
    pub fn is_input(&self) -> bool {
        matches!(
            self,
            Self::SessionCreated { .. }
                | Self::LearnerMessage { .. }
                | Self::Answered { .. }
                | Self::ProbeDone { .. }
                | Self::PracticeStarted { .. }
                | Self::PracticeAttempted { .. }
                | Self::PracticeConcluded { .. }
                | Self::Resumed
        )
    }
}

/// One stored event. `key` is an idempotency key: appending the same key twice is a no-op.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub session: String,
    pub seq: u64,
    pub at_ms: i64,
    pub key: String,
    pub version: u16,
    pub event: LearningEvent,
}

/// An event waiting to be appended; the store assigns `seq`.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub key: String,
    pub event: LearningEvent,
}

impl NewEvent {
    pub fn new(key: impl Into<String>, event: LearningEvent) -> Self {
        Self {
            key: key.into(),
            event,
        }
    }
}
