//! Deterministic escalation policy (Kilo). Spec: the module doc in `escalation/mod.rs`.
use super::{Decision, EscalationPolicy, EscalationRequest, OutcomeStats, TaskKind, UsageTotals};
use chrono::{DateTime, Utc};

/// Stable dedup key: `sha256(kind | normalized question)` as lowercase hex.
pub fn fingerprint(kind: TaskKind, question: &str) -> String {
    let _ = (kind, question);
    unimplemented!("Kilo: escalation::policy::fingerprint")
}

/// The routing score in `0..=1` (see the formula in the module doc).
pub fn route_score(request: &EscalationRequest, outcomes: &OutcomeStats) -> f32 {
    let _ = (request, outcomes);
    unimplemented!("Kilo: escalation::policy::route_score")
}

/// Estimated worst-case cost of a call, in micro-units, for the cost caps: input tokens
/// estimated as `question.len() / 4 + 200`, output tokens as `budget`.
pub fn estimate_cost_micros(provider: &super::ProviderConfig, question: &str, budget: u32) -> u64 {
    let _ = (provider, question, budget);
    unimplemented!("Kilo: escalation::policy::estimate_cost_micros")
}

/// Apply the ordered checks. Never returns `Decision::Cached` (the service handles the cache).
pub fn decide(
    request: &EscalationRequest,
    policy: &EscalationPolicy,
    usage: &UsageTotals,
    outcomes: &OutcomeStats,
    now: DateTime<Utc>,
) -> Decision {
    let _ = (request, policy, usage, outcomes, now);
    unimplemented!("Kilo: escalation::policy::decide")
}
