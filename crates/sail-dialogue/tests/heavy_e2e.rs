//! C8 acceptance through Dialogue's real MeshPicker and the Night Shift runner.
#![cfg(target_os = "linux")]

#[path = "../../sail-mind/tests/support/heavy_e2e.rs"]
mod support;

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use sail_continuity::Continuity;
use sail_forge::{
    admission::IssueCandidate,
    contracts::{ForgeJobReport, ReviewPolicyConfig},
    github::{DraftPr, GithubClient, OpenPullRequest},
    nightly::{ForgeNightlyRunner, JobExecutor, NightlyConfig},
    nightshift::{
        EndpointPicker, ErrandSource, ErrandState, ErrandStore, NightShiftSettings, RanOnKind,
        Size, Transition, WorkOrder,
    },
    orchestrator::{ForgeOrchestrator, OrchestratorConfig},
    persistence::TaskCheckpointStore,
    reviewer::ReviewBroker,
};
use sail_guardian::sandbox::SandboxPolicy;

const FIXTURE_REPO: &str = "local/c8-fixture";

struct OfflineGithub;

impl GithubClient for OfflineGithub {
    fn list_open_issues(&self, _: &str, _: &str) -> Result<Vec<IssueCandidate>, String> {
        panic!("GitHub intake must be disabled")
    }

    fn open_pull_requests(&self, _: &str) -> Result<Vec<OpenPullRequest>, String> {
        panic!("GitHub intake must be disabled")
    }

    fn create_draft_pr(&self, _: &str, _: &DraftPr) -> Result<String, String> {
        panic!("publishing must be disabled")
    }
}

struct HttpExecutor {
    client: reqwest::Client,
    pooled: String,
    host: String,
    requests: AtomicUsize,
}

#[async_trait::async_trait]
impl JobExecutor for HttpExecutor {
    async fn execute(
        &self,
        forge: &ForgeOrchestrator,
        job_id: &str,
    ) -> Result<ForgeJobReport, String> {
        self.requests.fetch_add(1, Ordering::Relaxed);
        let checkpoint = forge.checkpoint_store().load(job_id).unwrap().unwrap();
        let endpoint = checkpoint.model_endpoint.as_deref().unwrap_or(&self.host);
        assert_eq!(
            endpoint, self.pooled,
            "runner selected an unexpected endpoint"
        );
        let error = self
            .client
            .post(endpoint)
            .json(&serde_json::json!({"messages":[{"role":"user","content":"C8 fixture"}]}))
            .send()
            .await
            .expect_err("Pause must disconnect the in-flight HTTP request");
        Err(format!("Model transport failure at {endpoint}: {error}"))
    }
}

fn forge(root: &Path) -> Arc<ForgeOrchestrator> {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::write(
        workspace.join("src/lib.rs"),
        "pub fn value() -> u32 { 1 }\n",
    )
    .unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "C8"],
        vec!["config", "user.email", "c8@example.test"],
        vec!["config", "core.hooksPath", "/dev/null"],
        vec!["add", "."],
        vec!["commit", "-m", "fixture"],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&workspace)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Arc::new(
        ForgeOrchestrator::new(
            OrchestratorConfig {
                workspace_root: workspace,
                allowed_repos: [FIXTURE_REPO.into()].into(),
                ..Default::default()
            },
            Arc::new(TaskCheckpointStore::new(root.join("checkpoints"))),
            Arc::new(
                ReviewBroker::new(ReviewPolicyConfig::default())
                    .with_mock_mode(true)
                    .with_ledger_path(root.join("ledger.json")),
            ),
            Arc::new(SandboxPolicy::new()),
        )
        .with_continuity(Arc::new(
            Continuity::open(root.join("continuity.db")).unwrap(),
        )),
    )
}

#[tokio::test]
async fn paused_pooled_night_shift_requeues_without_host_fallback() {
    if !support::enter_fixture("paused_pooled_night_shift_requeues_without_host_fallback").await {
        return;
    }
    let mut stack = support::Stack::start().await;
    let picker: Arc<dyn EndpointPicker> =
        sail_dialogue::nightshift::endpoint_picker(stack.mind.clone());
    let endpoint = picker
        .heavy()
        .expect("MeshPicker must choose the active lane");
    assert_eq!(endpoint.ran_on.kind, RanOnKind::Pooled);
    assert_eq!(endpoint.ran_on.name, support::WORKER);
    assert_eq!(endpoint.url, stack.lane.endpoint().unwrap().url);

    let executor = Arc::new(HttpExecutor {
        client: endpoint.client.expect("pooled loopback client"),
        pooled: endpoint.url,
        host: stack.host_endpoint.clone(),
        requests: AtomicUsize::new(0),
    });
    let root = std::env::current_dir().unwrap();
    let state = root.join("nightly");
    let store = Arc::new(ErrandStore::open(&state).unwrap());
    store
        .set_settings(NightShiftSettings {
            enabled: true,
            include_forge_ready_issues: false,
            use_heavy: true,
            fallback_to_host: false,
            ..Default::default()
        })
        .unwrap();
    let now = chrono::Utc::now();
    let errand = store
        .add(
            "Add a value to the fixture library.",
            ErrandSource::Web,
            now,
            [1, 2, 3, 4, 5, 6],
        )
        .unwrap();
    store
        .transition(
            &errand.id,
            now,
            Transition::Proposed(WorkOrder {
                title: "Update fixture library".into(),
                why: "Exercise pooled Night Shift routing".into(),
                acceptance: vec!["The fixture library has a value".into()],
                target_files: vec!["src/lib.rs".into()],
                size: Size::S,
                notes_for_solver: String::new(),
            }),
        )
        .unwrap();
    let queued = store
        .transition(&errand.id, now, Transition::Approve(None))
        .unwrap();
    let runner = Arc::new(
        ForgeNightlyRunner::new(
            forge(&root),
            Arc::new(OfflineGithub),
            NightlyConfig {
                repo_slug: FIXTURE_REPO.into(),
                state_dir: state,
                publish: false,
                ..Default::default()
            },
        )
        .with_executor(executor.clone())
        .with_night_shift(store.clone())
        .with_endpoint_picker(picker),
    );
    let mut attempt = tokio::spawn({
        let runner = runner.clone();
        async move { runner.run_once(false, false).await }
    });

    tokio::select! {
        result = &mut attempt => panic!("Night Shift finished before the pooled request: {:?}", result.unwrap()),
        reached = support::wait_for(
            || stack.model.with_extension("inflight").is_file(),
            Duration::from_secs(5),
        ) => assert!(reached, "pooled request did not reach the server; lane: {:?}", stack.lane.status()),
    }
    let working = store.get(&errand.id).unwrap();
    assert!(
        matches!(working.state, ErrandState::Working { ran_on, .. } if ran_on.kind == RanOnKind::Pooled)
    );
    stack.pause().await;
    let digest = tokio::time::timeout(Duration::from_secs(5), attempt)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest.halted.as_deref(), Some("heavy_unavailable"));
    let requeued = store.get(&errand.id).unwrap();
    assert_eq!(requeued.state, queued.state);
    assert_eq!(requeued.attempts, queued.attempts);

    let next = runner.run_once(false, false).await;
    assert_eq!(next.halted.as_deref(), Some("heavy_unavailable"));
    assert_eq!(executor.requests.load(Ordering::Relaxed), 1);
    assert_eq!(stack.host_completions.load(Ordering::Relaxed), 0);
    stack.mind.shutdown().await.unwrap();
}
