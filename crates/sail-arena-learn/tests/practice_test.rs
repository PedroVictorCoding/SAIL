//! Practice sessions (#244): solution hiding, one active problem, hints and honest outcomes.
mod common;
use common::*;
use sail_arena_learn::core::events::{LearningEvent, TurnOutcome};
use sail_arena_learn::core::practice::PracticeOutcome;
use sail_arena_learn::core::view::{Entry, Mode};
use sail_arena_learn::{Engine, SqliteEventStore};
use serde_json::json;
use std::sync::Arc;

fn engine(model: Arc<Scripted>) -> (Arc<Engine>, Arc<SqliteEventStore>) {
    let store = Arc::new(SqliteEventStore::open_in_memory().unwrap());
    (Arc::new(Engine::new(store.clone(), model)), store)
}

fn problem() -> serde_json::Value {
    json!({
        "statement": "A sender halves its window after a loss. What happens next?",
        "solution": "The sending rate drops, then grows again as ACKs arrive.",
        "hints": ["Think about the congestion window.", "ACKs make it grow."]
    })
}

#[tokio::test]
async fn practice_hides_the_solution_and_records_an_honest_outcome() {
    let model = Arc::new(Scripted::new(vec![
        call("problem", problem()),
        call("problem", problem()), // rejected: one active problem
        say("One problem at a time."),
        call("conclude", json!({"outcome":"solved_with_hints"})),
    ]));
    let (engine, _) = engine(model.clone());
    let view = engine
        .create_practice_session("Congestion control")
        .unwrap();
    assert_eq!(view.mode, Mode::Practice);
    assert!(view.needs_turn);

    assert_eq!(
        engine.run_turn(&view.session).await.unwrap(),
        TurnOutcome::AwaitingLearner
    );
    let v = engine.view(&view.session).unwrap();
    let practice = v.practice.as_ref().unwrap();
    let pid = practice.active_problem.clone().unwrap();
    let problem = practice.active().unwrap();
    assert_eq!(problem.hint_count, 2);
    assert!(
        problem.hints.is_empty(),
        "no hint is shown before it is requested"
    );
    assert!(problem.solution.is_none(), "the solution is hidden");
    let json = serde_json::to_string(&v).unwrap();
    assert!(!json.contains("The sending rate drops"), "{json}");
    assert!(
        !json.contains("Think about the congestion window"),
        "{json}"
    );
    assert_eq!(v.progress.visited, Vec::<String>::new());
    assert!(v.plan.is_none());

    // A second problem is refused while one is active, with a correctable error.
    assert_eq!(
        engine.run_turn(&view.session).await.unwrap(),
        TurnOutcome::Completed
    );
    let errors: Vec<String> = engine
        .events(&view.session)
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
        errors.iter().any(|e| e.contains("already active")),
        "{errors:?}"
    );

    // Hints are requested one at a time by the learner and recorded per problem.
    let v = engine.request_hint(&view.session, &pid, 0).unwrap();
    assert_eq!(
        v.practice.as_ref().unwrap().active().unwrap().hints,
        ["Think about the congestion window."]
    );
    let v = engine.request_hint(&view.session, &pid, 1).unwrap();
    assert_eq!(
        v.practice.as_ref().unwrap().active().unwrap().hints.len(),
        2
    );
    assert!(engine.request_hint(&view.session, &pid, 2).is_err());
    assert_eq!(
        engine
            .events(&view.session)
            .unwrap()
            .iter()
            .filter(|e| matches!(e.event, LearningEvent::HintRequested { .. }))
            .count(),
        2
    );

    // The learner attempts the problem; the practitioner concludes it.
    engine
        .attempt(
            &view.session,
            &pid,
            "The window shrinks and then grows again.",
        )
        .unwrap();
    assert_eq!(
        engine.run_turn(&view.session).await.unwrap(),
        TurnOutcome::Completed
    );
    let v = engine.view(&view.session).unwrap();
    let done = v.practice.as_ref().unwrap().problems.last().unwrap();
    assert_eq!(done.outcome, Some(PracticeOutcome::SolvedWithHints));
    assert_eq!(
        done.solution.as_deref(),
        Some("The sending rate drops, then grows again as ACKs arrive.")
    );
    assert_eq!(
        v.practice.as_ref().unwrap().active_problem,
        None,
        "conclude closes the active problem"
    );
    // Practice outcome is session evidence, not plan progress.
    assert_eq!(v.progress.visited, Vec::<String>::new());
    assert!(v.plan.is_none());
    assert!(
        v.transcript
            .iter()
            .any(|e| matches!(e, Entry::Problem { problem_id, .. } if problem_id == &pid))
    );
}

#[tokio::test]
async fn practice_tools_are_not_available_during_teaching() {
    let model = Arc::new(Scripted::new(vec![
        call("problem", problem()),
        say("That tool is only for practice."),
    ]));
    let (engine, _) = engine(model);
    let s = engine.create_session("Understand TCP").unwrap().session;
    assert_eq!(engine.run_turn(&s).await.unwrap(), TurnOutcome::Completed);
    let v = engine.view(&s).unwrap();
    assert!(v.practice.is_none());
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
    assert!(
        errors
            .iter()
            .any(|e| e.contains("not available to the prober"))
    );
}
