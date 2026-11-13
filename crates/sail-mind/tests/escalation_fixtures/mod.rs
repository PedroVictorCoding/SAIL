//! Shared fixtures for the escalation integration tests.
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use sail_mind::escalation::{
    EscalationPolicy, EscalationRequest, Importance, Origin, ProviderConfig, TaskKind,
};

pub fn request(kind: TaskKind) -> EscalationRequest {
    EscalationRequest {
        kind,
        question: "How should I structure the retry logic in the sync client?".to_string(),
        justification: "The local model's first answers did not settle the design.".to_string(),
        expected_benefit: "A design that does not duplicate requests after a timeout.".to_string(),
        token_budget: 1_000,
        importance: Importance::Normal,
        complexity: 0.8,
        local_confidence: Some(0.3),
        local_attempts: 3,
        origin: Origin::ModelTool,
        correlation_id: None,
    }
}

pub fn provider(id: &str) -> ProviderConfig {
    ProviderConfig {
        id: id.to_string(),
        adapter: "openai_compatible".to_string(),
        base_url: "https://example.invalid/v1".to_string(),
        model: format!("{id}-model"),
        api_key_env: "SAIL_TEST_ESCALATION_KEY".to_string(),
        kinds: Vec::new(),
        input_price_micros_per_mtok: 1_000_000,
        output_price_micros_per_mtok: 1_000_000,
        enabled: true,
        command: None,
        cli_unverified_ok: false,
    }
}

/// Enabled, no caps, no cooldown, no attempts required: every check after 1 passes.
pub fn open_policy() -> EscalationPolicy {
    EscalationPolicy {
        enabled: true,
        allow_voice: false,
        providers: vec![provider("primary")],
        daily_token_cap: 0,
        monthly_token_cap: 0,
        daily_cost_cap_micros: 0,
        monthly_cost_cap_micros: 0,
        per_task_max_tokens: 4_000,
        cooldown_secs: 0,
        cache_ttl_secs: 0,
        route_threshold: 0.0,
        min_local_attempts: 0,
        min_local_attempts_coding: 0,
    }
}

pub fn at(seconds_ago: i64) -> DateTime<Utc> {
    Utc::now() - chrono::Duration::seconds(seconds_ago)
}
