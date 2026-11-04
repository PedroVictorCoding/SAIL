//! Persistent learning graph (propositions, chains, evidence).
//!
//! The learning graph stores accumulated knowledge from all learning sessions,
//! not just a single plan. It consists of propositions (atomic claims) connected
//! by chains (reasoning steps) and attributed to evidence sources.

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use chrono::{DateTime, Utc};

/// Unique proposition identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PropId(pub Uuid);

impl PropId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for PropId {
    fn default() -> Self {
        Self::new()
    }
}

/// Confidence rating for a proposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Uncertain,
    Tentative,
    Confident,
    Proven,
}

/// Atomic proposition: a single verifiable claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposition {
    pub id: PropId,
    pub statement: String,
    pub created_at: DateTime<Utc>,
    pub confidence: Confidence,
    pub evidence: Vec<String>,
    pub session_ids: Vec<Uuid>,
    pub error_count: u32,
    pub correct_count: u32,
}

impl Proposition {
    pub fn new(statement: String) -> Self {
        Self {
            id: PropId::new(),
            statement,
            created_at: Utc::now(),
            confidence: Confidence::Tentative,
            evidence: Vec::new(),
            session_ids: Vec::new(),
            error_count: 0,
            correct_count: 0,
        }
    }

    pub fn record_correct(&mut self) {
        self.correct_count += 1;
        self.update_confidence();
    }

    pub fn record_error(&mut self) {
        self.error_count += 1;
        self.update_confidence();
    }

    fn update_confidence(&mut self) {
        let total = self.correct_count + self.error_count;
        if total == 0 {
            self.confidence = Confidence::Tentative;
            return;
        }
        let accuracy = self.correct_count as f32 / total as f32;
        self.confidence = if accuracy > 0.9 {
            Confidence::Proven
        } else if accuracy > 0.7 {
            Confidence::Confident
        } else if accuracy > 0.5 {
            Confidence::Tentative
        } else {
            Confidence::Uncertain
        };
    }
}

/// Unique chain identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChainId(pub Uuid);

impl ChainId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

/// Chain: a reasoning step connecting propositions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chain {
    pub id: ChainId,
    pub from: PropId,
    pub to: PropId,
    pub reasoning: String,
    pub created_at: DateTime<Utc>,
    pub strength: f32,
}

impl Chain {
    pub fn new(from: PropId, to: PropId, reasoning: String) -> Self {
        Self {
            id: ChainId::new(),
            from,
            to,
            reasoning,
            created_at: Utc::now(),
            strength: 1.0,
        }
    }
}

/// Summary of the learning graph for UI display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct GraphSummary {
    pub total_propositions: usize,
    pub total_chains: usize,
    pub proven_count: usize,
    pub confident_count: usize,
    pub tentative_count: usize,
    pub uncertain_count: usize,
}

impl From<&[Proposition]> for GraphSummary {
    fn from(props: &[Proposition]) -> Self {
        let mut summary = GraphSummary {
            total_propositions: props.len(),
            ..Default::default()
        };
        for p in props {
            match p.confidence {
                Confidence::Proven => summary.proven_count += 1,
                Confidence::Confident => summary.confident_count += 1,
                Confidence::Tentative => summary.tentative_count += 1,
                Confidence::Uncertain => summary.uncertain_count += 1,
            }
        }
        summary
    }
}
