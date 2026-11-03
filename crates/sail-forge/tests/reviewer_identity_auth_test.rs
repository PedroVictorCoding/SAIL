//! #96: one Codex review per issue, whatever the issue is called, and only with a
//! ChatGPT sign-in.
//!
//! These tests use the broker's real (non-mock) path with a stand-in `codex` script. The
//! script answers `codex login status` from a file and appends every review invocation to
//! `calls.log`, so the tests count actual review launches.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sail_forge::contracts::{ReviewPolicyConfig, ReviewRequest};
use sail_forge::reviewer::{
    CodexAuth, ReviewBroker, classify_codex_login_status, parse_codex_verdict,
};
use uuid::Uuid;

const CHATGPT: &str = "Logged in using ChatGPT\n";
const APPROVED: &str =
    "Looks good.\nVERDICT: APPROVED\nREQUIREMENTS_COVERED: YES\nCONFIDENCE: 0.8\nSUMMARY: ok\n";

struct FakeCodex {
    dir: PathBuf,
}

impl FakeCodex {
    fn new(login_status: &str, review_output: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fake-codex-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let d = dir.display();
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"login\" ]; then cat '{d}/login_status'; exit 0; fi\n\
             echo \"$@\" >> '{d}/calls.log'\n\
             cat > /dev/null\n\
             cat '{d}/review_output'\n"
        );
        fs::write(dir.join("codex"), script).unwrap();
        fs::set_permissions(dir.join("codex"), fs::Permissions::from_mode(0o755)).unwrap();
        let fake = Self { dir };
        fake.set_login(login_status);
        fs::write(fake.dir.join("review_output"), review_output).unwrap();
        fake
    }

    fn set_login(&self, status: &str) {
        fs::write(self.dir.join("login_status"), status).unwrap();
    }

    fn bin(&self) -> PathBuf {
        self.dir.join("codex")
    }

    fn calls(&self) -> usize {
        fs::read_to_string(self.dir.join("calls.log"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    fn ledger(&self) -> PathBuf {
        self.dir.join("ledger.json")
    }
}

impl Drop for FakeCodex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn broker(fake: &FakeCodex) -> ReviewBroker {
    broker_with(fake.bin(), &fake.ledger())
}

fn broker_with(bin: PathBuf, ledger: &Path) -> ReviewBroker {
    ReviewBroker::new(ReviewPolicyConfig {
        enabled: true,
        max_requests_per_day: 100,
        max_requests_per_week: 100,
        ..ReviewPolicyConfig::default()
    })
    .with_ledger_path(ledger.to_path_buf())
    .with_codex_bin(bin)
}

fn request(issue_id: &str) -> ReviewRequest {
    ReviewRequest {
        job_id: format!("job-{}", Uuid::new_v4()),
        issue_id: issue_id.into(),
        base_commit: "base".into(),
        diff_patch: "+ verified patch".into(),
        test_manifest: "test result: ok".into(),
    }
}

#[tokio::test]
async fn api_key_sign_in_never_starts_a_review_or_uses_the_slot() {
    let fake = FakeCodex::new("Logged in using an API key - sk-proj-***\n", APPROVED);
    let b = broker(&fake);

    let refused = b.review(&request("owner/repo#3")).await;
    assert!(
        refused.provider.contains("codex_api_key_auth_refused"),
        "{}",
        refused.provider
    );
    assert!(!refused.verdict.passed);
    assert_eq!(fake.calls(), 0);
    assert!(b.get_ledger().canonical_issue_attempts.is_empty());

    // After signing in with ChatGPT, the unused slot still allows exactly one review.
    fake.set_login(CHATGPT);
    let reviewed = broker(&fake).review(&request("owner/repo#3")).await;
    assert_eq!(reviewed.provider, "codex-cli:subscription");
    assert!(reviewed.verdict.passed);
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn not_logged_in_or_unknown_status_starts_no_review() {
    for status in ["Not logged in\n", "something unexpected\n"] {
        let fake = FakeCodex::new(status, APPROVED);
        let r = broker(&fake).review(&request("owner/repo#4")).await;
        assert!(
            r.provider.contains("codex_not_logged_in")
                || r.provider.contains("codex_auth_unverified"),
            "{status}: {}",
            r.provider
        );
        assert_eq!(fake.calls(), 0);
    }
}

#[tokio::test]
async fn missing_cli_does_not_consume_the_slot() {
    let fake = FakeCodex::new(CHATGPT, APPROVED);
    let missing = fake.dir.join("no-such-codex");
    let r = broker_with(missing, &fake.ledger())
        .review(&request("owner/repo#5"))
        .await;
    assert!(
        r.provider.contains("codex_cli_not_installed"),
        "{}",
        r.provider
    );

    let r = broker(&fake).review(&request("owner/repo#5")).await;
    assert!(r.verdict.passed);
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn spellings_of_one_issue_share_one_lifetime_slot() {
    let fake = FakeCodex::new(CHATGPT, APPROVED);
    for id in [
        "PedroVictorCoding/SAIL#7",
        "https://github.com/pedrovictorcoding/sail.git#7",
        "pedrovictorcoding/SAIL/#7",
        "git@github.com:PedroVictorCoding/SAIL#07",
    ] {
        // A fresh broker each time: the limit must come from the durable ledger.
        let _ = broker(&fake).review(&request(id)).await;
    }
    assert_eq!(fake.calls(), 1, "one issue, several spellings, one review");

    let _ = broker(&fake)
        .review(&request("PedroVictorCoding/SAIL#8"))
        .await;
    assert_eq!(fake.calls(), 2, "a different issue gets its own slot");
}

#[tokio::test]
async fn slots_recorded_under_old_spellings_stay_consumed() {
    let fake = FakeCodex::new(CHATGPT, APPROVED);
    let b = broker(&fake);
    let mut ledger = b.get_ledger();
    ledger.canonical_issue_attempts.insert(
        "PedroVictorCoding/SAIL#9".into(),
        "attempted:2026-10-31T00:00:00Z:legacy".into(),
    );
    b.persist_ledger_sync(&ledger).unwrap();

    let r = broker(&fake)
        .review(&request("pedrovictorcoding/sail#9"))
        .await;
    assert!(
        r.provider.contains("previously_attempted_canonical_issue"),
        "{}",
        r.provider
    );
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn unparseable_identity_is_refused() {
    let fake = FakeCodex::new(CHATGPT, APPROVED);
    for id in [
        "owner/repo",
        "owner/repo#abc",
        "#12",
        "owner/repo#0",
        "a/b/c#1",
        "own er/repo#1",
    ] {
        let r = broker(&fake).review(&request(id)).await;
        assert!(
            r.provider.contains("invalid_issue_identity"),
            "{id}: {}",
            r.provider
        );
    }
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn malformed_verdict_is_inconclusive_and_still_uses_the_slot() {
    // Codex echoed the prompt's template line, then gave a verdict: two VERDICT lines.
    let output =
        "VERDICT: APPROVED or CHANGES_REQUESTED\nVERDICT: APPROVED\nREQUIREMENTS_COVERED: YES\n";
    let fake = FakeCodex::new(CHATGPT, output);
    let r = broker(&fake).review(&request("owner/repo#11")).await;
    assert!(!r.verdict.passed);
    assert!(!r.verdict.requirements_covered);
    assert_eq!(r.verdict.confidence_score, 0.0);
    assert!(
        r.verdict.detected_bugs[0].contains("inconclusive"),
        "{:?}",
        r.verdict.detected_bugs
    );
    assert_eq!(fake.calls(), 1);

    let _ = broker(&fake).review(&request("owner/repo#11")).await;
    assert_eq!(fake.calls(), 1, "the review ran, so the slot is consumed");
}

#[test]
fn canonical_issue_keys() {
    let k = ReviewBroker::canonical_issue_key;
    assert_eq!(
        k("PedroVictorCoding/SAIL#12").as_deref(),
        Some("pedrovictorcoding/sail#12")
    );
    assert_eq!(
        k(" https://github.com/A/B.git/#012 ").as_deref(),
        Some("a/b#12")
    );
    assert_eq!(k("github.com:A/B#1").as_deref(), Some("a/b#1"));
    assert_eq!(k("repo#1").as_deref(), Some("repo#1"));
    assert_eq!(k("owner/repo"), None);
    assert_eq!(k("owner/repo#-1"), None);
}

#[test]
fn verdict_parsing_is_strict() {
    let v = parse_codex_verdict(APPROVED).unwrap();
    assert!(v.approved && v.requirements_covered);
    assert_eq!(v.confidence, Some(0.8));

    let v =
        parse_codex_verdict("**VERDICT:** CHANGES_REQUESTED\nREQUIREMENTS_COVERED: no\n").unwrap();
    assert!(!v.approved && !v.requirements_covered);
    assert_eq!(v.confidence, None, "a missing confidence is not invented");

    for bad in [
        "",
        "approved",
        "The verdict: APPROVED is what I'd say",
        "VERDICT: APPROVED\n",
        "VERDICT: MAYBE\nREQUIREMENTS_COVERED: YES\n",
        "VERDICT: APPROVED\nREQUIREMENTS_COVERED: YES\nCONFIDENCE: 7\n",
    ] {
        assert!(parse_codex_verdict(bad).is_err(), "accepted {bad:?}");
    }
}

#[test]
fn login_status_classification() {
    assert_eq!(
        classify_codex_login_status(true, "Logged in using ChatGPT"),
        CodexAuth::ChatGpt
    );
    assert_eq!(
        classify_codex_login_status(true, "Logged in using an API key - sk-***"),
        CodexAuth::ApiKey
    );
    assert_eq!(
        classify_codex_login_status(true, "Logged in using ChatGPT (API key also set)"),
        CodexAuth::ApiKey
    );
    assert_eq!(
        classify_codex_login_status(false, "Not logged in"),
        CodexAuth::NotLoggedIn
    );
    assert_eq!(
        classify_codex_login_status(true, "Logged in"),
        CodexAuth::Unknown
    );
}
