//! Practice sessions (#244): independent retrieval and transfer after teaching.
//!
//! A practice session is a session kind, not more quiz inside a lesson. The `problem` tool
//! commits the statement and its solution together; the solution is stored but never shown
//! to the learner (or in a learner-facing view) until `conclude`. Only one problem is active
//! at a time. Hints are requested by the learner and recorded per problem. Outcomes are
//! session evidence only: they never change plan progress or the learning graph.
use super::{Invalid, MAX_TEXT, bounded};
use serde::{Deserialize, Serialize};

/// Hints a problem may carry.
pub const MAX_HINTS: usize = 5;
/// Longest one hint, in bytes.
pub const MAX_HINT: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProblemSpec {
    pub statement: String,
    /// Stored in the event log, hidden from the learner until conclusion.
    pub solution: String,
    #[serde(default)]
    pub hints: Vec<String>,
    /// Optional plan step this problem practises; it does not change plan progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

impl ProblemSpec {
    pub fn validate(&self) -> Result<(), Invalid> {
        bounded("statement", &self.statement, MAX_TEXT)?;
        bounded("solution", &self.solution, MAX_TEXT)?;
        if self.hints.len() > MAX_HINTS {
            return Err(Invalid::new(format!(
                "a problem takes at most {MAX_HINTS} hints"
            )));
        }
        for hint in &self.hints {
            bounded("hints[]", hint, MAX_HINT)?;
        }
        Ok(())
    }
}

/// How a practice problem ended, as reported by the practitioner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PracticeOutcome {
    Solved,
    SolvedWithHints,
    NotSolved,
}

/// One learner attempt, kept as session evidence only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PracticeAttempt {
    pub problem_id: String,
    pub text: String,
}

/// A problem as the learner may see it. Unrevealed hints and the solution are absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProblemView {
    pub id: String,
    pub statement: String,
    /// Hints revealed so far. Unrevealed text is not present in any learner-facing view.
    pub hints: Vec<String>,
    /// How many hints exist (never leaks their text).
    pub hint_count: usize,
    pub solution: Option<String>,
    pub outcome: Option<PracticeOutcome>,
    pub attempts: Vec<String>,
}

/// Practice progress for one session. There is no score or mastery field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct PracticeView {
    pub problems: Vec<ProblemView>,
    pub active_problem: Option<String>,
    pub last_outcome: Option<PracticeOutcome>,
}

impl PracticeView {
    pub fn active(&self) -> Option<&ProblemView> {
        let id = self.active_problem.as_deref()?;
        self.problems.iter().find(|p| p.id == id)
    }

    pub fn problem_mut(&mut self, id: &str) -> Option<&mut ProblemView> {
        self.problems.iter_mut().find(|p| p.id == id)
    }
}
