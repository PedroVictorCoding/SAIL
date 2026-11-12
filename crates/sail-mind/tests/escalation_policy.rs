//! Policy: fingerprint, routing score, cost estimate and the ordered deny checks.

mod escalation_fixtures;

use chrono::{Duration, Utc};
use escalation_fixtures::{open_policy, provider, request};
use sail_mind::escalation::policy::{decide, estimate_cost_micros, fingerprint, route_score};
use sail_mind::escalation::{
    Decision, DenyReason, EscalationRequest, Importance, Origin, OutcomeStats, TaskKind,
    UsageTotals,
};

fn reasons(
    request: &EscalationRequest,
    policy: &sail_mind::escalation::EscalationPolicy,
) -> Decision {
    decide(
        request,
        policy,
        &UsageTotals::default(),
        &OutcomeStats::default(),
        Utc::now(),
    )
}

#[test]
fn fingerprint_ignores_case_whitespace_and_trailing_punctuation() {
    let base = fingerprint(TaskKind::Coding, "How do I fix this bug?");
    assert_eq!(
        base,
        fingerprint(TaskKind::Coding, "  how DO i   fix this bug  ")
    );
    assert_eq!(
        base,
        fingerprint(TaskKind::Coding, "how do i fix this bug!")
    );
    assert_eq!(
        base,
        fingerprint(TaskKind::Coding, "how do i fix this bug...")
    );
    assert_eq!(base.len(), 64);
    assert!(
        base.chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
    );
    assert_ne!(
        base,
        fingerprint(TaskKind::Reasoning, "How do I fix this bug?")
    );
    assert_ne!(
        base,
        fingerprint(TaskKind::Coding, "How do I fix this other bug?")
    );
}

#[test]
fn fingerprint_ignores_budget_origin_and_justification() {
    let first = request(TaskKind::Coding);
    let mut second = first.clone();
    second.token_budget = 4_000;
    second.origin = Origin::Owner;
    second.justification = "different".into();
    assert_eq!(
        fingerprint(first.kind, &first.question),
        fingerprint(second.kind, &second.question)
    );
}

#[test]
fn route_score_is_monotone_in_complexity_confidence_and_importance() {
    let outcomes = OutcomeStats::default();
    let low = request(TaskKind::Reasoning);
    let mut high = low.clone();
    high.complexity = 0.95;
    assert!(route_score(&high, &outcomes) > route_score(&low, &outcomes));

    let mut confident = low.clone();
    confident.local_confidence = Some(0.95);
    assert!(route_score(&confident, &outcomes) < route_score(&low, &outcomes));

    let mut important = low.clone();
    important.importance = Importance::High;
    let mut unimportant = low.clone();
    unimportant.importance = Importance::Low;
    assert!(route_score(&important, &outcomes) > route_score(&low, &outcomes));
    assert!(route_score(&low, &outcomes) > route_score(&unimportant, &outcomes));
}

#[test]
fn route_score_is_moved_by_outcomes_and_stays_in_range() {
    let request = request(TaskKind::Coding);
    let empty = OutcomeStats::default();
    let helping = OutcomeStats {
        by_kind: vec![(TaskKind::Coding, 8, 0)],
    };
    let not_helping = OutcomeStats {
        by_kind: vec![(TaskKind::Coding, 0, 8)],
    };
    let baseline = route_score(&request, &empty);
    assert!(route_score(&request, &helping) > baseline);
    assert!(route_score(&request, &not_helping) < baseline);

    let mut extreme = request;
    extreme.complexity = 1.0;
    extreme.local_confidence = Some(0.0);
    extreme.importance = Importance::High;
    for outcomes in [&helping, &empty, &not_helping] {
        let score = route_score(&extreme, outcomes);
        assert!((0.0..=1.0).contains(&score), "score {score} escaped 0..=1");
    }
}

#[test]
fn route_score_defaults_missing_confidence_to_one_half() {
    let outcomes = OutcomeStats::default();
    let with_half = {
        let mut request = request(TaskKind::Research);
        request.local_confidence = Some(0.5);
        route_score(&request, &outcomes)
    };
    let without = {
        let mut request = request(TaskKind::Research);
        request.local_confidence = None;
        route_score(&request, &outcomes)
    };
    assert!((with_half - without).abs() < 1e-6);
}

#[test]
fn cost_estimate_uses_question_length_and_budget() {
    let mut provider = provider("costed");
    provider.input_price_micros_per_mtok = 3_000_000;
    provider.output_price_micros_per_mtok = 15_000_000;
    let question = "x".repeat(400);
    // input = 400/4 + 200 = 300 tokens; output = 1000 tokens.
    // (300*3 + 1000*15) micros = 15_900 micros.
    assert_eq!(estimate_cost_micros(&provider, &question, 1_000), 15_900);
    assert_eq!(estimate_cost_micros(&provider, &question, 0), 900);
}

#[test]
fn budget_must_fit_the_task_limit_and_the_answer_uses_the_remainder() {
    let mut policy = open_policy();
    policy.per_task_max_tokens = 2_000;
    let mut request = request(TaskKind::Writing);
    request.token_budget = 1_500;
    assert_eq!(
        reasons(&request, &policy),
        Decision::Escalate {
            provider: "primary".into(),
            budget: 1_500,
        }
    );
    request.token_budget = 2_001;
    assert_eq!(
        reasons(&request, &policy),
        Decision::Deny {
            reason: DenyReason::OverTaskBudget
        }
    );
    request.token_budget = 0;
    assert_eq!(
        reasons(&request, &policy),
        Decision::Deny {
            reason: DenyReason::OverTaskBudget
        }
    );
}

#[test]
fn every_policy_deny_reason_is_reachable_in_order() {
    let mut policy = open_policy();
    let request = request(TaskKind::Coding);

    let mut disabled = policy.clone();
    disabled.enabled = false;
    assert_eq!(
        reasons(&request, &disabled),
        Decision::Deny {
            reason: DenyReason::Disabled
        }
    );

    let mut voice = request.clone();
    voice.origin = Origin::VoiceTurn;
    assert_eq!(
        reasons(&voice, &policy),
        Decision::Deny {
            reason: DenyReason::VoiceTurn
        }
    );
    policy.allow_voice = true;
    assert_eq!(
        reasons(&voice, &policy),
        Decision::Escalate {
            provider: "primary".into(),
            budget: 1_000,
        }
    );
    policy.allow_voice = false;

    let mut few_attempts = request.clone();
    few_attempts.local_attempts = 0;
    let mut attempts_policy = open_policy();
    attempts_policy.min_local_attempts = 1;
    attempts_policy.min_local_attempts_coding = 2;
    assert_eq!(
        reasons(&few_attempts, &attempts_policy),
        Decision::Deny {
            reason: DenyReason::NotEnoughLocalAttempts
        }
    );
    few_attempts.local_attempts = 2;
    assert_eq!(
        reasons(&few_attempts, &attempts_policy),
        Decision::Escalate {
            provider: "primary".into(),
            budget: 1_000,
        }
    );

    let now = Utc::now();
    let usage = UsageTotals {
        tokens_today: 4_000,
        tokens_month: 4_000,
        ..UsageTotals::default()
    };
    let mut capped = open_policy();
    capped.daily_token_cap = 4_500;
    assert_eq!(
        decide(&request, &capped, &usage, &OutcomeStats::default(), now),
        Decision::Deny {
            reason: DenyReason::OverDailyTokens
        }
    );
    capped.daily_token_cap = 0;
    capped.monthly_token_cap = 4_500;
    assert_eq!(
        decide(&request, &capped, &usage, &OutcomeStats::default(), now),
        Decision::Deny {
            reason: DenyReason::OverMonthlyTokens
        }
    );

    let mut cost_capped = open_policy();
    cost_capped.daily_cost_cap_micros = 100;
    assert_eq!(
        decide(
            &request,
            &cost_capped,
            &UsageTotals::default(),
            &OutcomeStats::default(),
            now
        ),
        Decision::Deny {
            reason: DenyReason::OverDailyCost
        }
    );
    cost_capped.daily_cost_cap_micros = 0;
    cost_capped.monthly_cost_cap_micros = 100;
    assert_eq!(
        decide(
            &request,
            &cost_capped,
            &UsageTotals::default(),
            &OutcomeStats::default(),
            now
        ),
        Decision::Deny {
            reason: DenyReason::OverMonthlyCost
        }
    );

    let mut cooling = open_policy();
    cooling.cooldown_secs = 3_600;
    let mut recent = UsageTotals {
        recent: vec![(
            fingerprint(request.kind, &request.question),
            now - Duration::seconds(60),
        )],
        ..UsageTotals::default()
    };
    assert_eq!(
        decide(&request, &cooling, &recent, &OutcomeStats::default(), now),
        Decision::Deny {
            reason: DenyReason::Cooldown
        }
    );
    recent.recent = vec![(
        fingerprint(request.kind, &request.question),
        now - Duration::hours(2),
    )];
    assert_eq!(
        decide(&request, &cooling, &recent, &OutcomeStats::default(), now),
        Decision::Escalate {
            provider: "primary".into(),
            budget: 1_000,
        }
    );

    let in_flight = UsageTotals {
        in_flight: vec![fingerprint(request.kind, &request.question)],
        ..UsageTotals::default()
    };
    assert_eq!(
        decide(
            &request,
            &open_policy(),
            &in_flight,
            &OutcomeStats::default(),
            now
        ),
        Decision::Deny {
            reason: DenyReason::InFlight
        }
    );

    let mut threshold = open_policy();
    threshold.route_threshold = 0.99;
    let mut low = request.clone();
    low.complexity = 0.0;
    low.importance = Importance::Low;
    low.local_confidence = Some(1.0);
    assert_eq!(
        reasons(&low, &threshold),
        Decision::Deny {
            reason: DenyReason::BelowThreshold
        }
    );

    let mut no_provider = open_policy();
    no_provider.providers.clear();
    assert_eq!(
        reasons(&request, &no_provider),
        Decision::Deny {
            reason: DenyReason::NoProvider
        }
    );
}

#[test]
fn owner_skips_attempts_and_threshold_but_not_budgets() {
    let mut policy = open_policy();
    policy.route_threshold = 0.99;
    policy.min_local_attempts = 9;
    policy.min_local_attempts_coding = 9;
    let mut owner = request(TaskKind::Coding);
    owner.origin = Origin::Owner;
    owner.local_attempts = 0;
    owner.complexity = 0.0;
    owner.local_confidence = Some(1.0);
    owner.importance = Importance::Low;
    assert_eq!(
        reasons(&owner, &policy),
        Decision::Escalate {
            provider: "primary".into(),
            budget: 1_000,
        }
    );
    owner.token_budget = policy.per_task_max_tokens + 1;
    assert_eq!(
        reasons(&owner, &policy),
        Decision::Deny {
            reason: DenyReason::OverTaskBudget
        }
    );
    policy.daily_token_cap = 10;
    let usage = UsageTotals {
        tokens_today: 10,
        ..UsageTotals::default()
    };
    owner.token_budget = 1_000;
    assert_eq!(
        decide(
            &owner,
            &policy,
            &usage,
            &OutcomeStats::default(),
            Utc::now()
        ),
        Decision::Deny {
            reason: DenyReason::OverDailyTokens
        }
    );
}

#[test]
fn caps_of_zero_mean_unlimited() {
    let policy = open_policy();
    let request = request(TaskKind::Research);
    let usage = UsageTotals {
        tokens_today: u64::MAX / 2,
        tokens_month: u64::MAX / 2,
        cost_today_micros: u64::MAX / 2,
        cost_month_micros: u64::MAX / 2,
        calls_today: 1_000_000,
        ..UsageTotals::default()
    };
    assert_eq!(
        decide(
            &request,
            &policy,
            &usage,
            &OutcomeStats::default(),
            Utc::now()
        ),
        Decision::Escalate {
            provider: "primary".into(),
            budget: 1_000,
        }
    );
}

#[test]
fn first_eligible_provider_in_policy_order_is_used_and_rate_limits_are_skipped() {
    let request = request(TaskKind::Research);
    let now = Utc::now();
    let mut policy = open_policy();
    policy.providers = vec![provider("first"), provider("second")];
    assert_eq!(
        decide(
            &request,
            &policy,
            &UsageTotals::default(),
            &OutcomeStats::default(),
            now
        ),
        Decision::Escalate {
            provider: "first".into(),
            budget: 1_000,
        }
    );

    let mut limited = UsageTotals::default();
    limited
        .blocked_until
        .push(("first".into(), now + Duration::minutes(5)));
    assert_eq!(
        decide(&request, &policy, &limited, &OutcomeStats::default(), now),
        Decision::Escalate {
            provider: "second".into(),
            budget: 1_000,
        }
    );

    limited
        .blocked_until
        .push(("second".into(), now + Duration::minutes(5)));
    assert_eq!(
        decide(&request, &policy, &limited, &OutcomeStats::default(), now),
        Decision::Deny {
            reason: DenyReason::NoProvider
        }
    );

    let mut expired = UsageTotals::default();
    expired
        .blocked_until
        .push(("first".into(), now - Duration::minutes(1)));
    assert_eq!(
        decide(&request, &policy, &expired, &OutcomeStats::default(), now),
        Decision::Escalate {
            provider: "first".into(),
            budget: 1_000,
        }
    );
}

#[test]
fn providers_only_serve_their_configured_kinds() {
    let request = request(TaskKind::Writing);
    let mut policy = open_policy();
    let mut coding_only = provider("coding-only");
    coding_only.kinds = vec![TaskKind::Coding];
    policy.providers = vec![coding_only.clone()];
    assert_eq!(
        reasons(&request, &policy),
        Decision::Deny {
            reason: DenyReason::NoProvider
        }
    );

    let mut disabled = coding_only;
    disabled.enabled = false;
    disabled.kinds = Vec::new();
    policy.providers = vec![disabled];
    assert_eq!(
        reasons(&request, &policy),
        Decision::Deny {
            reason: DenyReason::NoProvider
        }
    );
}
