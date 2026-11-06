//! Typed knowledge contracts. Every enum round-trips through the exact strings the schema's
//! CHECK constraints accept.

use serde::{Deserialize, Serialize};

macro_rules! str_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }

            pub fn parse(text: &str) -> Option<Self> {
                match text { $($text => Some($name::$variant),)+ _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

str_enum!(
    /// What an entity is. `Assistant` is SAIL itself, so its capabilities and verified
    /// accomplishments live in the same graph as everything else.
    EntityKind {
        Person => "person",
        Project => "project",
        Software => "software",
        Device => "device",
        Environment => "environment",
        Place => "place",
        Organization => "organization",
        Concept => "concept",
        Procedure => "procedure",
        Assistant => "assistant",
        Other => "other",
    }
);

str_enum!(
    /// Where a piece of evidence came from. This is the reliability axis; it is never folded
    /// into a single confidence number.
    SourceKind {
        ToolObservation => "tool_observation",
        ExternalArtifact => "external_artifact",
        UserStatement => "user_statement",
        ModelInference => "model_inference",
        PriorMemory => "prior_memory",
    }
);

impl SourceKind {
    /// Observed directly by a tool or read from a verifiable artifact.
    pub fn is_direct(self) -> bool {
        matches!(self, Self::ToolObservation | Self::ExternalArtifact)
    }
}

str_enum!(
    /// What SAIL currently holds about a claim. `Superseded` and `Invalidated` are reached
    /// only through revision, never on insert.
    EpistemicStatus {
        Verified => "verified",
        UserReported => "user_reported",
        Inferred => "inferred",
        Unverified => "unverified",
        Disputed => "disputed",
        Superseded => "superseded",
        Invalidated => "invalidated",
    }
);

impl EpistemicStatus {
    /// Claims in these states take part in current-state projections.
    pub fn is_active(self) -> bool {
        !matches!(self, Self::Superseded | Self::Invalidated)
    }
}

str_enum!(
    ClaimCategory {
        Fact => "fact",
        Preference => "preference",
        Event => "event",
        Capability => "capability",
        Question => "question",
    }
);

str_enum!(
    /// How a preference became known. Explicit preferences outrank every inferred one.
    PreferenceBasis {
        Explicit => "explicit",
        Observed => "observed",
        Inferred => "inferred",
        Situational => "situational",
    }
);

str_enum!(
    Stance {
        Supports => "supports",
        Refutes => "refutes",
    }
);

str_enum!(
    ProjectItemKind {
        Objective => "objective",
        Constraint => "constraint",
        Decision => "decision",
        Milestone => "milestone",
        Task => "task",
        Dependency => "dependency",
        Risk => "risk",
    }
);

str_enum!(
    /// Lifecycle of project work. `Implemented`, `Tested` and `Deployed` need direct
    /// evidence: a model describing the work is not enough.
    WorkState {
        Proposed => "proposed",
        Approved => "approved",
        InProgress => "in_progress",
        Implemented => "implemented",
        Tested => "tested",
        Deployed => "deployed",
        Failed => "failed",
        Superseded => "superseded",
    }
);

impl WorkState {
    pub fn requires_direct_evidence(self) -> bool {
        matches!(self, Self::Implemented | Self::Tested | Self::Deployed)
    }
}

str_enum!(
    ProcedureStatus {
        Draft => "draft",
        Active => "active",
        Deprecated => "deprecated",
    }
);

str_enum!(
    RunOutcome {
        Succeeded => "succeeded",
        Failed => "failed",
        Aborted => "aborted",
    }
);

/// How sensitive a record is. Readers see records up to their `Access::max_sensitivity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Normal = 0,
    Private = 1,
    Sensitive = 2,
}

impl Sensitivity {
    pub(crate) fn from_i64(v: i64) -> Self {
        match v {
            0 => Self::Normal,
            1 => Self::Private,
            _ => Self::Sensitive,
        }
    }
}

/// Retention policy for a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "policy")]
pub enum Retention {
    Durable,
    /// Eligible for expiry after `expires_at` (ms since epoch).
    Transient {
        expires_at: i64,
    },
}

/// Who performed a change. User actions may override tombstones; subsystems may not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    User,
    Subsystem(String),
}

impl Actor {
    pub fn subsystem(name: impl Into<String>) -> Self {
        Self::Subsystem(name.into())
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Self::User => "user".into(),
            Self::Subsystem(name) => format!("subsystem:{name}"),
        }
    }
}

/// Read permission for one caller. Built by the integration layer from Guardian's grant;
/// the store enforces it in SQL, so unauthorized rows are never hydrated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access {
    /// Visible scopes, e.g. `user`, `project:sail`, `life`. `None` means every scope.
    pub scopes: Option<Vec<String>>,
    pub max_sensitivity: Sensitivity,
}

impl Access {
    /// Everything, for the owner's own administrative views.
    pub fn owner() -> Self {
        Self {
            scopes: None,
            max_sensitivity: Sensitivity::Sensitive,
        }
    }

    pub fn scoped(scopes: &[&str], max_sensitivity: Sensitivity) -> Self {
        Self {
            scopes: Some(scopes.iter().map(|s| s.to_string()).collect()),
            max_sensitivity,
        }
    }

    pub fn allows(&self, scope: &str, sensitivity: Sensitivity) -> bool {
        sensitivity <= self.max_sensitivity
            && self
                .scopes
                .as_ref()
                .is_none_or(|s| s.iter().any(|x| x == scope))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: i64,
    pub kind: EntityKind,
    pub name: String,
    pub aliases: Vec<String>,
    pub merged_into: Option<i64>,
    pub scope: String,
    pub sensitivity: Sensitivity,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewEntity<'a> {
    pub kind: EntityKind,
    pub name: &'a str,
    pub aliases: &'a [&'a str],
    pub scope: &'a str,
    pub sensitivity: Sensitivity,
}

#[derive(Debug, Clone)]
pub struct NewEvidence<'a> {
    pub source_kind: SourceKind,
    /// Tool call id, artifact path/URL, transcript record id, etc.
    pub source_ref: Option<&'a str>,
    pub observed_at: i64,
    /// The full source content. Only its hash and a bounded receipt are stored.
    pub content: &'a str,
    /// Short excerpt kept as a receipt; clipped to [`MAX_RECEIPT_CHARS`].
    pub receipt: Option<&'a str>,
    /// For derived evidence (`PriorMemory`, summaries): the evidence it restates. It shares
    /// that evidence's origin, so restatements never count as independent support.
    pub derived_from: Option<i64>,
    pub verification: Option<serde_json::Value>,
    pub scope: &'a str,
}

pub const MAX_RECEIPT_CHARS: usize = 280;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: i64,
    pub source_kind: SourceKind,
    pub source_ref: Option<String>,
    pub observed_at: i64,
    pub receipt: Option<String>,
    pub verification: Option<serde_json::Value>,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum ClaimObject {
    Entity(i64),
    Value(String),
}

#[derive(Debug, Clone)]
pub struct NewClaim<'a> {
    pub subject: i64,
    pub predicate: &'a str,
    pub object: ClaimObject,
    pub category: ClaimCategory,
    /// Required for, and only for, preferences.
    pub basis: Option<PreferenceBasis>,
    pub status: EpistemicStatus,
    /// Only for `Inferred` claims: the extractor's own confidence, kept apart from evidence.
    pub inference_confidence: Option<f64>,
    /// When the claim was observed (event time may differ: see `valid_from`).
    pub observed_at: i64,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub retention: Retention,
    pub scope: &'a str,
    pub sensitivity: Sensitivity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub id: i64,
    pub subject: i64,
    pub predicate: String,
    pub object: ClaimObject,
    pub category: ClaimCategory,
    pub basis: Option<PreferenceBasis>,
    pub status: EpistemicStatus,
    pub inference_confidence: Option<f64>,
    pub observed_at: i64,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub last_verified_at: Option<i64>,
    pub superseded_by: Option<i64>,
    pub status_reason: Option<String>,
    pub retention: Retention,
    pub scope: String,
    pub sensitivity: Sensitivity,
    /// Pinned by the user: durable and exempt from expiry and decay.
    pub pinned: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A claim with the names needed to display it outside its entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimSummary {
    pub claim: Claim,
    pub subject_name: String,
    pub object_name: Option<String>,
}

/// Which claims a listing returns. Empty lists mean "any".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaimFilter {
    pub categories: Vec<ClaimCategory>,
    pub statuses: Vec<EpistemicStatus>,
    pub limit: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeCounts {
    pub entities: i64,
    pub active_claims: i64,
    pub needs_review: i64,
    pub procedures: i64,
    pub project_items: i64,
}

/// Evidence behind a claim, broken into separate axes instead of one score.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Support {
    /// Distinct origins of supporting tool observations and artifacts.
    pub direct_origins: usize,
    /// Distinct origins of supporting user statements.
    pub user_origins: usize,
    /// Supporting model inferences (never independent of what they were inferred from).
    pub inferences: usize,
    /// Distinct origins of refuting evidence of any kind.
    pub refuting_origins: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceLink {
    pub evidence: Evidence,
    pub stance: Stance,
}

/// One relationship edge as seen from a given entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub claim_id: i64,
    pub relation: String,
    /// True when the queried entity is the subject.
    pub outgoing: bool,
    pub other: Entity,
    pub status: EpistemicStatus,
}

/// A remembered method. Remembering grants nothing: `required_capabilities` names what
/// Guardian must authorize before any step runs, and this crate exposes no way to execute.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcedureSpec {
    pub summary: String,
    pub preconditions: Vec<String>,
    pub required_capabilities: Vec<String>,
    pub steps: Vec<ProcedureStep>,
    pub postconditions: Vec<String>,
    pub failure_handling: Vec<String>,
    pub safety_constraints: Vec<String>,
    pub applicability: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcedureStep {
    pub description: String,
    /// Optional reference to a typed workflow/tool; never a raw shell command.
    pub workflow_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Procedure {
    pub id: i64,
    pub entity_id: i64,
    pub version: i64,
    pub status: ProcedureStatus,
    pub spec: ProcedureSpec,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcedureRun {
    pub id: i64,
    pub procedure_id: i64,
    pub outcome: RunOutcome,
    pub evidence_id: i64,
    pub notes: Option<String>,
    pub at: i64,
}

#[derive(Debug, Clone)]
pub struct NewProjectItem<'a> {
    pub project: i64,
    pub kind: ProjectItemKind,
    pub title: &'a str,
    pub detail: Option<&'a str>,
    pub state: WorkState,
    pub scope: &'a str,
    pub sensitivity: Sensitivity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectItem {
    pub id: i64,
    pub project: i64,
    pub kind: ProjectItemKind,
    pub title: String,
    pub detail: Option<String>,
    pub state: WorkState,
    pub state_evidence: Option<i64>,
    pub superseded_by: Option<i64>,
    pub scope: String,
    pub sensitivity: Sensitivity,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub id: i64,
    pub at: i64,
    pub actor: String,
    pub op: String,
    pub target_kind: String,
    pub target_id: Option<i64>,
    pub reason: Option<String>,
    pub detail: Option<serde_json::Value>,
}

/// Lowercased, whitespace-collapsed form used for exact name and alias lookup.
pub fn name_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// `snake_case`, 1–64 chars, starting with a letter.
pub fn valid_predicate(p: &str) -> bool {
    let mut chars = p.chars();
    p.len() <= 64
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_round_trip_their_schema_strings() {
        for s in EpistemicStatus::ALL {
            assert_eq!(EpistemicStatus::parse(s.as_str()), Some(*s));
        }
        for s in WorkState::ALL {
            assert_eq!(WorkState::parse(s.as_str()), Some(*s));
        }
        assert_eq!(SourceKind::parse("rumour"), None);
    }

    #[test]
    fn access_filters_scope_and_sensitivity() {
        let forge = Access::scoped(&["project:sail"], Sensitivity::Normal);
        assert!(forge.allows("project:sail", Sensitivity::Normal));
        assert!(!forge.allows("life", Sensitivity::Normal));
        assert!(!forge.allows("project:sail", Sensitivity::Private));
        assert!(Access::owner().allows("life", Sensitivity::Sensitive));
    }

    #[test]
    fn names_and_predicates_normalise() {
        assert_eq!(name_key("  SAIL   Core "), "sail core");
        assert!(valid_predicate("primary_language"));
        assert!(!valid_predicate("Primary Language"));
        assert!(!valid_predicate("_x"));
    }
}
