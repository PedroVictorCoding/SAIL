//! Deterministic escalation policy (Kilo). Spec: the module doc in `escalation/mod.rs`.
use super::{
    Decision, DenyReason, EscalationPolicy, EscalationRequest, Importance, Origin, OutcomeStats,
    ProviderConfig, TaskKind, UsageTotals,
};
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};

/// Stable dedup key: `sha256(kind | normalized question)` as lowercase hex.
pub fn fingerprint(kind: TaskKind, question: &str) -> String {
    let normalized = normalize(question);
    let mut hasher = Sha256::new();
    hasher.update(kind.as_str().as_bytes());
    hasher.update(b"|");
    hasher.update(normalized.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn normalize(question: &str) -> String {
    let mut folded = String::with_capacity(question.len());
    for word in question.split_whitespace() {
        if !folded.is_empty() {
            folded.push(' ');
        }
        folded.push_str(&word.to_lowercase());
    }
    folded
        .trim_end_matches(['?', '.', '!'])
        .trim_end()
        .to_owned()
}

/// The routing score in `0..=1` (see the formula in the module doc).
pub fn route_score(request: &EscalationRequest, outcomes: &OutcomeStats) -> f32 {
    let complexity = clamp01(request.complexity);
    let confidence = clamp01(request.local_confidence.unwrap_or(0.5));
    let importance = match request.importance {
        Importance::Low => 0.2,
        Importance::Normal => 0.5,
        Importance::High => 0.9,
    };
    let kind_prior = match request.kind {
        TaskKind::Coding => 0.8,
        TaskKind::Research => 0.7,
        TaskKind::Reasoning => 0.6,
        TaskKind::Planning => 0.5,
        TaskKind::Writing => 0.3,
    };
    let (improved, not_improved) = outcomes.tally(request.kind);
    let prior = (improved as f32 + 1.0) / (improved as f32 + not_improved as f32 + 2.0);
    let base =
        0.35 * complexity + 0.25 * (1.0 - confidence) + 0.25 * importance + 0.15 * kind_prior;
    (base * (0.5 + prior)).clamp(0.0, 1.0)
}

fn clamp01(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

/// Estimated worst-case cost of a call, in micro-units, for the cost caps: input tokens
/// estimated as `question.len() / 4 + 200`, output tokens as `budget`.
pub fn estimate_cost_micros(provider: &ProviderConfig, question: &str, budget: u32) -> u64 {
    let input_tokens = (question.len() as u64) / 4 + 200;
    let output_tokens = u64::from(budget);
    let input_cost = provider
        .input_price_micros_per_mtok
        .saturating_mul(input_tokens);
    let output_cost = provider
        .output_price_micros_per_mtok
        .saturating_mul(output_tokens);
    input_cost.saturating_add(output_cost) / 1_000_000
}

/// Apply the ordered checks. Never returns `Decision::Cached` (the service handles the cache).
pub fn decide(
    request: &EscalationRequest,
    policy: &EscalationPolicy,
    usage: &UsageTotals,
    outcomes: &OutcomeStats,
    now: DateTime<Utc>,
) -> Decision {
    if !policy.enabled {
        return deny(DenyReason::Disabled);
    }
    if request.origin == Origin::VoiceTurn && !policy.allow_voice {
        return deny(DenyReason::VoiceTurn);
    }
    if request.token_budget == 0 || request.token_budget > policy.per_task_max_tokens {
        return deny(DenyReason::OverTaskBudget);
    }
    let owner = request.origin == Origin::Owner;
    if !owner && request.local_attempts < policy.min_local_attempts(request.kind) {
        return deny(DenyReason::NotEnoughLocalAttempts);
    }

    let candidates: Vec<&ProviderConfig> = policy
        .providers
        .iter()
        .filter(|provider| provider.enabled && serves(provider, request.kind))
        .collect();
    let available = candidates
        .iter()
        .copied()
        .find(|provider| !is_blocked(provider, usage, now));
    let estimate_provider = available.or_else(|| candidates.first().copied());
    let estimated_cost = estimate_provider
        .map(|provider| estimate_cost_micros(provider, &request.question, request.token_budget))
        .unwrap_or(0);

    let budget = u64::from(request.token_budget);
    if policy.daily_token_cap != 0
        && usage.tokens_today.saturating_add(budget) > policy.daily_token_cap
    {
        return deny(DenyReason::OverDailyTokens);
    }
    if policy.monthly_token_cap != 0
        && usage.tokens_month.saturating_add(budget) > policy.monthly_token_cap
    {
        return deny(DenyReason::OverMonthlyTokens);
    }
    if policy.daily_cost_cap_micros != 0
        && usage.cost_today_micros.saturating_add(estimated_cost) > policy.daily_cost_cap_micros
    {
        return deny(DenyReason::OverDailyCost);
    }
    if policy.monthly_cost_cap_micros != 0
        && usage.cost_month_micros.saturating_add(estimated_cost) > policy.monthly_cost_cap_micros
    {
        return deny(DenyReason::OverMonthlyCost);
    }

    let fingerprint = fingerprint(request.kind, &request.question);
    if policy.cooldown_secs != 0 {
        let cooldown = Duration::seconds(i64::try_from(policy.cooldown_secs).unwrap_or(i64::MAX));
        let cooling = usage.recent.iter().any(|(recent, finished)| {
            recent == &fingerprint && now.signed_duration_since(*finished) < cooldown
        });
        if cooling {
            return deny(DenyReason::Cooldown);
        }
    }
    if usage
        .in_flight
        .iter()
        .any(|in_flight| in_flight == &fingerprint)
    {
        return deny(DenyReason::InFlight);
    }

    if !owner && route_score(request, outcomes) < policy.route_threshold {
        return deny(DenyReason::BelowThreshold);
    }

    let Some(provider) = available else {
        return deny(DenyReason::NoProvider);
    };
    Decision::Escalate {
        provider: provider.id.clone(),
        budget: request.token_budget.min(policy.per_task_max_tokens),
    }
}

fn deny(reason: DenyReason) -> Decision {
    Decision::Deny { reason }
}

fn serves(provider: &ProviderConfig, kind: TaskKind) -> bool {
    provider.kinds.is_empty() || provider.kinds.contains(&kind)
}

fn is_blocked(provider: &ProviderConfig, usage: &UsageTotals, now: DateTime<Utc>) -> bool {
    usage
        .blocked_until
        .iter()
        .any(|(id, until)| id == &provider.id && *until > now)
}
