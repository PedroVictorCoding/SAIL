//! Free-recall judgment with a rubric (#243), over a scripted model.
mod common;
use common::*;
use sail_arena_learn::core::events::{LearningEvent, TurnOutcome};
use sail_arena_learn::core::judgment::Verdict;
use sail_arena_learn::core::question::{Answer, Grade};
use sail_arena_learn::core::view::{Entry, LearnerQuestion};
use sail_arena_learn::{Engine, SqliteEventStore};
use sail_protocol::Role;
use serde_json::json;
use std::sync::Arc;

const RUBRIC: &str = "A full router queue has no room, so new packets are dropped";

fn engine(model: Arc<Scripted>) -> Arc<Engine> {
    let store = Arc::new(SqliteEventStore::open_in_memory().unwrap());
    Arc::new(Engine::new(store, model))
}

/// The scripted steps that take a new session to an open free-recall quiz on `loss`.
fn to_quiz(quiz: serde_json::Value) -> Vec<Step> {
    vec![
        call("probe_done", json!({"summary":"New to networking"})),
        calls(
            "",
            &[
                (
                    "plan_init",
                    json!({"goal":"loss","nodes":[{"id":"loss","title":"Packet loss"}],"edges":[]}),
                ),
                (
                    "explain",
                    json!({"node_id":"loss","text":"Routers queue packets; queues are finite."}),
                ),
            ],
        ),
        call("quiz", quiz),
    ]
}

fn quiz() -> serde_json::Value {
    json!({"node_id":"loss","kind":"free_recall","prompt":"Why do routers drop packets?",
           "rubric":RUBRIC})
}

fn judge(args: serde_json::Value) -> Step {
    call("judge", args)
}

fn text(t: &str) -> Answer {
    Answer {
        text: Some(t.into()),
        ..Answer::default()
    }
}

async fn open_quiz(model: &Arc<Scripted>, engine: &Arc<Engine>) -> (String, String) {
    let s = engine
        .create_session("Understand packet loss")
        .unwrap()
        .session;
    assert_eq!(
        engine.run_turn(&s).await.unwrap(),
        TurnOutcome::AwaitingLearner
    );
    let q = engine.view(&s).unwrap().active_question.unwrap();
    assert_eq!(model.remaining(), 0);
    (s, q)
}

fn question(engine: &Engine, s: &str, id: &str) -> LearnerQuestion {
    engine
        .view(s)
        .unwrap()
        .transcript
        .into_iter()
        .find_map(|e| match e {
            Entry::Question(q) if q.id == id => Some(*q),
            _ => None,
        })
        .unwrap()
}

fn tool_results(model: &Scripted, request: usize) -> Vec<String> {
    model.requests.lock().unwrap()[request]
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.content.text().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn partial_answer_is_judged_against_the_hidden_rubric_before_the_tutor_replies() {
    let model = Arc::new(Scripted::new(to_quiz(quiz())));
    let engine = engine(model.clone());
    let (s, q) = open_quiz(&model, &engine).await;

    // The rubric is stored but never reaches the learner's view.
    let view = serde_json::to_string(&engine.view(&s).unwrap()).unwrap();
    assert!(!view.contains(RUBRIC), "rubric leaked into the view");

    let outcome = engine.answer(&s, &q, text("Because of errors")).unwrap();
    assert_eq!(outcome.grade, Grade::NeedsJudgment);
    model.push(judge(json!({"verdict":"partial",
        "missing":"a full queue forces the router to drop new packets",
        "feedback":"Errors cause some loss, but the main cause is a full queue."})));
    model.push(say("Let's look at queues again."));
    assert_eq!(engine.run_turn(&s).await.unwrap(), TurnOutcome::Completed);

    // The judge call is separate: its own prompt, the rubric, only the `judge` tool.
    let judge_request = model.requests.lock().unwrap()[3].clone();
    assert_eq!(judge_request.tools.len(), 1);
    assert_eq!(judge_request.tools[0].function.name, "judge");
    let case = judge_request.messages[1].content.text().unwrap();
    assert!(case.contains(RUBRIC) && case.contains("Because of errors"));
    assert!(
        !case.contains("Routers queue packets"),
        "the judge does not see the lesson"
    );

    let judged = question(&engine, &s, &q).judgment.unwrap();
    assert_eq!(judged.verdict, Verdict::Partial);
    assert_eq!(judged.attempts, 1);
    assert!(judged.missing.unwrap().contains("full queue"));
    // The tutor saw the verdict and the missing relation as the quiz's result.
    let results = tool_results(&model, 4);
    let quiz_result = results
        .iter()
        .find(|r| r.contains("learner_answer"))
        .unwrap();
    assert!(
        quiz_result.contains("\"verdict\":\"partial\""),
        "{quiz_result}"
    );
    assert!(quiz_result.contains("Missing or wrong: a full queue"));
    assert_eq!(model.remaining(), 0);
    let v = engine.view(&s).unwrap();
    assert!(!v.needs_turn);
    assert_eq!(v.input_tokens, 500, "judge tokens count toward the session");
}

#[tokio::test]
async fn unclear_answer_gets_one_follow_up_instead_of_a_verdict() {
    let model = Arc::new(Scripted::new(to_quiz(quiz())));
    let engine = engine(model.clone());
    let (s, q) = open_quiz(&model, &engine).await;
    engine.answer(&s, &q, text("idk, stuff happens")).unwrap();

    model.push(judge(json!({"verdict":"unclear",
        "follow_up":"What happens to a packet that arrives when the queue is full?"})));
    assert_eq!(
        engine.run_turn(&s).await.unwrap(),
        TurnOutcome::AwaitingLearner
    );
    assert_eq!(model.requests.lock().unwrap().len(), 4, "no tutor call");
    let v = engine.view(&s).unwrap();
    let follow = v.active_question.clone().unwrap();
    assert_ne!(follow, q);
    let asked = question(&engine, &s, &follow);
    assert_eq!(asked.follow_up_of.as_deref(), Some(q.as_str()));
    assert_eq!(asked.node_id.as_deref(), Some("loss"));
    assert!(asked.prompt.contains("queue is full"));
    assert_eq!(
        question(&engine, &s, &q).judgment.unwrap().verdict,
        Verdict::Unclear
    );

    // The follow-up is judged with the same rubric. Unclear again: no second follow-up;
    // the tutor takes over without a verdict.
    engine.answer(&s, &follow, text("it waits?")).unwrap();
    model.push(judge(
        json!({"verdict":"unclear","follow_up":"And then what?"}),
    ));
    model.push(say("Here is another way to see it."));
    assert_eq!(engine.run_turn(&s).await.unwrap(), TurnOutcome::Completed);
    let second_judge = model.requests.lock().unwrap()[4].clone();
    assert!(
        second_judge.messages[1]
            .content
            .text()
            .unwrap()
            .contains(RUBRIC)
    );
    let last = question(&engine, &s, &follow).judgment.unwrap();
    assert_eq!(last.verdict, Verdict::Unclear);
    assert!(last.follow_up.is_none(), "no follow-up was asked");
    let results = tool_results(&model, 5);
    // The tutor's history pairs the follow-up call with its answer.
    assert!(
        results
            .iter()
            .any(|r| r.contains("SAIL asked the learner the follow-up"))
    );
    assert!(results.iter().any(|r| r.contains("Still unclear")));
    assert!(engine.view(&s).unwrap().active_question.is_none());
}

#[tokio::test]
async fn malformed_judgment_is_retried_once_then_never_guessed() {
    let model = Arc::new(Scripted::new(to_quiz(quiz())));
    let engine = engine(model.clone());
    let (s, q) = open_quiz(&model, &engine).await;
    engine.answer(&s, &q, text("queues overflow")).unwrap();

    model.push(say("I think this is right!"));
    model.push(judge(json!({"verdict":"incorrect"}))); // missing `missing`
    model.push(say("Let's go on."));
    assert_eq!(engine.run_turn(&s).await.unwrap(), TurnOutcome::Completed);
    let retry = model.requests.lock().unwrap()[4].clone();
    assert!(
        retry.messages[2]
            .content
            .text()
            .unwrap()
            .contains("could not be used: no `judge` call")
    );
    let judged = question(&engine, &s, &q).judgment.unwrap();
    assert_eq!(judged.verdict, Verdict::CouldNotJudge);
    assert_eq!(judged.attempts, 2);
    assert!(judged.missing.is_none() && judged.feedback.is_none());
    let results = tool_results(&model, 5);
    assert!(
        results
            .iter()
            .any(|r| r.contains("could not judge") && r.contains("Do not guess"))
    );
}

#[tokio::test]
async fn retry_can_recover_a_judgment() {
    let model = Arc::new(Scripted::new(to_quiz(quiz())));
    let engine = engine(model.clone());
    let (s, q) = open_quiz(&model, &engine).await;
    engine
        .answer(&s, &q, text("full queues drop new packets"))
        .unwrap();
    model.push(judge(json!({"verdict":"brilliant"})));
    model.push(say(
        r#"{"verdict":"correct","feedback":"You linked the queue to the drop."}"#,
    ));
    model.push(say("Next step."));
    engine.run_turn(&s).await.unwrap();
    let judged = question(&engine, &s, &q).judgment.unwrap();
    assert_eq!((judged.verdict, judged.attempts), (Verdict::Correct, 2));
}

#[tokio::test]
async fn offline_judge_leaves_the_answer_unjudged_and_the_next_turn_retries() {
    let model = Arc::new(Scripted::new(to_quiz(quiz())));
    let engine = engine(model.clone());
    let (s, q) = open_quiz(&model, &engine).await;
    engine.answer(&s, &q, text("queues overflow")).unwrap();
    let before = engine.events(&s).unwrap().len();

    model.push(Step::Fail("model offline".into()));
    assert_eq!(
        engine.run_turn(&s).await.unwrap(),
        TurnOutcome::Failed {
            message: "model offline".into()
        }
    );
    assert!(question(&engine, &s, &q).judgment.is_none());
    // Only the failed settlement was recorded: no verdict, no tutor reply.
    let added: Vec<_> = engine.events(&s).unwrap().split_off(before);
    assert_eq!(added.len(), 1);
    assert!(matches!(
        added[0].event,
        LearningEvent::TurnSettled {
            outcome: TurnOutcome::Failed { .. },
            ..
        }
    ));

    model.push(judge(json!({"verdict":"correct"})));
    model.push(say("Good reasoning about queues."));
    assert_eq!(engine.run_turn(&s).await.unwrap(), TurnOutcome::Completed);
    let judgments = engine
        .events(&s)
        .unwrap()
        .iter()
        .filter(|e| matches!(e.event, LearningEvent::AnswerJudged { .. }))
        .count();
    assert_eq!(judgments, 1);
    // A later turn does not judge the same answer again.
    engine.message(&s, "m1", "Explain it differently").unwrap();
    model.push(say("Think of a full parking lot."));
    engine.run_turn(&s).await.unwrap();
    assert_eq!(model.remaining(), 0);
    assert_eq!(
        model.requests.lock().unwrap().last().unwrap().tools.len(),
        6,
        "the last call was the tutor's"
    );
}

#[tokio::test]
async fn abort_during_judgment_records_nothing_but_the_abort() {
    let model = Arc::new(Scripted::new(to_quiz(quiz())));
    let engine = engine(model.clone());
    let (s, q) = open_quiz(&model, &engine).await;
    engine.answer(&s, &q, text("queues overflow")).unwrap();
    model.push(Step::Hang);
    engine.start_turn(&s).unwrap();
    while model.requests.lock().unwrap().len() < 4 {
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
    assert!(question(&engine, &s, &q).judgment.is_none());
}

#[tokio::test]
async fn knowledge_questions_need_a_rubric_but_open_questions_do_not() {
    let no_rubric = json!({"node_id":"loss","kind":"free_recall","prompt":"Why?"});
    let mut steps = to_quiz(no_rubric);
    steps.push(call(
        "ask_user_question",
        json!({"kind":"free_recall","prompt":"What do you want to build with this?"}),
    ));
    let model = Arc::new(Scripted::new(steps));
    let engine = engine(model.clone());
    let s = engine
        .create_session("Understand packet loss")
        .unwrap()
        .session;
    assert_eq!(
        engine.run_turn(&s).await.unwrap(),
        TurnOutcome::AwaitingLearner
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
    assert!(
        errors[0].contains("a free_recall `quiz` needs a `rubric`"),
        "{errors:?}"
    );

    // An open answer is not judged: the tutor is called directly and told not to give a verdict.
    let q = engine.view(&s).unwrap().active_question.unwrap();
    engine.answer(&s, &q, text("A home network")).unwrap();
    model.push(say("Great, we'll use home networks as examples."));
    engine.run_turn(&s).await.unwrap();
    let last = model.requests.lock().unwrap().last().unwrap().clone();
    assert_eq!(last.tools.len(), 6, "tutor, not judge");
    assert!(
        tool_results(&model, 4)
            .iter()
            .any(|r| r.contains("open answer with no rubric"))
    );
    assert!(question(&engine, &s, &q).judgment.is_none());
}

#[tokio::test]
async fn select_questions_drop_a_stray_rubric_and_are_never_judged() {
    let select = json!({"node_id":"loss","kind":"single","prompt":"Main cause of loss?",
        "options":[{"id":"a","text":"Full queues"},{"id":"b","text":"Cosmic rays"}],
        "correct":["a"],"rubric":"stray"});
    let model = Arc::new(Scripted::new(to_quiz(select)));
    let engine = engine(model.clone());
    let (s, q) = open_quiz(&model, &engine).await;
    assert_eq!(
        engine
            .answer(
                &s,
                &q,
                Answer {
                    choices: vec!["a".into()],
                    ..Answer::default()
                }
            )
            .unwrap()
            .grade,
        Grade::Correct
    );
    model.push(say("Right, queues."));
    engine.run_turn(&s).await.unwrap();
    assert_eq!(model.requests.lock().unwrap().len(), 4, "no judge call");
}
