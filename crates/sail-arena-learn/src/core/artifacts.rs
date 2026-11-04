//! Artifact types for Arena Learn.
//!
//! Artifacts are persistent, reusable outputs generated during learning sessions:
//! explanations, diagrams, problem sets, extracted notes, proof-of-concept code.
//! Each artifact carries provenance, validity range, and usage statistics.

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use chrono::{DateTime, Utc};

/// Unique artifact identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactId(pub Uuid);

impl ArtifactId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ArtifactId {
    fn default() -> Self {
        Self::new()
    }
}

/// Type of artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// Text explanation of a concept.
    Explanation,
    /// Visual diagram or illustration.
    Diagram,
    /// Practice problem specification.
    Problem,
    /// Extracted note from user import or session.
    Note,
    /// Working code example.
    CodeExample,
    /// Research brief summary.
    ResearchBrief,
}

/// Artifact validation status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    /// Artifact is unvalidated.
    Unverified,
    /// Artifact has been reviewed and matches known correct reasoning.
    Verified,
    /// Artifact contains errors and needs revision.
    Invalid,
    /// Artifact is outdated and should be regenerated.
    Outdated,
}

/// Persistent learning artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    pub title: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub provenance: Vec<String>,
    pub depends_on: Vec<Uuid>,
    pub validation: ValidationStatus,
    pub usage_count: u32,
}

impl Artifact {
    pub fn new(kind: ArtifactKind, title: String, content: String) -> Self {
        let now = Utc::now();
        Self {
            id: ArtifactId::new(),
            kind,
            title,
            content,
            created_at: now,
            updated_at: now,
            provenance: Vec::new(),
            depends_on: Vec::new(),
            validation: ValidationStatus::Unverified,
            usage_count: 0,
        }
    }

    pub fn add_provenance(&mut self, source: String) {
        self.provenance.push(source);
        self.updated_at = Utc::now();
    }

    pub fn increment_usage(&mut self) {
        self.usage_count += 1;
    }

    pub fn set_validation(&mut self, status: ValidationStatus) {
        self.validation = status;
        self.updated_at = Utc::now();
    }
}
