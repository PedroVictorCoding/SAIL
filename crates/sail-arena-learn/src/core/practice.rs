//! Practice session and problem types for Arena Learn.
//!
//! Practice is a distinct phase after teaching, where the learner solves
//! independent transfer problems with delayed feedback, attempts, and hints.

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use chrono::{DateTime, Utc};

/// Unique problem identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProblemId(pub Uuid);

impl ProblemId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Practice problem specification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProblemSpec {
    pub id: ProblemId,
    pub question: String,
    pub model_solution: String,
    pub hints: Vec<String>,
    pub difficulty: u8,
    pub related_concepts: Vec<Uuid>,
}

impl ProblemSpec {
    pub fn new(question: String, model_solution: String) -> Self {
        Self {
            id: ProblemId::new(),
            question,
            model_solution,
            hints: Vec::new(),
            difficulty: 1,
            related_concepts: Vec::new(),
        }
    }

    pub fn add_hint(&mut self, hint: String) {
        self.hints.push(hint);
    }
}

/// Result of a learner practice attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptResult {
    Correct,
    PartiallyCorrect,
    Incorrect,
    Skipped,
}

/// A single learner practice attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PracticeAttempt {
    pub problem_id: ProblemId,
    pub attempt_number: u32,
    pub learner_answer: String,
    pub result: AttemptResult,
    pub feedback: String,
    pub used_hints: u32,
    pub timestamp: DateTime<Utc>,
}

impl PracticeAttempt {
    pub fn new(problem_id: ProblemId, attempt_number: u32, learner_answer: String) -> Self {
        Self {
            problem_id,
            attempt_number,
            learner_answer,
            result: AttemptResult::Incorrect,
            feedback: String::new(),
            used_hints: 0,
            timestamp: Utc::now(),
        }
    }
}

/// Practice session state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PracticeSession {
    pub started_at: DateTime<Utc>,
    pub problems: Vec<ProblemSpec>,
    pub current_problem_index: usize,
    pub attempts: Vec<PracticeAttempt>,
}

impl Default for PracticeSession {
    fn default() -> Self {
        Self {
            started_at: Utc::now(),
            problems: Vec::new(),
            current_problem_index: 0,
            attempts: Vec::new(),
        }
    }
}

impl PracticeSession {
    pub fn add_problem(&mut self, problem: ProblemSpec) {
        self.problems.push(problem);
    }

    pub fn record_attempt(&mut self, attempt: PracticeAttempt) {
        self.attempts.push(attempt);
    }

    pub fn next_problem(&mut self) -> Option<&ProblemSpec> {
        self.current_problem_index += 1;
        self.problems.get(self.current_problem_index)
    }

    pub fn completion_rate(&self) -> f32 {
        if self.problems.is_empty() {
            return 0.0;
        }
        self.attempts.iter()
            .filter(|a| a.result == AttemptResult::Correct)
            .count() as f32 / self.problems.len() as f32
    }
}
