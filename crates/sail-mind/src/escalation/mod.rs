//! Reasoning escalation: when, and whether, the local model may ask a stronger model.
//!
//! This module is the contract for owner decision 12 in
//! `docs/SAIL_EXTERNAL_MODELS_GATEWAY_SKILLS.md` and for `docs/SAIL_PROACTIVE.md`. The local
//! model stays the default for conversation, reasoning and tools. It may *request* a
//! stronger model through the `escalate_reasoning` tool; trusted Rust code decides.
//!
//! # Flow
//!
//! ```text
//! EscalationRequest ──► fingerprint ──► cache hit? ──► Decision::Cached
//!        │                                  │ no
//!        ▼                                  ▼
//!   policy::decide(request, policy, usage, outcomes, now) ──► Deny{reason} | Escalate{provider, budget}
//!        │ Escalate
//!        ▼
//!   redact (fail closed) ──► provider.complete (async, background) ──► rehydrate
//!        ▼
//!   store: usage row, cache row, job row (pending → done | failed)
//!        ▼
//!   later: feedback(job, Outcome) ──► OutcomeStats used by the next decide()
//! ```
//!
//! # Rules (`policy::decide`, Kilo)
//!
//! Checks run in this order; the first that fails is the `DenyReason`:
//!
//! 1. `policy.enabled` is false → `Disabled`. **Escalation is off by default.**
//! 2. `request.origin == Origin::VoiceTurn` and `!policy.allow_voice` → `VoiceTurn`
//!    (live voice never waits on a remote model unless the owner allows it).
//! 3. `request.token_budget == 0` or above `policy.per_task_max_tokens` → `OverTaskBudget`.
//! 4. Fewer than `policy.min_local_attempts(kind)` local attempts and the origin is not
//!    `Owner` → `NotEnoughLocalAttempts`. The owner asking for a provider skips 4 and 7.
//! 5. `usage.tokens_today + budget > daily_token_cap` → `OverDailyTokens`; same for the
//!    month (`OverMonthlyTokens`), and for cost with the provider's estimated cost
//!    (`OverDailyCost`, `OverMonthlyCost`). A cap of 0 means "no cap of this kind".
//! 6. An escalation with the same fingerprint finished less than `cooldown_secs` ago, or is
//!    in flight → `Cooldown` / `InFlight`. (A finished one inside `cache_ttl_secs` is
//!    returned as `Decision::Cached` by the service *before* `decide` runs.)
//! 7. `route_score(request, outcomes) < policy.route_threshold` → `BelowThreshold`.
//! 8. No enabled provider supports `request.kind`, or all are out of quota
//!    (`ProviderUsage::blocked_until > now`) → `NoProvider`.
//!
//! Otherwise `Escalate { provider, budget }` with the first eligible provider in
//! `policy.providers` order and `budget = min(request.token_budget, per_task_max_tokens)`.
//!
//! # Routing score (`policy::route_score`, Kilo)
//!
//! A deterministic number in `0..=1`:
//!
//! ```text
//! base   = 0.35·complexity + 0.25·(1 − local_confidence) + 0.25·importance + 0.15·kind_prior
//! prior  = (improved + 1) / (improved + not_improved + 2)          // per kind, Laplace
//! score  = base · (0.5 + prior)  clamped to 0..=1
//! ```
//!
//! `complexity` is `request.complexity` (a hint in `0..=1`, clamped); `local_confidence`
//! defaults to 0.5 when absent; `importance` is Low 0.2, Normal 0.5, High 0.9;
//! `kind_prior` is Coding 0.8, Research 0.7, Reasoning 0.6, Planning 0.5, Writing 0.3. The
//! `(0.5 + prior)` factor means a kind whose escalations keep not helping drifts below
//! the threshold, and one that keeps helping drifts above.
//!
//! # Fingerprint (`policy::fingerprint`, Kilo)
//!
//! `sha256(kind | normalized question)` as lowercase hex, where normalization lowercases,
//! folds whitespace runs to one space, trims, and drops trailing `?.!`. Justification,
//! budget and origin do **not** change the fingerprint, so equivalent requests dedupe.
//!
//! # Redaction (`redact`, Kilo)
//!
//! Before anything leaves the host, [`redact::redact`] replaces e-mail addresses, phone
//! numbers (7+ digits with optional `+`, spaces, dashes), IPv4 addresses, paths under
//! `/home/` or `~/`, IBAN-like and card-like digit runs (12+ digits) and any extra
//! `known_names` with stable placeholders (`[EMAIL_1]`, `[PHONE_1]`, `[IP_1]`, `[PATH_1]`,
//! `[NUMBER_1]`, `[NAME_1]`), and then runs
//! `sail_guardian::credentials::redact_credential_patterns`. The same input value gets the
//! same placeholder within one request. [`redact::rehydrate`] restores the values in the
//! reply. Secrets are never restored (they are replaced, not mapped).
//!
//! # Store (`store::EscalationStore`, Kilo)
//!
//! SQLite under `sail_common::storage::data_path("mind")/escalation.db`, WAL, schema
//! version 1. Tables: `jobs` (one row per decided escalation, with state), `usage` (one
//! row per provider call: tokens, cost, provider, kind, at), `cache` (fingerprint →
//! answer, expires_at), `outcomes` (job → [`Outcome`]), `decisions` (every decision,
//! including denials, for observability). No prompt text is stored except the redacted
//! question in `jobs` and the redacted answer in `cache`, both bounded to 16 KiB.
//! Pending jobs survive a restart: `recover()` marks jobs that were `Running` as
//! `Pending` again so the service resubmits them, at most `MAX_ATTEMPTS` times.
//!
//! # Providers (`provider`, Kilo)
//!
//! [`provider::ExternalProvider`] is the boundary. [`provider::OpenAiCompatible`] calls
//! `POST {base_url}/chat/completions` with the API key from the environment variable the
//! config names (never a literal key in config), `max_tokens = budget`, a 120 s timeout,
//! and maps `usage.prompt_tokens`/`completion_tokens`. HTTP 429 or a body naming a rate
//! limit → `ProviderError::RateLimited { retry_after }`. Tests use a local fake server only.
//! Vendor CLIs (Codex, Claude Code, Gemini) are follow-up adapters behind the same trait.
//!
//! The answering model is always reported to the user. External output is untrusted text:
//! it is shown or fed back to the local model as a tool observation, never executed.

pub mod policy;
pub mod provider;
pub mod redact;
pub mod service;
pub mod store;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Longest question accepted for escalation, in characters.
pub const MAX_QUESTION_CHARS: usize = 12_000;
/// Longest justification or expected-benefit text, in characters.
pub const MAX_REASON_CHARS: usize = 600;
/// Most times a pending job is resubmitted after restarts or provider errors.
pub const MAX_ATTEMPTS: u32 = 3;
/// Stored redacted questions and answers are cut to this many bytes.
pub const MAX_STORED_BYTES: usize = 16 * 1024;

/// What kind of work is being escalated. Voice/dialogue turns are not a kind: they are
/// marked by [`Origin::VoiceTurn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Coding,
    Research,
    Reasoning,
    Planning,
    Writing,
}

impl TaskKind {
    pub const ALL: [TaskKind; 5] = [
        TaskKind::Coding,
        TaskKind::Research,
        TaskKind::Reasoning,
        TaskKind::Planning,
        TaskKind::Writing,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Coding => "coding",
            TaskKind::Research => "research",
            TaskKind::Reasoning => "reasoning",
            TaskKind::Planning => "planning",
            TaskKind::Writing => "writing",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Importance {
    Low,
    Normal,
    High,
}

/// Who asked. Set by trusted code, never by model-supplied JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The local model called `escalate_reasoning` during a typed (non-voice) turn.
    ModelTool,
    /// The local model called it during a live voice turn.
    VoiceTurn,
    /// Background work (Discovery investigation, Forge, research).
    Background,
    /// The owner explicitly asked ("ask a stronger model"). Skips attempts and threshold.
    Owner,
}

/// One request for a stronger model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EscalationRequest {
    pub kind: TaskKind,
    /// The self-contained question. Redacted before it leaves the host.
    pub question: String,
    /// Why the local model cannot answer well itself.
    pub justification: String,
    /// What a better answer would change.
    pub expected_benefit: String,
    /// Most output tokens the requester wants spent.
    pub token_budget: u32,
    pub importance: Importance,
    /// Hint in 0..=1 of how hard the problem is (model- or rule-supplied; clamped).
    pub complexity: f32,
    /// The local model's own confidence in 0..=1, if it gave one.
    pub local_confidence: Option<f32>,
    /// Local attempts already made at this task.
    pub local_attempts: u32,
    pub origin: Origin,
    /// Conversation or task this belongs to, so the result can be routed back.
    pub correlation_id: Option<String>,
}

/// Why an escalation was not made. Shown to the model and the owner verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    Disabled,
    VoiceTurn,
    OverTaskBudget,
    NotEnoughLocalAttempts,
    OverDailyTokens,
    OverMonthlyTokens,
    OverDailyCost,
    OverMonthlyCost,
    Cooldown,
    InFlight,
    BelowThreshold,
    NoProvider,
    Invalid,
    RedactionFailed,
}

impl DenyReason {
    pub fn explain(self) -> &'static str {
        match self {
            DenyReason::Disabled => "Escalation to stronger models is turned off.",
            DenyReason::VoiceTurn => "Live voice turns are not escalated.",
            DenyReason::OverTaskBudget => {
                "The requested token budget is outside the per-task limit."
            }
            DenyReason::NotEnoughLocalAttempts => {
                "Try locally first; escalation needs more local attempts."
            }
            DenyReason::OverDailyTokens => "Today's escalation token limit is reached.",
            DenyReason::OverMonthlyTokens => "This month's escalation token limit is reached.",
            DenyReason::OverDailyCost => "Today's escalation cost limit is reached.",
            DenyReason::OverMonthlyCost => "This month's escalation cost limit is reached.",
            DenyReason::Cooldown => "An equivalent question was escalated recently.",
            DenyReason::InFlight => "An equivalent question is already being escalated.",
            DenyReason::BelowThreshold => "The task does not justify a stronger model.",
            DenyReason::NoProvider => {
                "No external model with quota is configured for this kind of task."
            }
            DenyReason::Invalid => "The escalation request is invalid.",
            DenyReason::RedactionFailed => {
                "Private data could not be safely removed, so nothing was sent."
            }
        }
    }
}

/// The policy's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    Escalate { provider: String, budget: u32 },
    Cached { job_id: String },
    Deny { reason: DenyReason },
}

/// One configured external provider. Off unless listed in the policy file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Stable id shown to the owner, e.g. `openrouter-sonnet`.
    pub id: String,
    /// `openai_compatible` for now; CLI adapters are follow-ups.
    pub adapter: String,
    /// e.g. `https://openrouter.ai/api/v1`. Must be `https://` unless the host is loopback.
    pub base_url: String,
    /// Model name sent to the endpoint and shown as "answering model".
    pub model: String,
    /// Name of the environment variable holding the key. Never the key itself.
    pub api_key_env: String,
    /// Task kinds this provider may serve. Empty means all.
    #[serde(default)]
    pub kinds: Vec<TaskKind>,
    /// Estimated price per million input / output tokens, in micro-units of the owner's
    /// currency (1 € = 1_000_000). Used for cost caps before the call.
    #[serde(default)]
    pub input_price_micros_per_mtok: u64,
    #[serde(default)]
    pub output_price_micros_per_mtok: u64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Owner-configured limits. Loaded from `$SAIL_DATA/mind/escalation.json`; missing file
/// means the default, which is disabled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EscalationPolicy {
    pub enabled: bool,
    pub allow_voice: bool,
    pub providers: Vec<ProviderConfig>,
    /// 0 = no cap of this kind.
    pub daily_token_cap: u64,
    pub monthly_token_cap: u64,
    pub daily_cost_cap_micros: u64,
    pub monthly_cost_cap_micros: u64,
    pub per_task_max_tokens: u32,
    /// Seconds before an equivalent question may be escalated again.
    pub cooldown_secs: u64,
    /// Seconds a finished answer is reused for an equivalent question.
    pub cache_ttl_secs: u64,
    /// Minimum `route_score` to escalate.
    pub route_threshold: f32,
    /// Local attempts required per kind before a model/background escalation.
    pub min_local_attempts: u32,
    pub min_local_attempts_coding: u32,
}

impl Default for EscalationPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_voice: false,
            providers: Vec::new(),
            daily_token_cap: 50_000,
            monthly_token_cap: 1_000_000,
            daily_cost_cap_micros: 1_000_000,
            monthly_cost_cap_micros: 10_000_000,
            per_task_max_tokens: 8_000,
            cooldown_secs: 6 * 3600,
            cache_ttl_secs: 7 * 24 * 3600,
            route_threshold: 0.45,
            min_local_attempts: 1,
            min_local_attempts_coding: 2,
        }
    }
}

impl EscalationPolicy {
    pub fn min_local_attempts(&self, kind: TaskKind) -> u32 {
        if kind == TaskKind::Coding {
            self.min_local_attempts_coding
        } else {
            self.min_local_attempts
        }
    }
}

/// Usage totals the policy checks, computed by the store for `now`'s UTC day and month.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub tokens_today: u64,
    pub tokens_month: u64,
    pub cost_today_micros: u64,
    pub cost_month_micros: u64,
    pub calls_today: u64,
    /// Per provider id: until when it is out of quota after a rate-limit reply.
    pub blocked_until: Vec<(String, DateTime<Utc>)>,
    /// Fingerprints of jobs pending or running.
    pub in_flight: Vec<String>,
    /// Fingerprint → when its last escalation finished (within the cooldown window).
    pub recent: Vec<(String, DateTime<Utc>)>,
}

/// Whether an escalation helped. Given by the owner, or by a measurable check (tests now
/// pass, the local model adopted the answer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Improved,
    NoChange,
    Worse,
}

/// Per-kind tallies of [`Outcome`] used by `route_score`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutcomeStats {
    /// (kind, improved, not_improved). `NoChange` and `Worse` count as not improved.
    pub by_kind: Vec<(TaskKind, u32, u32)>,
}

impl OutcomeStats {
    pub fn tally(&self, kind: TaskKind) -> (u32, u32) {
        self.by_kind
            .iter()
            .find(|(k, _, _)| *k == kind)
            .map(|(_, i, n)| (*i, *n))
            .unwrap_or((0, 0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Pending,
    Running,
    Done,
    Failed,
}

/// A decided escalation and, once finished, its answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EscalationJob {
    pub id: String,
    pub fingerprint: String,
    pub kind: TaskKind,
    pub origin: Origin,
    pub provider: String,
    /// The answering model, shown to the owner.
    pub model: String,
    pub budget: u32,
    pub state: JobState,
    pub attempts: u32,
    /// Redacted question (bounded).
    pub question: String,
    pub justification: String,
    pub expected_benefit: String,
    /// Rehydrated answer when done (bounded).
    pub answer: Option<String>,
    pub error: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_micros: u64,
    pub correlation_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub outcome: Option<Outcome>,
}

/// One logged decision, including denials (observability).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub at: DateTime<Utc>,
    pub kind: TaskKind,
    pub origin: Origin,
    pub fingerprint: String,
    pub decision: Decision,
    pub route_score: f32,
    pub job_id: Option<String>,
}

/// Snapshot for `/api/mind/escalation/status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EscalationStatus {
    pub enabled: bool,
    pub providers: Vec<String>,
    pub usage: UsageTotals,
    pub daily_token_cap: u64,
    pub monthly_token_cap: u64,
    pub daily_cost_cap_micros: u64,
    pub monthly_cost_cap_micros: u64,
    pub outcomes: OutcomeStats,
    pub recent_decisions: Vec<DecisionRecord>,
}

/// Validate a request's shape before any policy check (`DenyReason::Invalid`).
pub fn validate(request: &EscalationRequest) -> Result<(), String> {
    let question = request.question.trim();
    if question.is_empty() {
        return Err("question is required".into());
    }
    if question.chars().count() > MAX_QUESTION_CHARS {
        return Err(format!(
            "question is longer than {MAX_QUESTION_CHARS} characters"
        ));
    }
    for (name, text) in [
        ("justification", &request.justification),
        ("expected_benefit", &request.expected_benefit),
    ] {
        if text.trim().is_empty() {
            return Err(format!("{name} is required"));
        }
        if text.chars().count() > MAX_REASON_CHARS {
            return Err(format!(
                "{name} is longer than {MAX_REASON_CHARS} characters"
            ));
        }
    }
    if !request.complexity.is_finite() || request.local_confidence.is_some_and(|c| !c.is_finite()) {
        return Err("complexity and local_confidence must be numbers".into());
    }
    Ok(())
}
