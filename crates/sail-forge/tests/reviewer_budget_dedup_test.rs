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
            .get("pedrovictorcoding/sail#90")
            .copied()
            .unwrap_or(0),
        1
    );
    assert!(
        ledger
            .canonical_issue_reviews
            .contains_key("pedrovictorcoding/sail#90")
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
    cred_store
        .store_credential(
            SubsystemId::Bridge,
            "openai_api_key",
            "sk-proj-test1234567890abcdef",
            "*",
            chrono::Duration::days(1),
        )
        .unwrap();
    cred_store
        .store_credential(
            SubsystemId::Bridge,
            "github_token",
            "ghp_super_secret_github_token",
            "*",
            chrono::Duration::days(1),
        )
        .unwrap();

    let config = ReviewPolicyConfig::default();
    let broker = ReviewBroker::new(config).with_credentials(cred_store);

    let raw_diff = "diff --git a/config.rs b/config.rs\n+let token = \"ghp_super_secret_github_token\";\n+let key = \"sk-proj-test1234567890abcdef\";\n";
    let sanitized = broker.sanitize_payload(raw_diff);

    assert!(!sanitized.contains("ghp_super_secret_github_token"));
    assert!(!sanitized.contains("sk-proj-test1234567890abcdef"));
    assert!(sanitized.contains("[REDACTED_SECRET]"));
}

#[tokio::test]
async fn test_crash_after_reservation_prevents_subsequent_process_invocation() {
    let temp_ledger =
        std::env::temp_dir().join(format!("test_crash_ledger_{}.json", Uuid::new_v4()));
    let config = ReviewPolicyConfig::default();

    // Session 1: Create broker, reserve issue in ledger with fsync (simulating crash before process completion)
    {
        let broker = ReviewBroker::new(config.clone()).with_ledger_path(temp_ledger.clone());
        let mut ledger = broker.get_ledger();
        ledger.canonical_issue_attempts.insert(
            "PedroVictorCoding/SAIL#100".into(),
            "attempted:2026-10-31T15:00:00Z:dummy_hash".into(),
        );
        broker.persist_ledger_sync(&ledger).unwrap();
        // Process "crashes" here before execute_codex_review completes
    }

    // Session 2: Broker reboots after crash with persisted ledger on disk
    let rebooted_broker = Arc::new(
        ReviewBroker::new(config)
            .with_ledger_path(temp_ledger.clone())
            .with_mock_mode(true),
    );

    let req = ReviewRequest {
        job_id: "job-post-crash".into(),
        issue_id: "PedroVictorCoding/SAIL/#100".into(),
        base_commit: "base123".into(),
        diff_patch: "+++ b/src/lib.rs\n+// fix".into(),
        test_manifest: "test result: ok".into(),
    };

    // Subsequent review request after reboot
    let res = rebooted_broker.review(&req).await;

    // Zero process invocations allowed on rebooted broker!
    assert_eq!(
        rebooted_broker.invocation_count(),
        0,
        "Crash-safe limit violated: process was invoked after daemon reboot when attempt was already reserved"
    );
    assert!(res.cached, "Must return cached/inconclusive fallback");
    assert!(!res.verdict.passed);

    let _ = fs::remove_file(&temp_ledger);
}

fn accounting_request(issue: usize) -> ReviewRequest {
    ReviewRequest {
        job_id: format!("accounting-{issue}"),
        issue_id: format!("repo#{}", issue),
        base_commit: "base".into(),
        diff_patch: "verified patch".into(),
        test_manifest: "test result: ok".into(),
    }
}

fn accounting_config() -> ReviewPolicyConfig {
    ReviewPolicyConfig {
        enabled: true,
        max_requests_per_day: 100,
        max_requests_per_week: 100,
        ..ReviewPolicyConfig::default()
    }
}

#[test]
fn independent_brokers_share_one_lifetime_reservation() {
    let dir = std::env::temp_dir().join(format!("review-race-{}", Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    let path = dir.join("ledger.json");
    // Construct all brokers before any ledger exists: cached snapshots are stale.
    let brokers: Vec<_> = (0..20)
        .map(|_| {
            Arc::new(
                ReviewBroker::new(accounting_config())
                    .with_ledger_path(path.clone())
                    .with_mock_mode(true),
            )
        })
        .collect();
    let barrier = Arc::new(std::sync::Barrier::new(20));
    let handles: Vec<_> = brokers
        .iter()
        .map(|broker| {
            let broker = broker.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                barrier.wait();
                rt.block_on(broker.review(&accounting_request(1)))
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(
        brokers.iter().map(|b| b.invocation_count()).sum::<usize>(),
        1
    );
    // A different job/diff on an already constructed broker cannot bypass the cap.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut retry = accounting_request(1);
    retry.job_id = "new-job".into();
    retry.diff_patch = "revised patch".into();
    rt.block_on(brokers[19].review(&retry));
    assert_eq!(
        brokers.iter().map(|b| b.invocation_count()).sum::<usize>(),
        1
    );
    let ledger: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(ledger["requests_today"], 1);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn stale_brokers_preserve_other_issues_and_enforce_daily_and_weekly_caps() {
    for weekly in [false, true] {
        let dir = std::env::temp_dir().join(format!("review-caps-{}", Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("ledger.json");
        let mut config = accounting_config();
        if weekly {
            config.max_requests_per_week = 2;
        } else {
            config.max_requests_per_day = 2;
        }
        let brokers: Vec<_> = (0..3)
            .map(|_| {
                ReviewBroker::new(config.clone())
                    .with_ledger_path(path.clone())
                    .with_mock_mode(true)
            })
            .collect();
        assert!(
            brokers[0]
                .review(&accounting_request(1))
                .await
                .verdict
                .passed
        );
        assert!(
            brokers[1]
                .review(&accounting_request(2))
                .await
                .verdict
                .passed
        );
        let denied = brokers[2].review(&accounting_request(3)).await;
        assert!(!denied.verdict.passed);
        assert!(denied.provider.contains(if weekly {
            "weekly_request_limit_exceeded"
        } else {
            "daily_request_limit_exceeded"
        }));
        assert_eq!(brokers[2].invocation_count(), 0);
        let ledger: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(ledger["requests_today"], 2);
        assert_eq!(
            ledger["canonical_issue_reviews"].as_object().unwrap().len(),
            2
        );
        assert_eq!(
            ledger["canonical_issue_attempts"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn corrupt_or_inaccessible_ledger_never_dispatches() {
    let dir = std::env::temp_dir().join(format!("review-fail-closed-{}", Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    for mode in [
        "corrupt",
        "directory",
        "invalid-parent",
        "symlink",
        "dangling",
    ] {
        let path = dir.join(mode).join("ledger.json");
        if mode == "invalid-parent" {
            fs::write(path.parent().unwrap(), "not a directory").unwrap();
        } else {
            fs::create_dir(path.parent().unwrap()).unwrap();
            if mode == "corrupt" {
                fs::write(&path, "{broken journal").unwrap();
            } else if mode == "symlink" {
                std::os::unix::fs::symlink("/dev/null", &path).unwrap();
            } else if mode == "dangling" {
                std::os::unix::fs::symlink(dir.join("nonexistent"), &path).unwrap();
            } else {
                fs::create_dir(&path).unwrap();
            }
        }
        let broker = ReviewBroker::new(accounting_config())
            .with_ledger_path(path.clone())
            .with_mock_mode(true);
        let result = broker.review(&accounting_request(1)).await;
        assert_eq!(broker.invocation_count(), 0, "{mode}");
        assert!(!result.verdict.passed);
        assert!(result.provider.contains("review_ledger_unavailable"));
        if mode == "corrupt" {
            assert_eq!(fs::read_to_string(&path).unwrap(), "{broken journal");
        }
    }
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn reservation_write_failure_is_returned_and_never_dispatches() {
    let dir = std::env::temp_dir().join(format!("review-write-failure-{}", Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    let path = dir.join("ledger.json");
    let broker = ReviewBroker::new(accounting_config())
        .with_ledger_path(path.clone())
        .with_mock_mode(true);
    let ledger = broker.get_ledger();
    // Rename cannot replace a directory. Persistence must expose that failure.
    fs::create_dir(&path).unwrap();
    assert!(broker.persist_ledger_sync(&ledger).is_err());
    assert!(
        broker
            .review(&accounting_request(1))
            .await
            .provider
            .contains("review_ledger_unavailable")
    );
    assert_eq!(broker.invocation_count(), 0);
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        2,
        "temporary file leaked"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn failed_reservation_commit_does_not_consume_or_dispatch() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("review-readonly-{}", Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    let path = dir.join("ledger.json");
    let broker = ReviewBroker::new(accounting_config())
        .with_ledger_path(path.clone())
        .with_mock_mode(true);
    broker.persist_ledger_sync(&broker.get_ledger()).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let result = broker.review(&accounting_request(1)).await;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.provider.contains("review_ledger_unavailable"));
    assert_eq!(broker.invocation_count(), 0);
    let ledger: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(ledger["requests_today"], 0);
    let retry = broker.review(&accounting_request(1)).await;
    assert!(retry.verdict.passed, "{}", retry.verdict.raw_feedback);
    assert_eq!(broker.invocation_count(), 1);
    fs::remove_dir_all(dir).unwrap();
}
