use std::fs;
use std::sync::Arc;
use uuid::Uuid;

use sail_common::types::SubsystemId;
use sail_forge::contracts::{ReviewPolicyConfig, ReviewRequest};
use sail_forge::reviewer::ReviewBroker;
use sail_guardian::credentials::CredentialStore;

#[tokio::test]
async fn test_twenty_parallel_review_requests_for_same_issue_invokes_at_most_once() {
    let temp_ledger = std::env::temp_dir().join(format!("test_ledger_{}.json", Uuid::new_v4()));
    let config = ReviewPolicyConfig {
        enabled: true,
        max_reviews_per_issue: 1,
        max_requests_per_day: 10,
        max_requests_per_week: 20,
        max_cost_per_day_usd: 2.00,
        max_cost_per_month_usd: 10.00,
        max_input_tokens_per_review: 20_000,
        max_output_tokens_per_review: 4_000,
        model: "gpt-4o-mini".into(),
        dedup_cache_enabled: true,
    };

    let broker = Arc::new(
        ReviewBroker::new(config)
            .with_ledger_path(temp_ledger.clone())
            .with_mock_mode(true),
    );

    // Spawn 20 parallel concurrent tasks attempting review for the EXACT SAME canonical issue
    let mut handles = Vec::new();
    for i in 0..20 {
        let b = broker.clone();
        let req = ReviewRequest {
            job_id: format!("job-race-{}", i),
            issue_id: "PedroVictorCoding/SAIL/#90".into(),
            base_commit: "83b5762".into(),
            diff_patch: "+++ b/src/lib.rs\n+// Verified solution".into(),
            test_manifest: "test result: ok".into(),
        };
        handles.push(tokio::spawn(async move { b.review(&req).await }));
    }

    let mut successful_results = 0;
    for h in handles {
        let res = h.await.unwrap();
        assert!(res.verdict.passed);
        successful_results += 1;
    }

    // All 20 requests received review results cleanly
    assert_eq!(successful_results, 20);

    // Strict P0 #96 Contract: AT MOST ONE process invocation occurred across all 20 concurrent requests!
    assert_eq!(
        broker.invocation_count(),
        1,
        "Must invoke reviewer process at most once for the same issue"
    );

    let ledger = broker.get_ledger();
    assert_eq!(ledger.requests_today, 1);
    assert_eq!(
        ledger
            .issue_review_counts
            .get("PedroVictorCoding/SAIL#90")
            .copied()
            .unwrap_or(0),
        1
    );
    assert!(
        ledger
            .canonical_issue_reviews
            .contains_key("PedroVictorCoding/SAIL#90")
    );

    let _ = fs::remove_file(&temp_ledger);
}

#[tokio::test]
async fn test_ledger_persists_and_survives_broker_reboot() {
    let temp_ledger =
        std::env::temp_dir().join(format!("test_reboot_ledger_{}.json", Uuid::new_v4()));
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
        dedup_cache_enabled: false,
    };

    // Session 1: Execute 2 reviews then shut down broker
    {
        let broker = ReviewBroker::new(config.clone())
            .with_ledger_path(temp_ledger.clone())
            .with_mock_mode(true);

        let req1 = ReviewRequest {
            job_id: "job-1".into(),
            issue_id: "repo/#1".into(),
            base_commit: "83b5762".into(),
            diff_patch: "diff 1".into(),
            test_manifest: "test ok".into(),
        };
        let _ = broker.review(&req1).await;

        let req2 = ReviewRequest {
            job_id: "job-2".into(),
            issue_id: "repo/#2".into(),
            base_commit: "83b5762".into(),
            diff_patch: "diff 2".into(),
            test_manifest: "test ok".into(),
        };
        let _ = broker.review(&req2).await;

        let ledger = broker.get_ledger();
        assert_eq!(ledger.requests_today, 2);
    }

    // Session 2: Reboot! New broker instance reading the same ledger path
    {
        let reboot_broker = ReviewBroker::new(config.clone())
            .with_ledger_path(temp_ledger.clone())
            .with_mock_mode(true);

        let ledger = reboot_broker.get_ledger();
        assert_eq!(ledger.requests_today, 2, "Usage ledger must survive reboot");

        // 3rd review succeeds
        let req3 = ReviewRequest {
            job_id: "job-3".into(),
            issue_id: "repo/#3".into(),
            base_commit: "83b5762".into(),
            diff_patch: "diff 3".into(),
            test_manifest: "test ok".into(),
        };
        let res3 = reboot_broker.review(&req3).await;
        assert_eq!(res3.provider, "codex-cli:mock");

        // 4th review hits daily cap (max 3/day) and falls back
        let req4 = ReviewRequest {
            job_id: "job-4".into(),
            issue_id: "repo/#4".into(),
            base_commit: "83b5762".into(),
            diff_patch: "diff 4".into(),
            test_manifest: "test ok".into(),
        };
        let res4 = reboot_broker.review(&req4).await;
        assert!(res4.provider.contains("daily_request_limit_exceeded"));
    }

    let _ = fs::remove_file(&temp_ledger);
}

#[tokio::test]
async fn test_hash_deduplication_prevents_duplicate_queries() {
    let temp_ledger =
        std::env::temp_dir().join(format!("test_dedup_ledger_{}.json", Uuid::new_v4()));
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

    let broker = ReviewBroker::new(config)
        .with_ledger_path(temp_ledger.clone())
        .with_mock_mode(true);

    let req = ReviewRequest {
        job_id: "job-dedup-1".into(),
        issue_id: "PedroVictorCoding/SAIL/#90".into(),
        base_commit: "83b5762".into(),
        diff_patch: "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n".into(),
        test_manifest: "test result: ok. 10 passed; 0 failed".into(),
    };

    let res1 = broker.review(&req).await;
    assert!(!res1.cached);
    assert_eq!(res1.provider, "codex-cli:mock");

    let ledger1 = broker.get_ledger();
    assert_eq!(ledger1.requests_today, 1);

    // Second call with identical payload: served from dedup cache
    let res2 = broker.review(&req).await;
    assert!(res2.cached);
    assert_eq!(res2.dedup_hash, res1.dedup_hash);

    let ledger2 = broker.get_ledger();
    assert_eq!(ledger2.requests_today, 1);
    assert_eq!(ledger2.cost_today_usd, ledger1.cost_today_usd);

    let _ = fs::remove_file(&temp_ledger);
}

#[tokio::test]
async fn test_honest_local_fallback_does_not_falsely_certify_success() {
    let temp_ledger =
        std::env::temp_dir().join(format!("test_honest_fallback_{}.json", Uuid::new_v4()));
    let config = ReviewPolicyConfig {
        enabled: false, // Policy disabled to force local fallback
        max_reviews_per_issue: 1,
        max_requests_per_day: 3,
        max_requests_per_week: 10,
        max_cost_per_day_usd: 2.00,
        max_cost_per_month_usd: 10.00,
        max_input_tokens_per_review: 20_000,
        max_output_tokens_per_review: 4_000,
        model: "gpt-4o-mini".into(),
        dedup_cache_enabled: false,
    };

    let broker = ReviewBroker::new(config).with_ledger_path(temp_ledger.clone());

    let req = ReviewRequest {
        job_id: "job-offline".into(),
        issue_id: "repo/#1".into(),
        base_commit: "83b5762".into(),
        diff_patch: "patch".into(),
        test_manifest: "test result: ok".into(),
    };

    let res = broker.review(&req).await;

    // Per Issue #96: Local fallback must report unreviewed externally / inconclusive
    // Cannot falsely certify requirements complete or passing
    assert!(!res.verdict.passed);
    assert!(!res.verdict.requirements_covered);
    assert_eq!(res.verdict.confidence_score, 0.0);
    assert!(
        res.verdict
            .raw_feedback
            .contains("Unreviewed externally / inconclusive")
    );

    let _ = fs::remove_file(&temp_ledger);
}

#[tokio::test]
async fn test_secrets_redacted_before_review_dispatch() {
    let cred_store = Arc::new(CredentialStore::new());
    cred_store.set_secret(
        SubsystemId::Forge,
        "openai_api_key",
        "sk-proj-test1234567890abcdef",
    );
    cred_store.set_secret(
        SubsystemId::Bridge,
        "github_token",
        "ghp_super_secret_github_token",
    );

    let config = ReviewPolicyConfig::default();
    let broker = ReviewBroker::new(config).with_credentials(cred_store);

    let raw_diff = "diff --git a/config.rs b/config.rs\n+let token = \"ghp_super_secret_github_token\";\n+let key = \"sk-proj-test1234567890abcdef\";\n";
    let sanitized = broker.sanitize_payload(raw_diff);

    assert!(!sanitized.contains("ghp_super_secret_github_token"));
    assert!(!sanitized.contains("sk-proj-test1234567890abcdef"));
    assert!(sanitized.contains("[REDACTED_SECRET]"));
}
