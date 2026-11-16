//! Attention: decides whether something SAIL noticed deserves the owner's attention, and how.
//!
//! Pulse owns notification timing (#11, #102). Other subsystems never notify the owner
//! directly: they hand Pulse a [`Signal`], and Pulse decides its [`Delivery`]. Detection
//! (Discovery, Moodle, Mind) and delivery (Dialogue, the web inbox) stay separate.
//!
//! ```text
//! Signal ─► dedupe / cooldown ─► score(signal, context, prefs) ─► Delivery
//!                                                               ├─ Interrupt  (toast/voice now)
//!                                                               ├─ Suggest    (raise in conversation)
//!                                                               ├─ Digest     (batched, at digest time)
//!                                                               ├─ Silent     (kept, prepared, not shown)
//!                                                               └─ Drop       (duplicate, muted, stale)
//! owner feedback (useful / not now / later / investigate / dismiss / mute) ─► Preferences
//! ```
//!
//! # Scoring (`score::assess`, Antigravity)
//!
//! All inputs are in `0..=1`; deterministic, no model call.
//!
//! ```text
//! urgency'  = max(signal.urgency, deadline_pressure)      // deadline_pressure below
//! value     = 0.35·urgency' + 0.25·relevance + 0.20·novelty + 0.20·confidence
//! learned   = (accepted + 1) / (accepted + rejected + 2)  // per (source, kind), decayed
//! score     = value · (0.6 + 0.8·learned)  clamped to 0..=1
//! cost      = interruption_cost(context)                  // below
//! ```
//!
//! `deadline_pressure` is 0 without a deadline, 1.0 when it is ≤ 2 h away, and falls
//! linearly to 0 at 7 days away. A deadline in the past gives 0 (stale) and the signal
//! is `Drop { Stale }` unless `provenance == UserCommitment` (an overdue commitment
//! still matters, at `Digest`).
//!
//! `interruption_cost` is 0.2 normally; 0.6 during a voice turn or when the owner was
//! active in the last 2 minutes on something else (`context.busy`); 0.9 in focus mode;
//! 1.0 in quiet hours (sleep). `Autonomy::Off` drops everything except
//! `UserCommitment` signals, which go to `Digest`.
//!
//! Delivery, first match wins:
//!
//! 1. Same `dedupe_key` delivered/digested within `settings.cooldown_secs`, or muted
//!    `(source, kind)` → `Drop { Duplicate | Muted }`.
//! 2. `novelty < settings.min_novelty` and no deadline → `Drop { NotNovel }`.
//! 3. `score - cost ≥ settings.interrupt_threshold` and the hourly interrupt budget is not
//!    spent and autonomy ≥ Balanced → `Interrupt`. Quiet hours block this unless
//!    `urgency' ≥ 0.95` *and* provenance is `UserCommitment` (the trusted override). The
//!    override also ignores focus and quiet costs: its test is `score − 0.2 ≥ threshold`.
//!    It still respects the hourly interrupt budget and `Autonomy`.
//! 4. `score ≥ settings.suggest_threshold` and the daily suggestion budget is not spent
//!    and autonomy ≥ Balanced → `Suggest`.
//! 5. `score ≥ settings.digest_threshold` → `Digest`.
//! 6. otherwise → `Silent`.
//!
//! `Autonomy::Quiet` caps everything at `Digest`; `Autonomy::Proactive` lowers both
//! thresholds by 0.1. Every assessment carries human-readable `reasons`.
//!
//! # Learning (`score::learn`, Antigravity)
//!
//! Feedback updates `(source, kind)` tallies: `Useful` and `Investigate` add 1 to
//! accepted; `Dismiss` adds 1 to rejected; `NotNow` and `Later` add 0.25 to rejected
//! (timing, not content); `Mute` mutes the pair. Before each update, both tallies decay
//! by `0.5^(days_since_last_update / 30)`, so old habits fade. SAIL never optimizes for
//! engagement: tallies only move the score inside `0.6..=1.4×` and never bypass budgets.
//!
//! # Store (`store::AttentionStore`, Antigravity)
//!
//! SQLite `$SAIL_DATA/pulse/attention.db`. Items survive restarts. `ingest` assesses and
//! stores; `deliverable(now)` returns `Interrupt` items not yet shown plus `Later` items
//! whose time has come; `release_digest(now)` collects `Digest` items (oldest first, at
//! most `MAX_DIGEST_ITEMS`) into one [`DigestBatch`] and marks them; `suggestions(n)`
//! gives the top pending `Suggest` items for the conversation prompt; `feedback` records
//! the owner's reaction and re-schedules (`Later` → snoozed until the given time, `NotNow`
//! → back to `Digest`, `Investigate` → state `Investigating`). `stats` counts items by
//! delivery and state and feedback by kind, for observability.

pub mod route;
pub mod score;
pub mod store;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Most items in one digest.
pub const MAX_DIGEST_ITEMS: usize = 12;
/// Longest title, in characters.
pub const MAX_TITLE_CHARS: usize = 160;
/// Longest body, in characters.
pub const MAX_BODY_CHARS: usize = 1_200;

/// Which subsystem noticed something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Moodle,
    Calendar,
    Reminders,
    GitHub,
    Research,
    Capital,
    System,
    Escalation,
    Curiosity,
    Projects,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Moodle => "moodle",
            Source::Calendar => "calendar",
            Source::Reminders => "reminders",
            Source::GitHub => "github",
            Source::Research => "research",
            Source::Capital => "capital",
            Source::System => "system",
            Source::Escalation => "escalation",
            Source::Curiosity => "curiosity",
            Source::Projects => "projects",
        }
    }
}

/// Where a claim came from. Keeps owner commitments apart from SAIL's own ideas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Something the owner committed to or that is due for them (deadline, reminder).
    UserCommitment,
    /// A fact SAIL observed in a source (a new grade, a new file, disk nearly full).
    Observation,
    /// SAIL's suggestion for an action.
    Suggestion,
    /// An idea or connection SAIL generated itself; unverified.
    Hypothesis,
}

/// Something worth considering for the owner's attention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    pub source: Source,
    /// Short machine kind within the source, e.g. `deadline`, `grade`, `disk_low`.
    pub kind: String,
    /// Same key = same thing; used for dedup and cooldown (e.g. `moodle:item:123:due`).
    pub dedupe_key: String,
    pub title: String,
    pub body: String,
    pub urgency: f32,
    pub relevance: f32,
    pub novelty: f32,
    pub confidence: f32,
    pub deadline: Option<DateTime<Utc>>,
    pub provenance: Provenance,
    /// Link to open: a same-origin path (validated like reminder links), or a
    /// `https://github.com/…` page (see [`ALLOWED_EXTERNAL_LINK_PREFIXES`]).
    pub link: Option<String>,
    pub observed_at: DateTime<Utc>,
}

/// The owner's state when a signal is assessed. Built by the caller from Pulse settings,
/// the resource scheduler (voice) and recent activity; Pulse does not sense on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttentionContext {
    pub now: DateTime<Utc>,
    pub quiet_hours: bool,
    pub focus_until: Option<DateTime<Utc>>,
    pub voice_active: bool,
    pub busy: bool,
    pub interrupts_last_hour: u32,
    pub suggestions_today: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Autonomy {
    /// No initiative: only owner commitments, and only in digests.
    Off,
    /// Prepare silently and batch into digests; never interrupt or bring things up.
    Quiet,
    /// Default: interrupt rarely, suggest in conversation, digest the rest.
    Balanced,
    /// Lower thresholds; idle-time curiosity may run.
    Proactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    Duplicate,
    Muted,
    NotNovel,
    Stale,
    AutonomyOff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "delivery", content = "reason", rename_all = "snake_case")]
pub enum Delivery {
    Interrupt,
    Suggest,
    Digest,
    Silent,
    Drop(DropReason),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assessment {
    pub score: f32,
    pub interruption_cost: f32,
    pub delivery: Delivery,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AttentionSettings {
    pub autonomy: Autonomy,
    pub interrupt_threshold: f32,
    pub suggest_threshold: f32,
    pub digest_threshold: f32,
    pub min_novelty: f32,
    pub max_interrupts_per_hour: u32,
    pub max_suggestions_per_day: u32,
    pub cooldown_secs: u64,
    /// Local minutes of day when digests are released (e.g. 08:00 and 18:00).
    pub digest_minutes: Vec<u16>,
    /// "Don't interrupt until …"
    pub focus_until: Option<DateTime<Utc>>,
}

impl Default for AttentionSettings {
    fn default() -> Self {
        Self {
            autonomy: Autonomy::Balanced,
            interrupt_threshold: 0.6,
            suggest_threshold: 0.45,
            digest_threshold: 0.25,
            min_novelty: 0.2,
            max_interrupts_per_hour: 2,
            max_suggestions_per_day: 6,
            cooldown_secs: 12 * 3600,
            digest_minutes: vec![8 * 60, 18 * 60],
            focus_until: None,
        }
    }
}

/// The owner's lightweight reactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "feedback", content = "until", rename_all = "snake_case")]
pub enum Feedback {
    Useful,
    NotNow,
    Later(DateTime<Utc>),
    Investigate,
    Dismiss,
    Mute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    /// Waiting for its delivery (digest time, suggestion moment, or snooze end).
    Pending,
    /// Shown as an interrupt or raised in conversation.
    Delivered,
    /// Included in a released digest.
    Digested,
    Snoozed,
    Investigating,
    Dismissed,
    Done,
}

/// Learned preference for one (source, kind).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preference {
    pub source: Source,
    pub kind: String,
    pub accepted: f32,
    pub rejected: f32,
    pub muted: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttentionItem {
    pub id: String,
    pub signal: Signal,
    pub assessment: Assessment,
    pub state: ItemState,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_feedback: Option<Feedback>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DigestBatch {
    pub released_at: DateTime<Utc>,
    pub items: Vec<AttentionItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AttentionStats {
    /// (delivery name, count) over all stored items.
    pub by_delivery: Vec<(String, u64)>,
    /// (state name, count).
    pub by_state: Vec<(String, u64)>,
    /// (feedback name, count).
    pub feedback: Vec<(String, u64)>,
    pub interrupts_last_hour: u32,
    pub suggestions_today: u32,
}

/// Bound and validate a signal before scoring. Clamps the four scores to 0..=1 (NaN → 0),
/// trims and cuts title/body, requires a non-empty title and dedupe key, and validates
/// the link like reminder links.
pub fn normalize(mut signal: Signal) -> crate::Result<Signal> {
    let clamp = |v: f32| {
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    signal.urgency = clamp(signal.urgency);
    signal.relevance = clamp(signal.relevance);
    signal.novelty = clamp(signal.novelty);
    signal.confidence = clamp(signal.confidence);
    signal.title = cut_chars(signal.title.trim(), MAX_TITLE_CHARS);
    signal.body = cut_chars(signal.body.trim(), MAX_BODY_CHARS);
    signal.kind = cut_chars(signal.kind.trim(), 48);
    signal.dedupe_key = cut_chars(signal.dedupe_key.trim(), 200);
    if signal.title.is_empty() || signal.dedupe_key.is_empty() || signal.kind.is_empty() {
        return Err(crate::PulseError::Invalid(
            "a signal needs a title, kind and dedupe key".into(),
        ));
    }
    signal.link = match signal.link {
        Some(link) => Some(validate_signal_link(&link)?),
        None => None,
    };
    Ok(signal)
}

fn cut_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// External pages a signal may link to. Everything else must be same-origin.
pub const ALLOWED_EXTERNAL_LINK_PREFIXES: &[&str] = &["https://github.com/"];

fn validate_signal_link(link: &str) -> crate::Result<String> {
    let trimmed = link.trim();
    let external = ALLOWED_EXTERNAL_LINK_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix));
    if !external {
        return crate::validate_link(trimmed);
    }
    let bad_char = trimmed
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\\' | '\''));
    if trimmed.len() > 300 || bad_char {
        return Err(crate::PulseError::Invalid(
            "external signal link is too long or has a disallowed character".into(),
        ));
    }
    Ok(trimmed.to_owned())
}
