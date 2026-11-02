//! Solver behavior when the local model is busy, restarting or interrupted, driven by a
//! fake OpenAI-compatible server on localhost.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

use sail_forge::solver::{
    EndpointFailure, LocalSolverController, ModelTimeouts, summarize_endpoint_failures,
};
use sail_forge::worktree::IsolatedWorktree;
use sail_guardian::sandbox::SandboxPolicy;

const FIX_REPLY: &str = "src/lib.rs\n<<<<<<< SEARCH\npub fn f() -> i32 { 1 }\n=======\npub fn f() -> i32 { 2 }\n>>>>>>> REPLACE\n";

/// What the fake server does for the n-th chat request (0-based).
#[derive(Clone, Copy)]
enum Behavior {
    /// Accept, then never answer (a request queued behind a busy slot).
    Hang,
    /// Start a chunked SSE reply, then drop the connection mid-body.
    BreakStream,
    /// Answer with the fix as a plain JSON completion.
    Fix,
    /// Answer with an edit whose SEARCH text does not exist.
    BadEdit,
}

const BAD_EDIT_REPLY: &str =
    "src/lib.rs\n<<<<<<< SEARCH\npub fn g() {}\n=======\npub fn g() { }\n>>>>>>> REPLACE\n";

async fn read_request(sock: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = sock.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let len = text
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if buf.len() >= end + 4 + len {
                return text;
            }
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

/// Serve `script` for chat POSTs (the last entry repeats); 404 for anything else.
async fn fake_model(script: Vec<Behavior>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let chats = Arc::new(AtomicUsize::new(0));
    let counter = chats.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let counter = counter.clone();
            let script = script.clone();
            tokio::spawn(async move {
                let req = read_request(&mut sock).await;
                if !req.starts_with("POST") {
                    let _ = sock
                        .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n")
                        .await;
                    return;
                }
                let n = counter.fetch_add(1, Ordering::SeqCst);
                match script[n.min(script.len() - 1)] {
                    Behavior::Hang => tokio::time::sleep(Duration::from_secs(30)).await,
                    Behavior::BreakStream => {
                        let chunk = "data: {\"choices\":[{\"delta\":{\"content\":\"src/lib.rs\\n<<<<<<< SEARCH\"}}]}\n\n";
                        let head = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n{chunk}\r\n",
                            chunk.len()
                        );
                        let _ = sock.write_all(head.as_bytes()).await;
                        // Drop without the terminating 0-length chunk.
                    }
                    reply @ (Behavior::Fix | Behavior::BadEdit) => {
                        let content = if matches!(reply, Behavior::Fix) {
                            FIX_REPLY
                        } else {
                            BAD_EDIT_REPLY
                        };
                        let body = serde_json::json!({
                            "model": "fake",
                            "choices": [{"message": {"content": content}}]
                        })
                        .to_string();
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = sock.write_all(resp.as_bytes()).await;
                    }
                }
            });
        }
    });
    (format!("http://{addr}/v1/chat/completions"), chats)
}

fn fast_timeouts() -> ModelTimeouts {
    ModelTimeouts {
        connect: Duration::from_millis(500),
        idle: Duration::from_millis(400),
        busy_retries: 3,
        backoff: Duration::from_millis(50),
    }
}

fn repo_worktree() -> (IsolatedWorktree, PathBuf) {
    let repo = std::env::temp_dir().join(format!("model_req_{}", Uuid::new_v4()));
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"model_req\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn f() -> i32 { 1 }\n#[test]\nfn t() { assert_eq!(f(), 2); }\n",
    )
    .unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "."],
        &[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
    }
    let wt = IsolatedWorktree::create(
        &format!("model-req-{}", Uuid::new_v4()),
        &repo,
        "HEAD",
        Arc::new(SandboxPolicy::new()),
    )
    .unwrap();
    (wt, repo)
}

async fn solve(
    endpoints: Vec<String>,
    wt: &IsolatedWorktree,
) -> (sail_forge::solver::SolverOutcome, Vec<String>) {
    let controller = LocalSolverController::new(2, 120, None)
        .with_model_timeouts(fast_timeouts())
        .with_endpoints(endpoints);
    let logs = std::env::temp_dir().join(format!("model_req_logs_{}", Uuid::new_v4()));
    let mut stages = Vec::new();
    let outcome = controller
        .solve_issue(
            wt,
            "f must return 2",
            "## Target files\n- `src/lib.rs`\n",
            &logs,
            || false,
            |p| {
                if let Some(a) = &p.activity {
                    stages.push(format!("activity:{}", a.kind));
                }
                stages.push(p.stage.clone())
            },
        )
        .await;
    (outcome, stages)
}

#[tokio::test]
async fn busy_model_is_waited_for_instead_of_failing_the_job() {
    let (ep, chats) = fake_model(vec![Behavior::Hang, Behavior::Hang, Behavior::Fix]).await;
    let (wt, repo) = repo_worktree();
    let (outcome, stages) = solve(vec![ep], &wt).await;
    assert!(
        outcome.success,
        "{:?} {:?}",
        outcome.verification_status, outcome.failure_reason
    );
    assert_eq!(chats.load(Ordering::SeqCst), 3);
    assert!(stages.iter().any(|s| s == "waiting"));
    let _ = fs::remove_dir_all(repo);
}

#[tokio::test]
async fn interrupted_stream_is_retried_not_used() {
    let (ep, chats) = fake_model(vec![Behavior::BreakStream, Behavior::Fix]).await;
    let (wt, repo) = repo_worktree();
    let (outcome, _) = solve(vec![ep], &wt).await;
    assert!(outcome.success, "{:?}", outcome.failure_reason);
    assert_eq!(chats.load(Ordering::SeqCst), 2);
    assert_eq!(
        outcome.parse_failures, 0,
        "partial reply must not be parsed"
    );
    let _ = fs::remove_dir_all(repo);
}

#[tokio::test]
async fn busy_forever_reports_every_endpoint_with_cause() {
    // Busy model on one port, nothing listening on another: the report must name the busy
    // one, not just whichever endpoint happened to be tried last.
    let (busy, _) = fake_model(vec![Behavior::Hang]).await;
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap();
        drop(l);
        format!("http://{a}/v1/chat/completions")
    };
    let (wt, repo) = repo_worktree();
    let (outcome, _) = solve(vec![busy.clone(), closed.clone()], &wt).await;
    assert_eq!(outcome.verification_status, "model_unavailable");
    let reason = outcome.failure_reason.unwrap();
    assert!(
        reason.starts_with("Local model unavailable after 4 attempt(s)"),
        "{reason}"
    );
    assert!(
        reason.contains(&format!("{busy}: no reply within 400ms")),
        "{reason}"
    );
    assert!(
        reason.contains(&format!("{closed}: connection refused")),
        "{reason}"
    );
    assert!(
        reason.find(&busy).unwrap() < reason.find(&closed).unwrap(),
        "transient cause listed first: {reason}"
    );
    let _ = fs::remove_dir_all(repo);
}

#[tokio::test]
async fn nothing_listening_fails_fast_with_one_retry() {
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap();
        drop(l);
        format!("http://{a}/v1/chat/completions")
    };
    let (wt, repo) = repo_worktree();
    let (outcome, _) = solve(vec![closed], &wt).await;
    assert_eq!(outcome.verification_status, "model_unavailable");
    assert!(
        outcome
            .failure_reason
            .unwrap()
            .contains("after 2 attempt(s)")
    );
    let _ = fs::remove_dir_all(repo);
}

#[tokio::test]
async fn activity_log_records_context_waits_and_replies() {
    let (ep, _) = fake_model(vec![Behavior::Hang, Behavior::Fix]).await;
    let (wt, repo) = repo_worktree();
    let controller = LocalSolverController::new(2, 120, None)
        .with_model_timeouts(fast_timeouts())
        .with_endpoints(vec![ep]);
    let logs = std::env::temp_dir().join(format!("model_req_logs_{}", Uuid::new_v4()));
    let mut activity = Vec::new();
    let mut activity_only_with_ui_state = false;
    let outcome = controller
        .solve_issue(
            &wt,
            "f must return 2",
            "## Target files\n- `src/lib.rs`\n",
            &logs,
            || false,
            |p| {
                if p.stage == "activity" && (p.live_output.is_some() || p.diff.is_some()) {
                    activity_only_with_ui_state = true;
                }
                if let Some(a) = p.activity {
                    activity.push(a);
                }
            },
        )
        .await;
    assert!(outcome.success, "{:?}", outcome.failure_reason);
    assert!(!activity_only_with_ui_state);
    let kinds: Vec<&str> = activity.iter().map(|a| a.kind.as_str()).collect();
    assert_eq!(kinds, vec!["context", "waiting", "model"], "{activity:?}");
    assert!(activity[0].body.contains("--- File: src/lib.rs ---"));
    assert!(activity[1].body.contains("no reply within"));
    assert!(activity[2].body.contains(">>>>>>> REPLACE"));
    let _ = fs::remove_dir_all(repo);
}

#[tokio::test]
async fn every_model_exchange_is_recorded_as_activity() {
    let (ep, _) = fake_model(vec![Behavior::Hang, Behavior::BadEdit, Behavior::Fix]).await;
    let (wt, repo) = repo_worktree();
    let (outcome, stages) = solve(vec![ep], &wt).await;
    assert!(outcome.success, "{:?}", outcome.failure_reason);
    let kinds: Vec<&str> = stages
        .iter()
        .filter_map(|s| s.strip_prefix("activity:"))
        .collect();
    // context, wait for the busy slot, bad edit reply, the edit error fed back, the fix.
    assert_eq!(
        kinds,
        vec!["context", "waiting", "model", "feedback", "model"],
        "{stages:?}"
    );
    let _ = fs::remove_dir_all(repo);
}

#[test]
fn summary_without_failures_is_explicit() {
    assert!(summarize_endpoint_failures(&[], 1).contains("configured"));
    let one = EndpointFailure {
        endpoint: "e".into(),
        detail: "d".into(),
        transient: false,
    };
    assert_eq!(
        summarize_endpoint_failures(&[one], 2),
        "Local model unavailable after 2 attempt(s): e: d"
    );
}
