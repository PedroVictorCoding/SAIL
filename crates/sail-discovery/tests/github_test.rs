use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, TimeZone, Utc};
use sail_discovery::github::{
    GhCliActivity, GithubActivity, GithubError, GithubEventKind, GithubPoller, github_signal,
};
use sail_pulse::attention::store::AttentionStore;
use sail_pulse::attention::{AttentionContext, Delivery};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 13, 2, 0, 0).unwrap()
}

fn default_context(at: DateTime<Utc>) -> AttentionContext {
    AttentionContext {
        now: at,
        quiet_hours: false,
        focus_until: None,
        voice_active: false,
        busy: false,
        interrupts_last_hour: 0,
        suggestions_today: 0,
    }
}

fn write_fake_gh(dir: &Path, body: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join("gh");
    let script = format!("#!/bin/sh\nset -eu\n{body}\n");
    fs::write(&path, script).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

fn full_fixture_script(head_sha: &str, fail_reviews: bool) -> String {
    let reviews_branch = if fail_reviews {
        "echo 'simulated failure' >&2; exit 1"
    } else {
        r#"printf '%s\n' '[{"number":101,"title":"Review me","url":"https://github.com/owner/repo/pull/101","repository":{"nameWithOwner":"owner/repo"},"updatedAt":"2026-11-13T01:30:00Z","closedAt":null}]'"#
    };

    format!(
        r#"
args="$*"
case "$args" in
  "api user --jq .login")
    echo "octocat"
    ;;
  *"search prs --review-requested=@me"*)
    {reviews_branch}
    ;;
  *"search prs --author=@me --state=open"*)
    printf '%s\n' '[{{"number":42,"title":"Fix build","url":"https://github.com/owner/repo/pull/42","repository":{{"nameWithOwner":"owner/repo"}},"updatedAt":"2026-11-13T01:45:00Z","closedAt":null}}]'
    ;;
  *"pr checks 42"*)
    printf '%s\n' '[{{"name":"clippy","state":"COMPLETED","bucket":"pass"}},{{"name":"test","state":"COMPLETED","bucket":"fail"}}]'
    ;;
  *"pr view 42"*)
    printf '%s\n' '{{"headRefOid":"{head_sha}"}}'
    ;;
  *"search prs --author=@me --merged"*)
    printf '%s\n' '[{{"number":39,"title":"Landed feature","url":"https://github.com/owner/repo/pull/39","repository":{{"nameWithOwner":"owner/repo"}},"updatedAt":"2026-11-12T20:00:00Z","closedAt":"2026-11-12T20:00:00Z"}}]'
    ;;
  *"search issues --assignee=@me"*)
    printf '%s\n' '[{{"number":7,"title":"Bug report","url":"https://evil.example.com/issue/7","repository":{{"nameWithOwner":"owner/repo"}},"updatedAt":"2026-11-13T01:00:00Z","closedAt":null}}]'
    ;;
  *"release list --repo owner/watched"*)
    printf '%s\n' '[{{"tagName":"v1.2.0","publishedAt":"2026-11-13T01:15:00Z","name":"Release 1.2.0"}},{{"tagName":"v1.0.0","publishedAt":"2026-10-01T00:00:00Z","name":"Old"}}]'
    ;;
  *)
    echo "unexpected args: $args" >&2
    exit 2
    ;;
esac
"#
    )
}

#[test]
fn parses_all_kinds_and_acceptance_ingests_ci_failed() {
    let dir = std::env::temp_dir().join(format!("sail-gh-test-{}", uuid::Uuid::new_v4()));
    let bin = write_fake_gh(&dir, &full_fixture_script("abc1234", false));
    let client = GhCliActivity::with_bin(bin, vec!["owner/watched".into()]);

    let events = client.fetch(now() - Duration::hours(24)).unwrap();
    assert_eq!(events.len(), 5, "{events:?}");

    let review = events
        .iter()
        .find(|e| e.kind == GithubEventKind::ReviewRequested)
        .unwrap();
    assert_eq!(review.id, "review:owner/repo#101");

    let ci = events
        .iter()
        .find(|e| e.kind == GithubEventKind::CiFailed)
        .unwrap();
    assert_eq!(ci.id, "ci:owner/repo#42:abc1234");
    assert_eq!(ci.failing_checks, vec!["test"]);

    let merged = events
        .iter()
        .find(|e| e.kind == GithubEventKind::PrMerged)
        .unwrap();
    assert_eq!(merged.id, "merged:owner/repo#39");

    let assigned = events
        .iter()
        .find(|e| e.kind == GithubEventKind::IssueAssigned)
        .unwrap();
    assert_eq!(assigned.id, "assigned:owner/repo#7");
    let assigned_sig = github_signal(assigned, now());
    assert_eq!(assigned_sig[0].link, None, "non-github URL must be dropped");

    let release = events
        .iter()
        .find(|e| e.kind == GithubEventKind::ReleasePublished)
        .unwrap();
    assert_eq!(release.id, "release:owner/watched:v1.2.0");

    // Acceptance: ingest CiFailed into AttentionStore -> Suggest or Interrupt, deduped on second poll.
    let store = AttentionStore::open(dir.join("attention.db")).unwrap();
    let ci_sig = github_signal(ci, now()).remove(0);
    assert_eq!(
        ci_sig.link.as_deref(),
        Some("https://github.com/owner/repo/pull/42")
    );
    let item = store
        .ingest(ci_sig.clone(), default_context(now()))
        .unwrap();
    assert!(
        matches!(
            item.assessment.delivery,
            Delivery::Suggest | Delivery::Interrupt
        ),
        "unexpected delivery: {:?}",
        item.assessment.delivery
    );
    assert_eq!(store.inbox(10).unwrap().len(), 1);

    // Same sha on second poll -> dropped as duplicate, inbox still 1.
    let dup = store
        .ingest(ci_sig, default_context(now() + Duration::minutes(15)))
        .unwrap();
    assert!(matches!(dup.assessment.delivery, Delivery::Drop(_)));
    assert_eq!(store.inbox(10).unwrap().len(), 1);

    // New head sha -> new item.
    let bin2 = write_fake_gh(&dir, &full_fixture_script("def5678", false));
    let client2 = GhCliActivity::with_bin(bin2, vec![]);
    let events2 = client2.fetch(now() - Duration::hours(24)).unwrap();
    let ci2 = events2
        .iter()
        .find(|e| e.kind == GithubEventKind::CiFailed)
        .unwrap();
    let ci2_sig = github_signal(ci2, now() + Duration::minutes(20)).remove(0);
    let item2 = store
        .ingest(ci2_sig, default_context(now() + Duration::minutes(20)))
        .unwrap();
    assert!(matches!(
        item2.assessment.delivery,
        Delivery::Suggest | Delivery::Interrupt
    ));
    assert_eq!(store.inbox(10).unwrap().len(), 2);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn poller_pacing_respects_15_min_interval_and_failures() {
    let mut poller = GithubPoller::default();
    let t0 = now();
    assert!(poller.due(t0));
    assert_eq!(poller.since(t0), t0 - Duration::days(3));

    // Failed attempt still blocks for 15 min and does not advance `since`.
    poller.record(t0, false);
    assert!(!poller.due(t0 + Duration::minutes(14)));
    assert!(poller.due(t0 + Duration::minutes(15)));
    assert_eq!(
        poller.since(t0 + Duration::minutes(15)),
        t0 + Duration::minutes(15) - Duration::days(3)
    );

    // Successful attempt advances `since`.
    let t1 = t0 + Duration::minutes(15);
    poller.record(t1, true);
    assert!(!poller.due(t1 + Duration::minutes(5)));
    assert_eq!(poller.since(t1 + Duration::minutes(20)), t1);
}

#[test]
fn missing_gh_returns_unavailable_and_partial_query_failure_keeps_rest() {
    let missing = GhCliActivity::with_bin(PathBuf::from("/nonexistent/gh-bin-xyz"), vec![]);
    assert!(matches!(
        missing.fetch(now() - Duration::days(1)),
        Err(GithubError::Unavailable(_))
    ));

    let dir = std::env::temp_dir().join(format!("sail-gh-partial-{}", uuid::Uuid::new_v4()));
    let bin = write_fake_gh(&dir, &full_fixture_script("abc1234", true));
    let client = GhCliActivity::with_bin(bin, vec!["owner/watched".into()]);
    let events = client.fetch(now() - Duration::hours(24)).unwrap();
    assert_eq!(events.len(), 4, "all other queries should succeed");
    assert!(
        events
            .iter()
            .all(|e| e.kind != GithubEventKind::ReviewRequested)
    );

    // Events older than 7 days produce no signal.
    let mut old = events[0].clone();
    old.at = now() - Duration::days(8);
    assert!(github_signal(&old, now()).is_empty());

    let _ = fs::remove_dir_all(&dir);
}
