//! Deterministic detectors: turn observations from SAIL's subsystems into attention signals.
//!
//! No model is called here. Each detector is a pure function of its observation and `now`,
//! so the same facts always produce the same signal and the same `dedupe_key` (Pulse drops
//! repeats). Callers gather observations from the stores they already own (Moodle's local
//! store, the escalation ledger, the host probe) and pass them in; Discovery reads nothing
//! itself.
use chrono::{DateTime, Duration, Utc};
use sail_pulse::attention::{Provenance, Signal, Source};
use serde::{Deserialize, Serialize};

/// Kinds of school items, mirrored from `sail_moodle::contracts::ItemKind` without the
/// dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchoolItemKind {
    Exam,
    Test,
    Quiz,
    Deadline,
    Presentation,
    Lab,
    ScheduleChange,
    Registration,
    Other,
}

/// Kinds of observed school changes worth attention. Structural noise (sections, renames,
/// visibility) is not represented and is never surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchoolChangeKind {
    GradeReleased,
    AnnouncementPosted,
    DeadlineMoved,
    AssignmentAdded,
    FileAdded,
    EventAdded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Observation {
    /// A dated school item from the Moodle store.
    SchoolItem {
        id: String,
        course: String,
        kind: SchoolItemKind,
        title: String,
        due: DateTime<Utc>,
        /// Share of the final grade (0–100), when known.
        weight_percent: Option<f32>,
        /// Extraction confidence in 0..=1.
        confidence: f32,
    },
    SchoolChange {
        id: String,
        course: String,
        kind: SchoolChangeKind,
        title: String,
        observed_at: DateTime<Utc>,
    },
    /// A stronger model answered a background or earlier question.
    EscalationAnswered {
        job_id: String,
        model: String,
        question: String,
        correlation_id: Option<String>,
    },
    /// Free space on the SAIL data disk.
    DiskSpace { mount: String, free_percent: f32 },
    /// An open agenda question met new evidence (see `agenda::AgendaStore::match_evidence`).
    AgendaRevisit {
        item_id: String,
        title: String,
        evidence: String,
    },
}

/// Days before a school item when preparation starts to matter.
pub const SCHOOL_HORIZON_DAYS: i64 = 14;

/// The signals one observation produces (zero or more).
pub fn detect(observation: &Observation, now: DateTime<Utc>) -> Vec<Signal> {
    match observation {
        Observation::SchoolItem {
            id,
            course,
            kind,
            title,
            due,
            weight_percent,
            confidence,
        } => school_item(
            id,
            course,
            *kind,
            title,
            *due,
            *weight_percent,
            *confidence,
            now,
        ),
        Observation::SchoolChange {
            id,
            course,
            kind,
            title,
            observed_at,
        } => school_change(id, course, *kind, title, *observed_at, now),
        Observation::EscalationAnswered {
            job_id,
            model,
            question,
            ..
        } => vec![Signal {
            source: Source::Escalation,
            kind: "answer".into(),
            dedupe_key: format!("escalation:{job_id}"),
            title: format!("{model} answered: {}", excerpt(question, 80)),
            body: "A stronger model answered a question SAIL escalated earlier.".into(),
            urgency: 0.3,
            relevance: 0.8,
            novelty: 1.0,
            confidence: 0.7,
            deadline: None,
            provenance: Provenance::Observation,
            link: None,
            observed_at: now,
        }],
        Observation::DiskSpace {
            mount,
            free_percent,
        } => disk(mount, *free_percent, now),
        Observation::AgendaRevisit {
            item_id,
            title,
            evidence,
        } => vec![Signal {
            source: Source::Projects,
            kind: "revisit".into(),
            dedupe_key: format!("agenda:{item_id}:revisit:{}", short_hash(evidence)),
            title: format!("New evidence on: {}", excerpt(title, 100)),
            body: excerpt(evidence, 400),
            urgency: 0.2,
            relevance: 0.7,
            novelty: 0.9,
            confidence: 0.5,
            deadline: None,
            provenance: Provenance::Hypothesis,
            link: None,
            observed_at: now,
        }],
    }
}

#[allow(clippy::too_many_arguments)]
fn school_item(
    id: &str,
    course: &str,
    kind: SchoolItemKind,
    title: &str,
    due: DateTime<Utc>,
    weight_percent: Option<f32>,
    confidence: f32,
    now: DateTime<Utc>,
) -> Vec<Signal> {
    let until = due - now;
    if until <= Duration::zero() || until > Duration::days(SCHOOL_HORIZON_DAYS) {
        return Vec::new();
    }
    let assessed = matches!(
        kind,
        SchoolItemKind::Exam
            | SchoolItemKind::Test
            | SchoolItemKind::Quiz
            | SchoolItemKind::Deadline
            | SchoolItemKind::Presentation
            | SchoolItemKind::Registration
    );
    // Heavier items matter more; unknown weight is treated as moderate.
    let weight = weight_percent.map_or(0.5, |w| (w / 40.0).clamp(0.2, 1.0));
    let relevance = if assessed { 0.6 + 0.4 * weight } else { 0.4 };
    // The stage decides the dedupe key, so one item yields at most one signal per stage:
    // two weeks out (prepare), three days out (plan), and the last day (now).
    let (stage, urgency, what) = if until <= Duration::hours(24) {
        ("day", 0.9, "is due within a day")
    } else if until <= Duration::days(3) {
        ("3d", 0.6, "is due in under three days")
    } else {
        ("14d", 0.25, "is coming up in the next two weeks")
    };
    let label = match kind {
        SchoolItemKind::Exam => "Exam",
        SchoolItemKind::Test => "Test",
        SchoolItemKind::Quiz => "Quiz",
        SchoolItemKind::Deadline => "Deadline",
        SchoolItemKind::Presentation => "Presentation",
        SchoolItemKind::Lab => "Lab",
        SchoolItemKind::ScheduleChange => "Schedule change",
        SchoolItemKind::Registration => "Registration",
        SchoolItemKind::Other => "School item",
    };
    let weight_text = weight_percent
        .map(|w| format!(" It counts for {w:.0}% of the grade."))
        .unwrap_or_default();
    vec![Signal {
        source: Source::Moodle,
        kind: format!("due_{stage}"),
        dedupe_key: format!("moodle:item:{id}:{stage}"),
        title: format!("{label} in {}: {}", excerpt(course, 60), excerpt(title, 80)),
        body: format!("{label} {what}.{weight_text}"),
        urgency,
        relevance,
        novelty: if stage == "14d" { 0.8 } else { 0.6 },
        confidence: confidence.clamp(0.0, 1.0),
        deadline: Some(due),
        provenance: Provenance::UserCommitment,
        link: Some("/moodle".into()),
        observed_at: now,
    }]
}

fn school_change(
    id: &str,
    course: &str,
    kind: SchoolChangeKind,
    title: &str,
    observed_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Vec<Signal> {
    // Changes older than a week are history, not news.
    if now - observed_at > Duration::days(7) {
        return Vec::new();
    }
    let (kind_name, urgency, relevance) = match kind {
        SchoolChangeKind::GradeReleased => ("grade", 0.5, 0.9),
        SchoolChangeKind::DeadlineMoved => ("deadline_moved", 0.6, 0.9),
        SchoolChangeKind::AnnouncementPosted => ("announcement", 0.35, 0.7),
        SchoolChangeKind::AssignmentAdded => ("assignment", 0.4, 0.8),
        SchoolChangeKind::EventAdded => ("event", 0.3, 0.6),
        SchoolChangeKind::FileAdded => ("file", 0.1, 0.4),
    };
    vec![Signal {
        source: Source::Moodle,
        kind: kind_name.into(),
        dedupe_key: format!("moodle:change:{id}"),
        title: format!("{}: {}", excerpt(course, 60), excerpt(title, 90)),
        body: String::new(),
        urgency,
        relevance,
        // A new file is routine; other changes are news.
        novelty: if kind == SchoolChangeKind::FileAdded {
            0.5
        } else {
            1.0
        },
        confidence: 0.9,
        deadline: None,
        provenance: Provenance::Observation,
        link: Some("/moodle".into()),
        observed_at: now,
    }]
}

fn disk(mount: &str, free_percent: f32, now: DateTime<Utc>) -> Vec<Signal> {
    if !free_percent.is_finite() || free_percent >= 10.0 {
        return Vec::new();
    }
    let critical = free_percent < 3.0;
    vec![Signal {
        source: Source::System,
        kind: "disk_low".into(),
        dedupe_key: format!(
            "system:disk:{mount}:{}",
            if critical { "critical" } else { "low" }
        ),
        title: format!("{mount} has {free_percent:.1}% free"),
        body: "SAIL's data disk is filling up; model downloads, memory and logs may fail.".into(),
        urgency: if critical { 0.85 } else { 0.4 },
        relevance: 0.8,
        novelty: 0.7,
        confidence: 1.0,
        deadline: None,
        provenance: Provenance::Observation,
        link: None,
        observed_at: now,
    }]
}

/// Character-safe excerpt with an ellipsis.
pub fn excerpt(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A short, stable (FNV-1a) hash for dedupe keys built from free text.
pub fn short_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}
