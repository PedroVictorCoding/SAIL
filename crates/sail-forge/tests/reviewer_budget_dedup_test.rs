use std::sync::Arc;
use sail_forge::contracts::{ReviewPolicyConfig, ReviewRequest};
use sail_forge::reviewer::ReviewBroker;
use sail_guardian::credentials::CredentialStore;
use sail_common::types::SubsystemId;

#[tokio::test]
async fn test_hash_deduplication_prevents_duplicate_queries() {
    let config = ReviewPolicyConfig {
        enabled: true,
        max_reviews_per_issue: 2,
        max_requests_per_day: 3,
        max_requests_per_week: 10,
        max_cost_per_day_usd: 2.00,
        max_cost_per_month_usd: 10.00,
        max_input_tokens_per_review: 20_000,
        max_output_tokens_per_review: 4_000,
        model: "gpt-4o-mini".into(),
        dedup_cache_enabled: true,
    };

    let broker = ReviewBroker::new(config).with_mock_mode(true);

    let req = ReviewRequest {
        job_id: "job-dedup-1".into(),
        issue_id: "PedroVictorCoding/SAIL/#90".into(),
        base_commit: "83b5762".into(),
        diff_patch: "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n".into(),
        test_manifest: "test result: ok. 10 passed; 0 failed".into(),
    };

    // First call: execute mock review
    let res1 = broker.review(&req).await;
    assert!(!res1.cached);
    assert_eq!(res1.provider, "openai_mock");
    assert!(res1.cost_usd > 0.0);

    let ledger1 = broker.get_ledger();
    assert_eq!(ledger1.requests_today, 1);

    // Second call with identical payload: must be served from dedup cache without new API query or additional charge!
    let res2 = broker.review(&req).await;
    assert!(res2.cached);
    assert_eq!(res2.dedup_hash, res1.dedup_hash);

    let ledger2 = broker.get_ledger();
    // Request count and cost must remain unchanged
    assert_eq!(ledger2.requests_today, 1);
    assert_eq!(ledger2.cost_today_usd, ledger1.cost_today_usd);
}

#[tokio::test]
async fn test_daily_and_per_issue_budget_caps_trigger_local_fallback() {
    let config = ReviewPolicyConfig {
        enabled: true,
        max_reviews_per_issue: 1, // Only 1 review per issue (max 2 with material revision)
        max_requests_per_day: 2,  // Max 2 requests per day
        max_requests_per_week: 10,
        max_cost_per_day_usd: 2.00,
        max_cost_per_month_usd: 10.00,
        max_input_tokens_per_review: 20_000,
        max_output_tokens_per_review: 4_000,
        model: "gpt-4o-mini".into(),
        dedup_cache_enabled: false,
    };

    let broker = ReviewBroker::new(config).with_mock_mode(true);

    let req1 = ReviewRequest {
        job_id: "job-cap-1".into(),
        issue_id: "PedroVictorCoding/SAIL/#90".into(),
        base_commit: "83b5762".into(),
        diff_patch: "diff 1".into(),
        test_manifest: "test result: ok".into(),
    };

    // Review 1 on Issue 90: succeeds (count = 1)
    let res1 = broker.review(&req1).await;
    assert_eq!(res1.provider, "openai_mock");

    // Review 2 on Issue 90 with material revision: succeeds (count = 2)
    let req2 = ReviewRequest {
        job_id: "job-cap-2".into(),
        issue_id: "PedroVictorCoding/SAIL/#90".into(),
        base_commit: "83b5762".into(),
        diff_patch: "diff 2 with material revision".into(),
        test_manifest: "test result: ok".into(),
    };

    let res2 = broker.review(&req2).await;
    assert_eq!(res2.provider, "openai_mock");

    // Review 3 on Issue 90: exceeds per-issue limit (max 2) AND daily request cap (max 2)
    let req3 = ReviewRequest {
        job_id: "job-cap-3".into(),
        issue_id: "PedroVictorCoding/SAIL/#90".into(),
        base_commit: "83b5762".into(),
        diff_patch: "diff 3 attempt".into(),
        test_manifest: "test result: ok".into(),
    };

    let res3 = broker.review(&req3).await;
    // Must gracefully fall back to local review without failing the job!
    assert!(res3.provider.starts_with("local_deterministic_review:quota_capped"));
    assert_eq!(res3.cost_usd, 0.0);
    assert!(res3.verdict.passed);
}

#[tokio::test]
async fn test_missing_api_key_gracefully_falls_back_to_local_review() {
    let config = ReviewPolicyConfig {
        enabled: true,
        max_reviews_per_issue: 1,
        max_requests_per_day: 3,
        max_requests_per_week: 10,
        max_cost_per_day_usd: 2.00,
        max_cost_per_month_usd: 10.00,
        max_input_tokens_per_review: 20_000,
        max_output_tokens_per_review: 4_000,
        model: "gpt-4o-mini".into(),
        dedup_cache_enabled: true,
    };

    // Not in mock mode and no API key in environment or store
    let broker = ReviewBroker::new(config).with_mock_mode(false);

    let req = ReviewRequest {
        job_id: "job-offline-1".into(),
        issue_id: "PedroVictorCoding/SAIL/#90".into(),
        base_commit: "83b5762".into(),
        diff_patch: "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n".into(),
        test_manifest: "test result: ok. 5 passed; 0 failed".into(),
    };

    let res = broker.review(&req).await;
    // Missing key must never block local issue solving
    assert!(res.provider.starts_with("local_deterministic_review:missing_api_key"));
    assert_eq!(res.cost_usd, 0.0);
    assert!(res.verdict.passed);
}

#[tokio::test]
async fn test_secrets_redacted_before_review_dispatch() {
    let cred_store = Arc::new(CredentialStore::new());
    cred_store.set_secret(SubsystemId::Forge, "openai_api_key", "sk-proj-test1234567890abcdef");
    cred_store.set_secret(SubsystemId::Bridge, "github_token", "ghp_super_secret_github_token");

    let config = ReviewPolicyConfig::default();
    let broker = ReviewBroker::new(config).with_credentials(cred_store);

    let raw_diff = "diff --git a/config.rs b/config.rs\n+let token = \"ghp_super_secret_github_token\";\n+let key = \"sk-proj-test1234567890abcdef\";\n";
    let sanitized = broker.sanitize_payload(raw_diff);

    // Verify secrets are strictly redacted
    assert!(!sanitized.contains("ghp_super_secret_github_token"));
    assert!(!sanitized.contains("sk-proj-test1234567890abcdef"));
    assert!(sanitized.contains("[REDACTED_SECRET]"));
}
