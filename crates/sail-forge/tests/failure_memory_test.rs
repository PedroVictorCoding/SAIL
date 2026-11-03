//! #121: finished Forge jobs are recorded in failure memory, and the next attempt's brief
//! reflects them. Uses the orchestrator's real lifecycle (sandboxed `cargo test`, gates).
#![allow(clippy::field_reassign_with_default)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use sail_forge::contracts::{IssueEnqueueRequest, ReviewPolicyConfig};
use sail_forge::failure_memory::{AttemptOutcome, FailureClass, FailureMemory};
use sail_forge::orchestrator::{ForgeOrchestrator, OrchestratorConfig};
use sail_forge::persistence::TaskCheckpointStore;
use sail_forge::reviewer::ReviewBroker;
use sail_guardian::sandbox::SandboxPolicy;
use uuid::Uuid;

const BODY: &str = "## Target files\n- `src/lib.rs`\n\n## Acceptance\n- [ ] works";

fn repo(path: &Path) {
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(
        path.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    fs::write(
        path.join("src/lib.rs"),
        "pub fn compute() -> i32 { 42 }\n#[test]\nfn test_compute() { assert_eq!(compute(), 42); }\n",
    )
    .unwrap();
    fs::write(path.join(".gitignore"), "target/\nCargo.lock\n").unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t.local",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(&args)
                .current_dir(path)
                .status()
                .unwrap()
                .success()
        );
    }
}

fn orchestrator(dir: &Path, memory: Arc<FailureMemory>) -> ForgeOrchestrator {
    let mut config = OrchestratorConfig::default();
    config.workspace_root = dir.to_path_buf();
    ForgeOrchestrator::new(
        config,
        Arc::new(TaskCheckpointStore::new(dir.join("checkpoints"))),
        Arc::new(
            ReviewBroker::new(ReviewPolicyConfig::default())
                .with_mock_mode(true)
                .with_ledger_path(dir.join("review_ledger.json")),
        ),
        Arc::new(SandboxPolicy::new()),
    )
    .with_failure_memory(memory)
}

fn enqueue(o: &ForgeOrchestrator, issue: u64) -> String {
    o.enqueue_issue(IssueEnqueueRequest {
        repo: "PedroVictorCoding/SAIL".into(),
        issue_number: issue,
        issue_body: Some(BODY.into()),
        ..Default::default()
    })
    .unwrap()
}

fn temp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("forge_memory_{}", Uuid::new_v4()));
    repo(&d);
    d
}

#[tokio::test]
async fn gate_failures_and_verified_fixes_are_recorded_and_briefed() {
    let dir = temp();
    let memory = Arc::new(FailureMemory::new(dir.join("state/failure_memory.jsonl")));
    let o = orchestrator(&dir, memory.clone());

    // Two attempts that add a production `.unwrap()`: the gate rejects both.
    let unwrap = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n pub fn compute() -> i32 { 42 }\n+pub fn parse() -> u32 { \"1\".parse().unwrap() }\n";
    for issue in [301, 302] {
        let job = enqueue(&o, issue);
        assert!(o.run_job_lifecycle(&job, Some(unwrap)).await.is_err());
    }
    // A clean change on another issue passes.
    let ok = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n pub fn compute() -> i32 { 42 }\n+// documented\n";
    let job = enqueue(&o, 303);
    o.run_job_lifecycle(&job, Some(ok)).await.unwrap();

    let records = memory.load().unwrap();
    assert_eq!(records.len(), 3, "{records:#?}");
    assert_eq!(records[0].issue, "pedrovictorcoding/sail#301");
    assert_eq!(
        records[0].outcome,
        AttemptOutcome::Failed {
            class: FailureClass::Gate
        }
    );
    assert!(
        records[0]
            .signatures
            .contains(&"gate:new_unwrap".to_string()),
        "{:?}",
        records[0]
    );
    assert_eq!(records[0].files, vec!["src/lib.rs"]);
    assert!(records[2].is_verified());

    // The next attempt at #301 is told what happened, what recurs and what worked.
    let brief = memory
        .brief("PedroVictorCoding/SAIL#301", &["src/lib.rs".into()])
        .unwrap();
    assert!(
        brief.contains("Earlier attempts on this issue failed:\n- diff gate"),
        "{brief}"
    );
    assert!(brief.contains("gate:new_unwrap (2 attempts)"), "{brief}");
    assert!(
        brief.contains("pedrovictorcoding/sail#303 changed src/lib.rs"),
        "{brief}"
    );

    // After the user invalidates the "verified" fix, it is no longer offered.
    memory.invalidate(&records[2].id, "reverted").unwrap();
    let brief = memory
        .brief("PedroVictorCoding/SAIL#301", &["src/lib.rs".into()])
        .unwrap();
    assert!(!brief.contains("#303"), "{brief}");

    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn model_outages_are_not_recorded_as_engineering_failures() {
    // SAFETY: this test binary runs this test alone in its process under nextest; under
    // cargo test no other test here reads the variable.
    unsafe {
        std::env::set_var(
            "SAIL_FORGE_LLM_URL",
            "http://127.0.0.1:19998/v1/chat/completions",
        );
    }
    let dir = temp();
    let memory = Arc::new(FailureMemory::new(dir.join("state/failure_memory.jsonl")));
    let o = orchestrator(&dir, memory.clone());
    let job = enqueue(&o, 310);
    let err = o.run_job_lifecycle(&job, None).await.unwrap_err();
    assert!(memory.load().unwrap().is_empty(), "outage recorded: {err}");
    let _ = fs::remove_dir_all(&dir);
}
