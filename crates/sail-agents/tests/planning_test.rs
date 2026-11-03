//! Agent task planning against the real ToolScheduler, Guardian approvals and a durable
//! Continuity store. The planner is scripted; tools are real and write to a temp workspace.

use async_trait::async_trait;
use sail_agents::planning::*;
use sail_agents::tools::*;
use sail_common::types::SubsystemId;
use sail_continuity::{Continuity, TaskState};
use sail_guardian::{
    ApprovalBroker, AuditLedger, GuardianExecutionBroker, PolicyEngine, SandboxPolicy,
};
use sail_protocol::execution::{ToolDescriptor, ToolEffect, ToolState};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Notes in a temp workspace, plus a tool that waits until released.
struct Notes {
    root: PathBuf,
    writes: AtomicUsize,
    entered: AtomicUsize,
    hold: AtomicBool,
}
struct NoteTool {
    notes: Arc<Notes>,
    kind: &'static str,
}
#[async_trait]
impl Tool for NoteTool {
    async fn execute(&self, context: ToolContext, input: Value) -> Result<Value, ToolFailure> {
        let path = |input: &Value| self.notes.root.join(input["path"].as_str().unwrap_or("x"));
        match self.kind {
            "write" => {
                self.notes.writes.fetch_add(1, Ordering::SeqCst);
                let text = input["text"].as_str().unwrap_or_default();
                let path = path(&input);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, text).unwrap();
                Ok(json!({"bytes": text.len()}))
            }
            "read" => {
                Ok(json!({"text": std::fs::read_to_string(path(&input)).unwrap_or_default()}))
            }
            _ => {
                // "wait" (read-only) and "hold" (effectful): block while held.
                self.notes.entered.fetch_add(1, Ordering::SeqCst);
                while self.notes.hold.load(Ordering::SeqCst) {
                    if context.cancel.is_cancelled() {
                        return Err(ToolFailure {
                            message: "cancelled".into(),
                            retryable: false,
                        });
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok(json!({"text": "released"}))
            }
        }
    }
}

fn descriptor(id: &str, effect: ToolEffect, input: Value, output: Value) -> ToolDescriptor {
    ToolDescriptor {
        id: id.into(),
        version: 1,
        description: format!("Test tool {id}"),
        input_schema: input,
        output_schema: output,
        security_effect: effect,
        required_capabilities: vec![id.into()],
        timeout_ms: 20_000,
        resource_class: "io".into(),
        concurrency_limit: 2,
    }
}

struct Scripted {
    replies: Mutex<VecDeque<String>>,
    requests: Mutex<Vec<PlanRequest>>,
}
#[async_trait]
impl Planner for Scripted {
    fn name(&self) -> String {
        "scripted test planner".into()
    }
    async fn plan(&self, request: &PlanRequest) -> Result<String, String> {
        self.requests.lock().unwrap().push(request.clone());
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or("no more scripted plans".into())
    }
}

struct Harness {
    dir: PathBuf,
    notes: Arc<Notes>,
    registry: Arc<ToolRegistry>,
    approvals: Arc<ApprovalBroker>,
    scheduler: Arc<ToolScheduler>,
    planner: Arc<Scripted>,
    continuity: Arc<Continuity>,
}
impl Harness {
    fn new(replies: &[Value]) -> Self {
        let dir = std::env::temp_dir().join(format!("sail-plan-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = Arc::new(Notes {
            root: dir.join("workspace"),
            writes: AtomicUsize::new(0),
            entered: AtomicUsize::new(0),
            hold: AtomicBool::new(false),
        });
        let registry = Arc::new(ToolRegistry::default());
        register_builtin_time(&registry).unwrap();
        let path = json!({"type":"string","maxLength":200});
        let text_out = json!({"type":"object","properties":{"text":{"type":"string","maxLength":4096}},"required":["text"],"additionalProperties":false});
        let empty_in = json!({"type":"object","properties":{},"additionalProperties":false});
        for (id, kind, effect, input, output) in [
            (
                "notes:write",
                "write",
                ToolEffect::Mutating,
                json!({"type":"object","properties":{"path":path,"text":{"type":"string","maxLength":1000}},"required":["path","text"],"additionalProperties":false}),
                json!({"type":"object","properties":{"bytes":{"type":"integer","minimum":0}},"required":["bytes"],"additionalProperties":false}),
            ),
            (
                "notes:read",
                "read",
                ToolEffect::ReadOnly,
                json!({"type":"object","properties":{"path":path},"required":["path"],"additionalProperties":false}),
                text_out.clone(),
            ),
            (
                "test:wait",
                "wait",
                ToolEffect::ReadOnly,
                empty_in.clone(),
                text_out.clone(),
            ),
            (
                "test:hold",
                "hold",
                ToolEffect::Mutating,
                empty_in.clone(),
                text_out.clone(),
            ),
        ] {
            registry
                .register(
                    descriptor(id, effect, input, output),
                    Arc::new(NoteTool {
                        notes: notes.clone(),
                        kind,
                    }),
                )
                .unwrap();
        }
        let approvals = Arc::new(ApprovalBroker::new());
        let scheduler = Arc::new(ToolScheduler::new(
            registry.clone(),
            Arc::new(GuardianExecutionBroker::new(
                Arc::new(PolicyEngine::with_persistence(dir.join("rules.json"))),
                Arc::new(AuditLedger::new()),
                approvals.clone(),
                Arc::new(SandboxPolicy::new()),
            )),
            4,
        ));
        let planner = Arc::new(Scripted {
            replies: Mutex::new(replies.iter().map(Value::to_string).collect()),
            requests: Mutex::new(Vec::new()),
        });
        let continuity = Arc::new(Continuity::open(dir.join("continuity.db")).unwrap());
        Self {
            dir,
            notes,
            registry,
            approvals,
            scheduler,
            planner,
            continuity,
        }
    }
    fn runner(&self) -> PlanRunner {
        PlanRunner::new(
            self.continuity.clone(),
            self.scheduler.clone(),
            self.registry.clone(),
            self.planner.clone(),
            SubsystemId::Agents,
        )
    }
    /// Answer every Guardian approval request, as the owner would in the UI.
    fn owner_answers(&self, approve: bool) -> tokio::task::JoinHandle<()> {
        let mut events = self.scheduler.subscribe();
        let approvals = self.approvals.clone();
        tokio::spawn(async move {
            while let Ok(event) = events.recv().await {
                if let (ToolState::PolicyPending, Some(session)) =
                    (event.state, event.approval_session)
                {
                    let _ = approvals.resolve_session(session, approve);
                }
            }
        })
    }
    /// A new daemon boot over the same store.
    fn restart(&mut self) {
        self.continuity = Arc::new(Continuity::open(self.dir.join("continuity.db")).unwrap());
        self.continuity.recover().unwrap();
    }
    fn file(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.notes.root.join(rel)).ok()
    }
    async fn wait_entered(&self, n: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.notes.entered.load(Ordering::SeqCst) < n {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the waiting tool never started");
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn step(id: &str, tool: &str, input: Value, deps: &[&str], check: Value) -> Value {
    json!({"id": id, "intent": format!("do {id}"), "tool": tool, "input": input, "depends_on": deps, "check": check})
}
fn plan(steps: Vec<Value>) -> Value {
    json!({"steps": steps})
}
fn ok() -> Value {
    json!({"kind":"tool_succeeded"})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn effectful_plan_waits_for_approval_then_runs_each_step_through_guardian_and_checks_it() {
    let h = Harness::new(&[plan(vec![
        step(
            "time",
            "local:time",
            json!({}),
            &[],
            json!({"kind":"exists","pointer":"/utc"}),
        ),
        step(
            "write",
            "notes:write",
            json!({"path":"notes/time.txt","text":"checked"}),
            &["time"],
            json!({"kind":"equals","pointer":"/bytes","value":7}),
        ),
    ])]);
    let runner = h.runner();
    let created = runner.create("Write a checked note").await.unwrap();
    assert_eq!(created.state, PlanState::AwaitingApproval);
    assert!(created.steps[1].effectful && !created.steps[0].effectful);

    // Nothing runs before the owner approves the plan.
    let waiting = runner
        .run(&created.id, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(waiting.state, PlanState::AwaitingApproval);
    assert_eq!(h.notes.writes.load(Ordering::SeqCst), 0);

    runner.approve(&created.id).unwrap();
    let _owner = h.owner_answers(true); // Guardian still asks for the effectful step itself.
    let done = runner
        .run(&created.id, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(done.state, PlanState::Succeeded, "{:?}", done.history);
    assert_eq!(h.file("notes/time.txt").as_deref(), Some("checked"));
    assert!(
        done.steps
            .iter()
            .all(|s| s.state == StepState::Done && s.receipt.as_ref().unwrap().check_passed)
    );
    let view = runner.get(&created.id).unwrap();
    assert_eq!(view.task_state, TaskState::Succeeded);
    assert!(
        view.plan
            .history
            .iter()
            .any(|l| l.contains("scripted test planner"))
    );
    assert!(
        view.plan
            .history
            .iter()
            .any(|l| l.contains("Approved by the owner"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_resumes_the_plan_without_repeating_a_completed_effect() {
    let mut h = Harness::new(&[plan(vec![
        step(
            "write",
            "notes:write",
            json!({"path":"a.txt","text":"once"}),
            &[],
            ok(),
        ),
        step("wait", "test:wait", json!({}), &["write"], ok()),
    ])]);
    let _owner = h.owner_answers(true);
    h.notes.hold.store(true, Ordering::SeqCst);
    let id = {
        let runner = Arc::new(h.runner());
        let plan = runner.create("Write once, then wait").await.unwrap();
        runner.approve(&plan.id).unwrap();
        let task = {
            let (runner, id) = (runner.clone(), plan.id.clone());
            tokio::spawn(async move { runner.run(&id, CancellationToken::new()).await })
        };
        h.wait_entered(1).await;
        task.abort(); // the daemon dies during step 2
        let _ = task.await;
        plan.id
    };
    assert_eq!(h.notes.writes.load(Ordering::SeqCst), 1);

    h.restart();
    h.notes.hold.store(false, Ordering::SeqCst);
    let runner = h.runner();
    assert_eq!(runner.resume_all().unwrap(), vec![id.clone()]);
    let done = runner.run(&id, CancellationToken::new()).await.unwrap();
    assert_eq!(done.state, PlanState::Succeeded, "{:?}", done.history);
    assert_eq!(
        h.notes.writes.load(Ordering::SeqCst),
        1,
        "the completed write was repeated"
    );
    assert_eq!(h.file("a.txt").as_deref(), Some("once"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_effect_interrupted_by_a_crash_needs_the_owner_and_is_not_retried() {
    let mut h = Harness::new(&[plan(vec![step("hold", "test:hold", json!({}), &[], ok())])]);
    let _owner = h.owner_answers(true);
    h.notes.hold.store(true, Ordering::SeqCst);
    let id = {
        let runner = Arc::new(h.runner());
        let plan = runner
            .create("An effect that never finishes")
            .await
            .unwrap();
        runner.approve(&plan.id).unwrap();
        let task = {
            let (runner, id) = (runner.clone(), plan.id.clone());
            tokio::spawn(async move { runner.run(&id, CancellationToken::new()).await })
        };
        h.wait_entered(1).await;
        task.abort();
        let _ = task.await;
        plan.id
    };
    h.restart();
    let runner = h.runner();
    assert!(
        runner.resume_all().unwrap().is_empty(),
        "an effect in doubt was resumed"
    );
    let view = runner.get(&id).unwrap();
    assert_eq!(view.task_state, TaskState::NeedsIntervention);
    assert_eq!(h.notes.entered.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_drafts_are_rejected_with_reasons_and_the_planner_gets_one_retry() {
    let h = Harness::new(&[]);
    let check = |draft: Value| {
        let draft: PlanDraft = serde_json::from_value(draft).unwrap();
        validate_draft(&draft, &h.registry, &[], None)
            .unwrap_err()
            .join("; ")
    };
    assert!(
        check(plan(vec![step("a", "shell:run", json!({}), &[], ok())])).contains("unknown tool")
    );
    assert!(
        check(plan(vec![
            step("a", "local:time", json!({}), &["b"], ok()),
            step("b", "local:time", json!({}), &["a"], ok()),
        ]))
        .contains("cycle")
    );
    assert!(
        check(plan(vec![step(
            "a",
            "notes:write",
            json!({"path": 3}),
            &[],
            ok()
        )]))
        .contains("input")
    );
    assert!(
        check(plan(vec![
            step("a", "local:time", json!({}), &[], ok()),
            step("a", "local:time", json!({}), &[], ok()),
        ]))
        .contains("duplicate")
    );
    assert!(
        check(plan(vec![step(
            "a",
            "local:time",
            json!({}),
            &["zz"],
            ok()
        )]))
        .contains("unknown step")
    );
    let allowed = ["local:time".to_string()].into_iter().collect();
    let draft: PlanDraft = serde_json::from_value(plan(vec![step(
        "w",
        "notes:write",
        json!({"path":"x","text":"y"}),
        &[],
        ok(),
    )]))
    .unwrap();
    assert!(
        validate_draft(&draft, &h.registry, &[], Some(&allowed)).unwrap_err()[0]
            .contains("not allowed")
    );

    // A first invalid reply is sent back with its errors; the second must be valid.
    let h = Harness::new(&[]);
    h.planner.replies.lock().unwrap().extend([
        "Sure! Here is my plan: {\"steps\": [{\"id\": \"a\", \"intent\": \"x\", \"tool\": \"nope\"}]}".to_string(),
        format!("```json\n{}\n```", plan(vec![step("a", "local:time", json!({}), &[], ok())])),
    ]);
    let runner = h.runner();
    let created = runner.create("What time is it?").await.unwrap();
    assert_eq!(created.state, PlanState::Running);
    let requests = h.planner.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .validation_errors
            .iter()
            .any(|e| e.contains("unknown tool nope"))
    );
    assert!(requests[1].prompt().contains("rejected"));

    let h = Harness::new(&[json!({"steps": []}), json!("not a plan")]);
    let error = h.runner().create("Do something").await.unwrap_err();
    assert!(matches!(error, PlanError::Planning(_)), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_check_leads_to_a_bounded_replan_that_keeps_completed_steps() {
    let h = Harness::new(&[
        plan(vec![
            step("time", "local:time", json!({}), &[], ok()),
            step(
                "read",
                "notes:read",
                json!({"path":"a.txt"}),
                &["time"],
                json!({"kind":"contains","pointer":"/text","text":"hello"}),
            ),
        ]),
        plan(vec![step(
            "read2",
            "notes:read",
            json!({"path":"b.txt"}),
            &["time"],
            json!({"kind":"contains","pointer":"/text","text":"hello"}),
        )]),
    ]);
    std::fs::create_dir_all(&h.notes.root).unwrap();
    std::fs::write(h.notes.root.join("a.txt"), "bye").unwrap();
    std::fs::write(h.notes.root.join("b.txt"), "hello world").unwrap();
    // No plan-level approval (read-only), but Guardian still asks about these reads.
    let _owner = h.owner_answers(true);
    let runner = h.runner();
    let created = runner
        .create("Find the note that says hello")
        .await
        .unwrap();
    let done = runner
        .run(&created.id, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(done.state, PlanState::Succeeded, "{:?}", done.history);
    assert_eq!((done.revision, done.replans), (2, 1));
    assert_eq!(
        done.steps.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["time", "read2"]
    );
    let requests = h.planner.requests.lock().unwrap().clone();
    assert_eq!(
        requests[1].completed.len(),
        1,
        "the re-plan must see the completed step"
    );
    assert!(requests[1].failure.as_deref().unwrap().contains("check"));

    // A plan that never passes stops after MAX_REPLANS.
    let failing = plan(vec![step(
        "r",
        "notes:read",
        json!({"path":"a.txt"}),
        &[],
        json!({"kind":"contains","pointer":"/text","text":"hello"}),
    )]);
    let h2 = Harness::new(&vec![failing; 1 + MAX_REPLANS as usize]);
    std::fs::create_dir_all(&h2.notes.root).unwrap();
    std::fs::write(h2.notes.root.join("a.txt"), "bye").unwrap();
    let _owner2 = h2.owner_answers(true);
    let runner = h2.runner();
    let created = runner.create("Never satisfied").await.unwrap();
    let failed = runner
        .run(&created.id, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(failed.state, PlanState::Failed);
    assert_eq!(failed.replans, MAX_REPLANS);
    assert!(failed.reason.unwrap().contains("re-plans"));
    assert_eq!(
        runner.get(&created.id).unwrap().task_state,
        TaskState::Failed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_denied_step_fails_the_plan_with_the_denial_and_writes_nothing() {
    let h = Harness::new(&[plan(vec![step(
        "w",
        "notes:write",
        json!({"path":"x.txt","text":"no"}),
        &[],
        ok(),
    )])]);
    let _owner = h.owner_answers(false); // the owner denies Guardian's request
    let runner = h.runner();
    let created = runner.create("Write a note").await.unwrap();
    runner.approve(&created.id).unwrap();
    let failed = runner
        .run(&created.id, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(failed.state, PlanState::Failed);
    assert!(
        failed.history.iter().any(|l| l.contains("Step w failed")),
        "{:?}",
        failed.history
    );
    assert_eq!(h.notes.writes.load(Ordering::SeqCst), 0);
    assert!(h.file("x.txt").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_stops_the_running_step_and_the_plan() {
    let h = Harness::new(&[plan(vec![
        step("wait", "test:wait", json!({}), &[], ok()),
        step("time", "local:time", json!({}), &["wait"], ok()),
    ])]);
    h.notes.hold.store(true, Ordering::SeqCst);
    let _owner = h.owner_answers(true);
    let runner = Arc::new(h.runner());
    let created = runner.create("Wait, then read the time").await.unwrap();
    let task = {
        let (runner, id) = (runner.clone(), created.id.clone());
        tokio::spawn(async move { runner.run(&id, CancellationToken::new()).await })
    };
    h.wait_entered(1).await;
    runner
        .cancel(&created.id, "owner changed their mind")
        .unwrap();
    let cancelled = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.state, PlanState::Cancelled);
    assert_eq!(
        cancelled.replans, 0,
        "cancellation must not trigger a re-plan"
    );
    assert_eq!(
        runner.get(&created.id).unwrap().task_state,
        TaskState::Cancelled
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owner_can_edit_a_plan_before_approving_it() {
    let h = Harness::new(&[plan(vec![step(
        "w",
        "notes:write",
        json!({"path":"draft.txt","text":"model"}),
        &[],
        ok(),
    )])]);
    let _owner = h.owner_answers(true);
    let runner = h.runner();
    let created = runner.create("Write a note").await.unwrap();
    let edited: PlanDraft = serde_json::from_value(plan(vec![step(
        "w",
        "notes:write",
        json!({"path":"draft.txt","text":"owner"}),
        &[],
        ok(),
    )]))
    .unwrap();
    let plan = runner.edit(&created.id, &edited).unwrap();
    assert_eq!(
        (plan.revision, plan.state),
        (2, PlanState::AwaitingApproval)
    );
    let bad: PlanDraft =
        serde_json::from_value(json!({"steps":[{"id":"w","intent":"x","tool":"shell:run"}]}))
            .unwrap();
    assert!(runner.edit(&created.id, &bad).is_err());
    runner.approve(&created.id).unwrap();
    assert!(
        runner.edit(&created.id, &edited).is_err(),
        "edits after approval"
    );
    let done = runner
        .run(&created.id, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(done.state, PlanState::Succeeded);
    assert_eq!(h.file("draft.txt").as_deref(), Some("owner"));
}
