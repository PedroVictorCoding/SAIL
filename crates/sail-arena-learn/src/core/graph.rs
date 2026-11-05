//! Persistent learning graph (propositions, reasoning chains, sources, layout, settings).
//!
//! The learning graph stores accumulated knowledge from all learning sessions and imported notes,
//! not just a single lesson plan. It consists of propositions (atomic claims) connected by
//! multi-premise reasoning chains (`A ∧ B ⟹ C`) and attributed to birth sources.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

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

impl Default for ChainId {
    fn default() -> Self {
        Self::new()
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

/// A proposition passed as context into a teaching or practice session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphContextProp {
    pub name: String,
    pub statement: String,
}

/// Provenance record attributing a proposition to a teaching session or imported file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphProvenance {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: i64,
}

/// Persistent proposition node in the Knowledge Graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphProposition {
    pub id: String,
    pub name: String,
    pub statement: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub created_at: i64,
    #[serde(default)]
    pub provenance: Vec<GraphProvenance>,
}

/// Multi-premise reasoning chain (`premises[0] ∧ premises[1] ⟹ conclusion`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphReasoningChain {
    pub id: String,
    pub premises: Vec<String>,
    pub conclusion: String,
    pub created_at: i64,
    pub source_id: String,
    pub source_kind: String,
    pub source_label: String,
}

/// 2D canvas coordinate for a proposition node.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GraphNodePosition {
    pub x: f64,
    pub y: f64,
}

/// Visual rendering settings for the Knowledge Graph canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphRenderSettings {
    pub node_size: f64,
    pub edge_width: f64,
    pub arrow_size: f64,
    pub label_size: f64,
    pub label_zoom: f64,
    pub show_cluster_labels: bool,
}

impl Default for GraphRenderSettings {
    fn default() -> Self {
        Self {
            node_size: 1.0,
            edge_width: 1.0,
            arrow_size: 1.0,
            label_size: 1.0,
            label_zoom: 1.0,
            show_cluster_labels: true,
        }
    }
}

/// Source summary for the Chain Table cluster grouping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphSourceInfo {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub created_at: i64,
}

/// Full Knowledge Graph snapshot returned to the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct GraphSnapshot {
    pub propositions: Vec<GraphProposition>,
    pub chains: Vec<GraphReasoningChain>,
    pub sources: Vec<GraphSourceInfo>,
    pub positions: BTreeMap<String, GraphNodePosition>,
    pub render_settings: GraphRenderSettings,
}

fn slug_name(raw: &str) -> String {
    let mut words = Vec::new();
    for w in raw
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|s| !s.is_empty())
    {
        words.push(w.to_lowercase());
        if words.len() >= 5 {
            break;
        }
    }
    if words.is_empty() {
        "concept".to_string()
    } else {
        words.join(" ")
    }
}

/// Deterministically extracts atomic propositions and reasoning chains from markdown/text notes.
pub fn extract_from_notes(
    file_name: &str,
    content: &str,
    at_ms: i64,
) -> (Vec<GraphProposition>, Vec<GraphReasoningChain>) {
    let source_id = format!("notes:{file_name}");
    let prov = GraphProvenance {
        kind: "notes".into(),
        session_id: None,
        file_name: Some(file_name.to_string()),
        label: Some(file_name.to_string()),
        created_at: at_ms,
    };

    let mut props: Vec<GraphProposition> = Vec::new();
    let mut current_heading: Option<String> = None;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("```") {
            continue;
        }
        if let Some(h) = line.strip_prefix('#') {
            let h = h.trim_start_matches('#').trim();
            if !h.is_empty() {
                current_heading = Some(slug_name(h));
            }
            continue;
        }
        let cleaned = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .unwrap_or(line)
            .trim();
        if cleaned.len() < 12 {
            continue;
        }
        // Check for "name: statement" or "**name**: statement"
        let (name, statement) = if let Some((left, right)) = cleaned.split_once(':') {
            let left_clean = left.trim_matches('*').trim();
            if !left_clean.is_empty()
                && left_clean.split_whitespace().count() <= 6
                && right.trim().len() >= 8
            {
                (slug_name(left_clean), right.trim().to_string())
            } else {
                let prefix = current_heading.as_deref().unwrap_or("claim");
                (
                    format!("{} {}", prefix, props.len() + 1),
                    cleaned.to_string(),
                )
            }
        } else {
            let first_clause = cleaned
                .split(['.', ',', ';', '—', '('])
                .next()
                .unwrap_or(cleaned);
            (slug_name(first_clause), cleaned.to_string())
        };

        if !props.iter().any(|p| p.name == name) {
            props.push(GraphProposition {
                id: format!("prop-{}", Uuid::new_v4()),
                name,
                statement,
                aliases: Vec::new(),
                created_at: at_ms,
                provenance: vec![prov.clone()],
            });
        }
        if props.len() >= 24 {
            break;
        }
    }

    let mut chains = Vec::new();
    for pair in props.windows(2) {
        chains.push(GraphReasoningChain {
            id: format!("chain-{}", Uuid::new_v4()),
            premises: vec![pair[0].name.clone()],
            conclusion: pair[1].name.clone(),
            created_at: at_ms,
            source_id: source_id.clone(),
            source_kind: "notes".into(),
            source_label: file_name.to_string(),
        });
    }
    (props, chains)
}
