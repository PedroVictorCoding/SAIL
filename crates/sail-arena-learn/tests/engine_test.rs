//! Turn runner, handoff, answers and failure paths with a scripted model (#240, #241, #242).
mod common;
use common::*;
use sail_arena_learn::core::events::{
    AgentRole, LearningEvent, NewEvent, ToolCallRecord, TurnOutcome,
};
use sail_arena_learn::core::question::{Answer, Grade};
use sail_arena_learn::core::view::{Entry, Mode};
use sail_arena_learn::{Engine, EventStore, SqliteEventStore, TurnError};
use serde_json::json;
use std::sync::Arc;

fn engine(model: Arc<Scripted>) -> (Arc<Engine>, Arc<SqliteEventStore>) {
    let store = Arc::new(SqliteEventStore::open_in_memory().unwrap());
    (Arc::new(Engine::new(store.clone(), model)), store)
}

fn probe() -> serde_json::Value {
    json!({"kind":"single","prompt":"What does TCP guarantee?",
           "options":[{"id":"a","text":"In-order delivery"},{"id":"b","text":"Low latency"},
                      {"id":"c","text":"I don't know"}],"correct":["a"]})
}

fn plan() -> serde_json::Value {
    json!({"goal":"cc","nodes":[{"id":"ack","title":"Acknowledgements"},
            {"id":"win","title":"Sliding window"},{"id":"cc","title":"Congestion control"}],
           "edges":[["ack","win"],["win","cc"]]})
}

fn choose(id: &str) -> Answer {
    Answer {
        choices: vec![id.into()],
        confidence: Some(4),
        ..Answer::default()
    }
}

fn tool_names(model: &Scripted, i: usize) -> Vec<String> {
    model.requests.lock().unwrap()[i]
        .tools
        .iter()
        .map(|t| t.function.name.clone())
        .collect()
}

#[tokio::test]
async fn probe_handoff_plan_explain_quiz_and_answer_once() {
    let model = Arc::new(Scripted::new(vec![
        call("probe", probe()),
        call(
            "probe_done",
            json!({"summary":"Knows TCP basics; windowing unclear",
                                  "known":["acks"],"gaps":["windows"]}),
        ),
        calls(
            "Let's start.",
            &[
                ("plan_init", plan()),
                (
                    "explain",
                    json!({"node_id":"ack","text":"Each ACK confirms bytes received."}),
                ),
            ],
        ),
        call(
            "quiz",
            json!({"node_id":"ack","kind":"free_recall",
                            "prompt":"Why does the sender need ACKs?",
                            "rubric":"ACKs tell the sender which bytes arrived"}),
        ),
    ]));
    let (engine, _) = engine(model.clone());
    let view = engine
        .create_session("Understand TCP congestion control")
        .unwrap();
    assert!(view.needs_turn);
    assert_eq!(view.mode, Mode::Assess);

    assert_eq!(
        engine.run_turn(&view.session).await.unwrap(),
        TurnOutcome::AwaitingLearner
    );
    assert_eq!(
        tool_names(&model, 0),
        [
            "probe",
            "ask_user_question",
            "probe_done",
            "search_knowledge"
        ]
    );
    let v = engine.view(&view.session).unwrap();
    let q = v.active_question.clone().unwrap();
    // The answer key is hidden until the learner answers.
    let Some(Entry::Question(shown)) = v.transcript.last() else {
        panic!()
    };
    assert!(shown.correct.is_none());
    assert!(
        engine
            .run_turn(&view.session)
            .await
            .is_err_and(|e| matches!(e, TurnError::QuestionOpen))
    );

    let first = engine.answer(&view.session, &q, choose("a")).unwrap();
    assert_eq!((first.grade, first.duplicate), (Grade::Correct, false));
    let again = engine.answer(&view.session, &q, choose("b")).unwrap();
    assert_eq!((again.grade, again.duplicate), (Grade::Correct, true));
    let answers = engine
        .events(&view.session)
        .unwrap()
        .iter()
        .filter(|e| matches!(e.event, LearningEvent::Answered { .. }))
        .count();
    assert_eq!(answers, 1);

    // probe_done hands off to the teacher inside the same turn.
    assert_eq!(
        engine.run_turn(&view.session).await.unwrap(),
        TurnOutcome::AwaitingLearner
    );
    assert_eq!(
        tool_names(&model, 2),
        [
            "plan_init",
            "explain",
            "quiz",
            "ask_user_question",
            "show_diagram",
            "search_knowledge"
        ]
    );
    let v = engine.view(&view.session).unwrap();
    assert_eq!(v.mode, Mode::Teach);
    assert_eq!(v.plan.as_ref().unwrap().revision, 1);
    assert_eq!(v.plan.as_ref().unwrap().order, ["ack", "win", "cc"]);
    assert_eq!(v.progress.visited, ["ack"]);
    assert_eq!(v.progress.current.as_deref(), Some("ack"));
    assert_eq!(v.progress.upcoming, ["win", "cc"]);
    assert!(
        v.transcript
            .iter()
            .any(|e| matches!(e, Entry::Explain { node_id, .. } if node_id == "ack"))
    );
    assert!(
        v.probe
            .as_ref()
            .unwrap()
            .gaps
            .contains(&"windows".to_string())
    );
    // The teacher saw the learner's graded probe answer as the probe's tool result.
    let teacher_request = &model.requests.lock().unwrap()[1];
    assert!(teacher_request.messages.iter().any(|m| {
        m.role == sail_protocol::Role::Tool
            && m.content
                .text()
                .is_some_and(|t| t.contains("In-order delivery") && t.contains("correct"))
    }));
    assert_eq!(v.input_tokens, 400);
    assert_eq!(model.remaining(), 0);
}

#[tokio::test]
async fn role_allowlist_and_plan_checks_return_correctable_errors() {
    let model = Arc::new(Scripted::new(vec![
        call("plan_init", plan()),
        call("explain", json!({"node_id":"x","text":"t"})),
        call("probe", json!(probe().to_string())), // stringified: healed
    ]));
    let (engine, _) = engine(model.clone());
    let s = engine.create_session("goal").unwrap().session;
    assert_eq!(
        engine.run_turn(&s).await.unwrap(),
        TurnOutcome::AwaitingLearner
    );
    let results: Vec<String> = engine
        .events(&s)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            LearningEvent::ToolResult {
                content, ok: false, ..
            } => Some(content),
            _ => None,
        })
        .collect();
    assert!(
        results[0].contains("`plan_init` is not available to the prober"),
        "{results:?}"
    );
    assert!(
        results[1].contains("`explain` is not available"),
        "{results:?}"
    );
    assert!(engine.view(&s).unwrap().plan.is_none());
}

#[tokio::test]
async fn offline_model_records_failure_without_fabricating() {
    let model = Arc::new(Scripted::new(vec![Step::Fail("model offline".into())]));
    let (engine, _) = engine(model.clone());
    let s = engine.create_session("goal").unwrap().session;
    let outcome = engine.run_turn(&s).await.unwrap();
    assert_eq!(
        outcome,
        TurnOutcome::Failed {
            message: "model offline".into()
        }
    );
    let v = engine.view(&s).unwrap();
    assert_eq!(v.last_failure.as_deref(), Some("model offline"));
    assert!(
        !v.transcript
            .iter()
            .any(|e| matches!(e, Entry::Assistant { .. }))
    );
    // Retry works once the model is back.
    model.push(call("probe", probe()));
    assert_eq!(
        engine.run_turn(&s).await.unwrap(),
        TurnOutcome::AwaitingLearner
    );
    assert!(engine.view(&s).unwrap().last_failure.is_none());
}

const OVERFLOW: &str =
    "Rendered prompt, image reservation and response headroom exceed the model context window";

#[tokio::test]
async fn context_overflow_retries_with_less_history() {
    let model = Arc::new(Scripted::new(vec![
        Step::Fail(OVERFLOW.into()),
        call("probe", probe()),
    ]));
    let (engine, _) = engine(model.clone());
    let s = engine.create_session("goal").unwrap().session;
    assert_eq!(
        engine.run_turn(&s).await.unwrap(),
        TurnOutcome::AwaitingLearner
    );
    assert_eq!(model.requests.lock().unwrap().len(), 2);
    assert!(engine.view(&s).unwrap().last_failure.is_none());
}

#[tokio::test]
async fn persistent_context_overflow_fails_after_shrinking() {
    let model = Arc::new(Scripted::new(
        (0..10).map(|_| Step::Fail(OVERFLOW.into())).collect(),
    ));
    let (engine, _) = engine(model.clone());
    let s = engine.create_session("goal").unwrap().session;
    assert_eq!(
        engine.run_turn(&s).await.unwrap(),
        TurnOutcome::Failed {
            message: OVERFLOW.into()
        }
    );
    // 96 → 48 → 24 → 12 → 8 KiB, then the refusal is reported.
    assert_eq!(model.requests.lock().unwrap().len(), 5);
}

#[tokio::test]
async fn transient_memory_pressure_waits_then_continues() {
    let model = Arc::new(Scripted::new(vec![
        Step::Fail(
            "resource admission denied: host memory pressure 33.0% exceeds admission limit".into(),
        ),
        call("probe", probe()),
    ]));
    let (engine, _) = engine(model.clone());
    let s = engine.create_session("goal").unwrap().session;
    assert_eq!(
        engine.run_turn(&s).await.unwrap(),
        TurnOutcome::AwaitingLearner
    );
    assert_eq!(model.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn background_turn_is_busy_then_aborts_cleanly() {
    let model = Arc::new(Scripted::new(vec![Step::Hang]));
    let (engine, _) = engine(model.clone());
    let s = engine.create_session("goal").unwrap().session;
    engine.start_turn(&s).unwrap();
    assert!(matches!(engine.start_turn(&s), Err(TurnError::Busy)));
    assert!(matches!(engine.run_turn(&s).await, Err(TurnError::Busy)));
    assert!(matches!(
        engine.message(&s, "k1", "hello"),
        Err(TurnError::Busy)
    ));
    while model.requests.lock().unwrap().is_empty() {
        tokio::task::yield_now().await;
    }
    assert!(engine.abort(&s));
    while engine.is_running(&s) {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let last = engine.events(&s).unwrap().pop().unwrap();
    assert!(matches!(
        last.event,
        LearningEvent::TurnSettled {
            outcome: TurnOutcome::Aborted,
            ..
        }
    ));
    assert!(!engine.abort(&s));
}

#[tokio::test]
async fn crash_after_handoff_resumes_with_the_teacher_once() {
    let model = Arc::new(Scripted::new(vec![say("Here is the plan we will follow.")]));
    let (engine, store) = engine(model.clone());
    let s = engine.create_session("goal").unwrap().session;
    // A turn that died after probe_done was stored but before the teacher ran.
    store
        .append(
            &s,
            1,
            5,
            &[
                NewEvent::new(
                    "x1",
                    LearningEvent::AssistantMessage {
                        role: AgentRole::Prober,
                        text: String::new(),
                        tool_calls: vec![ToolCallRecord {
                            id: "call_x".into(),
                            name: "probe_done".into(),
                            arguments: "{}".into(),
                        }],
                    },
                ),
                NewEvent::new(
                    "x2",
                    LearningEvent::ToolResult {
                        call_id: "call_x".into(),
                        content: "ok".into(),
                        ok: true,
                    },
                ),
                NewEvent::new(
                    "x3",
                    LearningEvent::ProbeDone {
                        summary: "s".into(),
                        known: vec![],
                        gaps: vec![],
                    },
                ),
            ],
        )
        .unwrap();
    let v = engine.view(&s).unwrap();
    assert!(v.needs_turn);
    assert_eq!(v.role(), AgentRole::Teacher);
    assert_eq!(engine.run_turn(&s).await.unwrap(), TurnOutcome::Completed);
    assert_eq!(tool_names(&model, 0)[0], "plan_init");
    assert!(!engine.view(&s).unwrap().needs_turn);
}

#[tokio::test]
async fn loop_is_bounded_and_extra_calls_after_a_question_are_skipped() {
    let steps = (0..8).map(|_| call("probe_done", json!({}))).collect(); // missing summary
    let model = Arc::new(Scripted::new(steps));
    let (engine, _) = engine(model.clone());
    let s = engine.create_session("goal").unwrap().session;
    assert_eq!(engine.run_turn(&s).await.unwrap(), TurnOutcome::LoopLimit);
    assert!(engine.view(&s).unwrap().last_failure.is_some());

    model.push(calls("", &[("probe", probe()), ("probe", probe())]));
    assert_eq!(
        engine.run_turn(&s).await.unwrap(),
        TurnOutcome::AwaitingLearner
    );
    let skipped = engine
        .events(&s)
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(&e.event,
        LearningEvent::ToolResult { content, .. } if content.starts_with("Skipped"))
        })
        .count();
    assert_eq!(skipped, 1);
    assert!(matches!(
        engine.message(&s, "k", "hi"),
        Err(TurnError::QuestionOpen)
    ));
}

#[tokio::test]
async fn pause_blocks_turns_until_resumed() {
    let model = Arc::new(Scripted::new(vec![call("probe", probe())]));
    let (engine, _) = engine(model);
    let s = engine.create_session("goal").unwrap().session;
    assert!(engine.set_paused(&s, true).unwrap().paused);
    assert!(matches!(engine.run_turn(&s).await, Err(TurnError::Paused)));
    let v = engine.set_paused(&s, false).unwrap();
    assert!(v.needs_turn);
    assert_eq!(
        engine.run_turn(&s).await.unwrap(),
        TurnOutcome::AwaitingLearner
    );
}

#[tokio::test]
async fn role_model_slots_are_explicit_and_not_guessed() {
    let model = Arc::new(Scripted::new(vec![call("probe", probe())]));
    let store = Arc::new(SqliteEventStore::open_in_memory().unwrap());
    let mut roles = sail_arena_learn::RoleModels::default();
    roles.set("prober", "tiny-local");
    let engine = Arc::new(Engine::new(store, model.clone()).with_models(roles));
    let s = engine.create_session("goal").unwrap().session;
    engine.run_turn(&s).await.unwrap();
    assert_eq!(
        model.requests.lock().unwrap()[0].model.as_deref(),
        Some("tiny-local")
    );
}

#[tokio::test]
async fn diagrams_are_rendered_as_events_and_failures_reach_the_next_turn() {
    let model = Arc::new(Scripted::new(vec![
        calls(
            "Here is a diagram.",
            &[
                ("plan_init", plan()),
                (
                    "show_diagram",
                    json!({"node_id":"ack","mermaid":"flowchart TD\n  a-->b","caption":"A to B"}),
                ),
            ],
        ),
        say("What do you notice?"),
        say("Let's try a simpler one."),
    ]));
    let (engine, store) = engine(model.clone());
    let s = engine.create_session("goal").unwrap().session;
    store
        .append(
            &s,
            1,
            5,
            &[NewEvent::new(
                "handoff",
                LearningEvent::ProbeDone {
                    summary: "ready".into(),
                    known: vec![],
                    gaps: vec![],
                },
            )],
        )
        .unwrap();
    assert_eq!(engine.run_turn(&s).await.unwrap(), TurnOutcome::Completed);
    let v = engine.view(&s).unwrap();
    let Some(Entry::Diagram {
        call_id,
        mermaid,
        error,
        ..
    }) = v
        .transcript
        .iter()
        .find(|e| matches!(e, Entry::Diagram { .. }))
    else {
        panic!("no diagram entry")
    };
    assert_eq!(mermaid, "flowchart TD\n  a-->b");
    assert!(error.is_none());
    engine
        .diagram_failed(&s, call_id, "strict Mermaid parse error")
        .unwrap();
    let v = engine.view(&s).unwrap();
    assert!(matches!(
        v.transcript.iter().find(|e| matches!(e, Entry::Diagram { .. })),
        Some(Entry::Diagram { error: Some(e), .. }) if e.contains("parse error")
    ));
    // The next turn's prompt carries the browser failure as a tool error note.
    engine.message(&s, "m1", "try again").unwrap();
    engine.run_turn(&s).await.unwrap();
    let last = model.requests.lock().unwrap().last().unwrap().clone();
    assert!(last.messages.iter().any(|m| {
        m.content
            .text()
            .is_some_and(|t| t.contains("show_diagram") && t.contains("parse error"))
    }));
}

#[tokio::test]
async fn diagram_arguments_cannot_smuggle_html() {
    let model = Arc::new(Scripted::new(vec![
        calls(
            "",
            &[
                ("plan_init", plan()),
                (
                    "show_diagram",
                    json!({"node_id":"ack","mermaid":"<script>alert(1)</script>","caption":"x"}),
                ),
            ],
        ),
        say("No HTML."),
    ]));
    let (engine, store) = engine(model);
    let s = engine.create_session("goal").unwrap().session;
    store
        .append(
            &s,
            1,
            5,
            &[NewEvent::new(
                "handoff2",
                LearningEvent::ProbeDone {
                    summary: "ready".into(),
                    known: vec![],
                    gaps: vec![],
                },
            )],
        )
        .unwrap();
    engine.run_turn(&s).await.unwrap();
    assert!(
        engine
            .view(&s)
            .unwrap()
            .transcript
            .iter()
            .all(|e| !matches!(e, Entry::Diagram { .. }))
    );
    let errors: Vec<String> = engine
        .events(&s)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            LearningEvent::ToolResult {
                content, ok: false, ..
            } => Some(content),
            _ => None,
        })
        .collect();
    assert!(errors.iter().any(|e| e.contains("must not contain HTML")));
}

#[tokio::test]
async fn picked_context_too_large_for_the_model_window_is_refused() {
    use sail_arena_learn::core::MAX_GRAPH_CONTEXT_BYTES;
    use sail_arena_learn::core::graph::GraphContextProp;

    let (engine, store) = engine(Arc::new(Scripted::new(Vec::new())));
    let prop = |i: usize| GraphContextProp {
        name: format!("p{i}"),
        statement: "x".repeat(400),
    };
    let fits: Vec<_> = (0..MAX_GRAPH_CONTEXT_BYTES / 410).map(prop).collect();
    assert!(
        engine
            .create_session_with_options("goal", "practice", fits)
            .is_ok()
    );

    let sessions = store.sessions(100).unwrap().len();
    let too_many: Vec<_> = (0..MAX_GRAPH_CONTEXT_BYTES / 400 + 1).map(prop).collect();
    let err = engine
        .create_session_with_options("goal", "practice", too_many)
        .unwrap_err();
    assert!(
        matches!(&err, TurnError::Invalid(m) if m.contains("pick fewer")),
        "{err}"
    );
    // Refused before anything is stored.
    assert_eq!(store.sessions(100).unwrap().len(), sessions);
}
