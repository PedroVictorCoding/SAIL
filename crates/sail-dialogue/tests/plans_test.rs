//! Agent task plans through Dialogue's real tools and the owner's HTTP controls: a goal is
//! planned, waits for the owner's approval, then runs `local:time` and an approval-gated
//! `fs:write` through Guardian, and each step's check passes on the real output.
use async_trait::async_trait;
use sail_agents::planning::{PlanRequest, Planner};
use sail_guardian::{
    ApprovalBroker, AuditLedger, GuardianExecutionBroker, PolicyEngine, SandboxPolicy,
};
use sail_protocol::execution::ToolState;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TOKEN: &str = "test-plans-owner-token-at-least-32-bytes";

/// Returns prepared drafts in order, and records what it was asked.
struct ScriptedPlanner {
    drafts: Mutex<Vec<String>>,
    prompts: Mutex<Vec<String>>,
}
#[async_trait]
impl Planner for ScriptedPlanner {
    fn name(&self) -> String {
        "scripted".into()
    }
    async fn plan(&self, request: &PlanRequest) -> Result<String, String> {
        self.prompts.lock().unwrap().push(request.prompt());
        let mut drafts = self.drafts.lock().unwrap();
        (!drafts.is_empty())
            .then(|| drafts.remove(0))
            .ok_or_else(|| "no more drafts".into())
    }
}

async fn wait_for(client: &reqwest::Client, url: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let view: Value = client
            .get(url)
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if done(&view) {
            return view;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "plan never reached the expected state: {view:#}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn owner_approved_plan_runs_dialogue_tools_through_guardian() {
    let dir = std::env::temp_dir().join(format!("sail-plans-{}", uuid::Uuid::new_v4().simple()));
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let workspace = std::fs::canonicalize(&workspace).unwrap();
    let approvals = Arc::new(ApprovalBroker::new());
    sail_guardian::register_global_broker(Arc::new(GuardianExecutionBroker::new(
        Arc::new(PolicyEngine::new()),
        Arc::new(AuditLedger::new()),
        approvals.clone(),
        Arc::new(SandboxPolicy::new().with_root(&workspace)),
    )));
    let note = workspace.join("time.txt");
    let draft = json!({"steps":[
        {"id":"clock","intent":"Read the current time","tool":"local:time",
         "check":{"kind":"exists","pointer":"/utc"}},
        {"id":"write","intent":"Write the note","tool":"fs:write",
         "input":{"path":note,"content":"noted by a plan"},"depends_on":["clock"],
         "check":{"kind":"equals","pointer":"/bytes_written","value":15}}
    ]});
    let planner = Arc::new(ScriptedPlanner {
        drafts: Mutex::new(vec![draft.to_string()]),
        prompts: Mutex::new(Vec::new()),
    });
    let continuity =
        Arc::new(sail_continuity::Continuity::open(dir.join("continuity.db")).unwrap());
    let plans = sail_dialogue::plans::Plans::with_planner(continuity, planner.clone()).unwrap();

    // Served as in the daemon, nested under /api/plans.
    let app = axum::Router::new().nest_service(
        "/api/plans",
        sail_dialogue::plans::router(
            plans.clone(),
            Some(sail_guardian::authority::ApprovalAuthority::new(TOKEN).unwrap()),
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/plans", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let goal = json!({"goal":"write today's time to time.txt"});

    // Every control needs the owner token.
    let anonymous = client.post(&base).json(&goal).send().await.unwrap();
    assert_eq!(anonymous.status(), 401);
    assert!(planner.prompts.lock().unwrap().is_empty());

    let created: Value = client
        .post(&base)
        .bearer_auth(TOKEN)
        .json(&goal)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let plan = &created["plan"]["plan"];
    let id = plan["id"].as_str().unwrap().to_owned();
    assert_eq!(plan["state"], "awaiting_approval", "{created:#}");
    assert_eq!(plan["steps"][1]["effectful"], true);
    // The planner saw the registry's real descriptions, not placeholders.
    assert!(
        planner.prompts.lock().unwrap()[0]
            .contains("Create or write a file inside the authorized workspace")
    );
    let listed: Value = client
        .get(&base)
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["plans"][0]["plan"]["id"], id.as_str());

    let approve = format!("{base}/{id}/approve");
    assert_eq!(client.post(&approve).send().await.unwrap().status(), 401);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!note.exists(), "nothing runs before the owner approves");

    // Guardian still asks about the write when it runs; the owner answers yes.
    let scheduler_events = {
        let mut events = plans.tool_events();
        let approvals = approvals.clone();
        tokio::spawn(async move {
            while let Ok(event) = events.recv().await {
                if let (ToolState::PolicyPending, Some(session)) =
                    (event.state, event.approval_session)
                {
                    let _ = approvals.resolve_session(session, true);
                }
            }
        })
    };
    let approved = client
        .post(&approve)
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert!(approved.status().is_success());
    let done = wait_for(&client, &format!("{base}/{id}"), |v| {
        matches!(
            v["plan"]["plan"]["state"].as_str(),
            Some("succeeded" | "failed")
        )
    })
    .await;
    assert_eq!(done["plan"]["plan"]["state"], "succeeded", "{done:#}");
    assert_eq!(done["plan"]["task_state"], "succeeded");
    assert_eq!(std::fs::read_to_string(&note).unwrap(), "noted by a plan");
    let steps = done["plan"]["plan"]["steps"].as_array().unwrap();
    assert!(steps.iter().all(|s| s["receipt"]["check_passed"] == true));

    // A finished plan cannot be approved again.
    let again = client
        .post(&approve)
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 409);
    let missing = client
        .get(format!("{base}/plan-missing"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);

    scheduler_events.abort();
    server.abort();
    let _ = std::fs::remove_dir_all(&dir);
}
