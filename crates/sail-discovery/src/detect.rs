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
    /// Activity on the owner's GitHub repositories (#451; see [`crate::github`]).
    GitHub(crate::github::GithubEvent),
    /// An active SAIL Capital suggestion (portfolio policy, idea, data problem).
    CapitalSuggestion {
        id: String,
        severity: Level,
        title: String,
        why: String,
    },
    /// A Forge coding job reached a state the owner may care about.
    ForgeJob {
        job_id: String,
        /// e.g. `owner/repo#12`.
        subject: String,
        state: ForgeJobState,
    },
    /// A Curiosity Engine run produced notes on an agenda item (#453).
    CuriosityFinding {
        run_id: String,
        item_id: String,
        title: String,
        summary: String,
        sources: Vec<String>,
    },
    /// A calendar event that may need preparation (#452).
    CalendarEvent {
        id: String,
        title: String,
        start: DateTime<Utc>,
        all_day: bool,
        prepare: bool,
        location: Option<String>,
    },
    /// A subscribed feed event moved or was cancelled.
    CalendarChange {
        id: String,
        title: String,
        kind: CalendarChangeKind,
        new_start: Option<DateTime<Utc>>,
        observed_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalendarChangeKind {
    Moved,
    Cancelled,
}

/// How much a source says something matters (Capital's severity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    High,
    Medium,
    Low,
}

/// The Forge job states worth noticing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForgeJobState {
    /// The job needs an answer from the owner.
    AwaitingUser,
    /// Tests passed; a review is waiting.
    ReviewPending,
    Blocked,
    Completed,
}

/// Days before a school item when preparation starts to matter.
pub const SCHOOL_HORIZON_DAYS: i64 = 14;

/// The signals one observation produces (zero or more).
pub fn detect(observation: &Observation, now: DateTime<Utc>) -> Vec<Signal> {
    detect_in_tz(observation, now, chrono_tz::Europe::Lisbon)
}

/// Same rules, with the owner's time zone for calendar change wording.
pub fn detect_in_tz(
    observation: &Observation,
    now: DateTime<Utc>,
    owner_tz: chrono_tz::Tz,
) -> Vec<Signal> {
    match observation {
        Observation::GitHub(event) => crate::github::github_signal(event, now),
        Observation::CapitalSuggestion {
            id,
            severity,
            title,
            why,
        } => capital_suggestion(id, *severity, title, why, now),
        Observation::ForgeJob {
            job_id,
            subject,
            state,
        } => forge_job(job_id, subject, *state, now),
        Observation::CuriosityFinding {
            run_id,
            title,
            summary,
            sources,
            ..
        } => curiosity_finding(run_id, title, summary, sources, now),
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
        Observation::CalendarEvent {
            id,
            title,
            start,
            prepare,
            ..
        } => calendar_event(id, title, *start, *prepare, now),
        Observation::CalendarChange {
            id,
            title,
            kind,
            new_start,
            observed_at,
        } => calendar_change(id, title, *kind, *new_start, *observed_at, now, owner_tz),
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
/// A prepared calendar event within 48 h becomes a Commitment. All other calendar
/// events are handled by the greeting/briefing.
fn calendar_event(
    id: &str,
    title: &str,
    start: DateTime<Utc>,
    prepare: bool,
    now: DateTime<Utc>,
) -> Vec<Signal> {
    if !prepare || start <= now || start - now > Duration::hours(48) {
        return Vec::new();
    }
    let until = start - now;
    let urgency = if until <= Duration::hours(6) {
        0.8
    } else {
        0.4
    };
    vec![Signal {
        source: Source::Calendar,
        kind: "calendar_prep".into(),
        dedupe_key: format!("cal-prep:{id}"),
        title: format!("Prepare: {}", excerpt(title, 90)),
        body: "This event needs preparation.".into(),
        urgency,
        relevance: 0.75,
        novelty: 0.6,
        confidence: 0.9,
        deadline: Some(start),
        provenance: Provenance::UserCommitment,
        link: Some("/calendar".into()),
        observed_at: now,
    }]
}

fn calendar_change(
    id: &str,
    title: &str,
    kind: CalendarChangeKind,
    new_start: Option<DateTime<Utc>>,
    observed_at: DateTime<Utc>,
    now: DateTime<Utc>,
    owner_tz: chrono_tz::Tz,
) -> Vec<Signal> {
    if now - observed_at > Duration::days(7) {
        return Vec::new();
    }
    let (kind_name, label) = match kind {
        CalendarChangeKind::Moved => {
            let when = new_start
                .map(|start| {
                    start
                        .with_timezone(&owner_tz)
                        .format("%a %H:%M")
                        .to_string()
                })
                .unwrap_or_else(|| "a new time".into());
            (
                "calendar_moved",
                format!("Moved: {} → {when}", excerpt(title, 80)),
            )
        }
        CalendarChangeKind::Cancelled => (
            "calendar_cancelled",
            format!("Cancelled: {}", excerpt(title, 90)),
        ),
    };
    vec![Signal {
        source: Source::Calendar,
        kind: kind_name.into(),
        dedupe_key: format!(
            "cal-chg:{id}:{}",
            new_start
                .map(|start| start.to_rfc3339())
                .unwrap_or_else(|| "x".into())
        ),
        title: label,
        body: "A subscribed calendar changed.".into(),
        urgency: 0.45,
        relevance: 0.8,
        novelty: 1.0,
        confidence: 0.9,
        deadline: new_start,
        provenance: Provenance::Observation,
        link: Some("/calendar".into()),
        observed_at: now,
    }]
}

#[allow(clippy::too_many_arguments)] // pre-existing signature; logged in LLM_Handoff
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
    // Two weeks out is preparation, not news: the digest, unless the item is heavy.
    let early_relevance = if assessed { 0.3 + 0.3 * weight } else { 0.25 };
    // The stage decides the dedupe key, so one item yields at most one signal per stage:
    // two weeks out (prepare), three days out (plan), and the last day (now).
    let (stage, urgency, what, relevance) = if until <= Duration::hours(24) {
        ("day", 0.9, "is due within a day", relevance)
    } else if until <= Duration::days(3) {
        ("3d", 0.6, "is due in under three days", relevance)
    } else {
        (
            "14d",
            0.15,
            "is coming up in the next two weeks",
            early_relevance,
        )
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
        // The card shows the item's own name; the course and stage go in the body,
        // so a long course name never crowds the title.
        title: excerpt(title, 90),
        body: format!("{label} for {} {what}.{weight_text}", excerpt(course, 60)),
        urgency,
        relevance,
        novelty: if stage == "14d" { 0.4 } else { 0.6 },
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
    // Only a released grade or a moved deadline is worth raising; the rest of the
    // course's churn goes to the digest. Nothing here interrupts on its own.
    let (kind_name, urgency, relevance, novelty, confidence) = match kind {
        SchoolChangeKind::GradeReleased => ("grade", 0.5, 0.9, 1.0, 0.9),
        SchoolChangeKind::DeadlineMoved => ("deadline_moved", 0.45, 0.8, 1.0, 0.9),
        SchoolChangeKind::AnnouncementPosted => ("announcement", 0.15, 0.4, 0.5, 0.7),
        SchoolChangeKind::AssignmentAdded => ("assignment", 0.2, 0.45, 0.6, 0.7),
        SchoolChangeKind::EventAdded => ("event", 0.15, 0.4, 0.5, 0.7),
        SchoolChangeKind::FileAdded => ("file", 0.1, 0.4, 0.5, 0.7),
    };
    vec![Signal {
        source: Source::Moodle,
        kind: kind_name.into(),
        dedupe_key: format!("moodle:change:{id}"),
        title: excerpt(title, 90),
        body: excerpt(course, 80),
        urgency,
        relevance,
        novelty,
        confidence,
        deadline: None,
        provenance: Provenance::Observation,
        link: Some("/moodle".into()),
        observed_at: now,
    }]
}

/// High → worth raising; Medium → digest; Low → nothing.
fn capital_suggestion(
    id: &str,
    severity: Level,
    title: &str,
    why: &str,
    now: DateTime<Utc>,
) -> Vec<Signal> {
    let (urgency, relevance, novelty) = match severity {
        Level::High => (0.5, 0.8, 0.9),
        Level::Medium => (0.2, 0.45, 0.6),
        Level::Low => return Vec::new(),
    };
    vec![Signal {
        source: Source::Capital,
        kind: "suggestion".into(),
        dedupe_key: format!("capital:suggestion:{id}"),
        title: excerpt(title, 120),
        body: excerpt(why, 400),
        urgency,
        relevance,
        novelty,
        confidence: 0.7,
        deadline: None,
        provenance: Provenance::Suggestion,
        link: Some("/capital".into()),
        observed_at: now,
    }]
}

/// One signal per job and state, so a job that moves on is news again.
fn forge_job(job_id: &str, subject: &str, state: ForgeJobState, now: DateTime<Utc>) -> Vec<Signal> {
    let (kind, title, urgency, relevance, novelty) = match state {
        ForgeJobState::AwaitingUser => ("awaiting_user", "Forge needs your answer", 0.5, 0.8, 1.0),
        ForgeJobState::ReviewPending => (
            "review_pending",
            "Forge job ready for review",
            0.4,
            0.7,
            0.9,
        ),
        ForgeJobState::Blocked => ("blocked", "Forge job blocked", 0.35, 0.7, 0.9),
        ForgeJobState::Completed => ("completed", "Forge job finished", 0.1, 0.4, 0.5),
    };
    vec![Signal {
        source: Source::Projects,
        kind: format!("forge_{kind}"),
        dedupe_key: format!("forge:{job_id}:{kind}"),
        title: format!("{title}: {}", excerpt(subject, 90)),
        body: String::new(),
        urgency,
        relevance,
        novelty,
        confidence: 0.95,
        deadline: None,
        provenance: Provenance::Observation,
        link: None,
        observed_at: now,
    }]
}

/// SAIL's own notes: a hypothesis for the digest, never the owner's words.
fn curiosity_finding(
    run_id: &str,
    title: &str,
    summary: &str,
    sources: &[String],
    now: DateTime<Utc>,
) -> Vec<Signal> {
    if summary.trim().is_empty() {
        return Vec::new();
    }
    let sources = if sources.is_empty() {
        "no sources".to_owned()
    } else {
        sources.join("; ")
    };
    vec![Signal {
        source: Source::Curiosity,
        kind: "finding".into(),
        dedupe_key: format!("curiosity:{run_id}"),
        title: format!("Looked into: {}", excerpt(title, 90)),
        body: excerpt(
            &format!(
                "SAIL's unverified notes: {} Sources: {sources}.",
                summary.trim()
            ),
            900,
        ),
        urgency: 0.1,
        relevance: 0.6,
        novelty: 0.8,
        confidence: 0.5,
        deadline: None,
        provenance: Provenance::Hypothesis,
        link: None,
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
