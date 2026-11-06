//! Recorded real local-model Arena Learn journey (#215).
//!
//! This is an opt-in acceptance harness, not a unit test: it needs a live local llama.cpp
//! server and a real model. It drives two related teach sessions, answers one diagnostic
//! question with a misconception, records the teacher's repair, extracts the graph, and
//! starts the second session with those propositions as explicit context.
//!
//! Run against the resident model:
//!
//! ```sh
//! SAIL_LLM_URL=http://127.0.0.1:8081/v1/chat/completions \
//! SAIL_MODEL_PATH=models/Qwen3.6-35B-A3B-Q4_K_M.gguf \
//! SAIL_JOURNEY_OUT=/tmp/arena-learn-journey.json \
//!   cargo test -p sail-dialogue --test real_learn_journey -- --ignored --nocapture
//! ```
//!
//! The output is evidence for one run on a specific machine and model; it is not a
//! benchmark and must not be presented as one.
use sail_arena::MemoryStore;
use sail_arena_learn::agent::{ArenaKnowledge, MindModel};
use sail_arena_learn::core::graph::GraphContextProp;
use sail_arena_learn::core::judgment::Verdict;
use sail_arena_learn::core::question::{Answer, Grade, QuestionKind};
use sail_arena_learn::core::view::{Entry, LearnerQuestion, Mode, SessionView};
use sail_arena_learn::{Engine, SqliteEventStore};
use sail_mind::{LocalModelConfig, MindService};
use sail_protocol::ResourceLimits;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MISCONCEPTION: &str = "Congestion control only matters when the receiver is slow. The sender window is fixed, \
     and a lost packet never changes it.";
const CORRECT_ANSWER: &str = "The congestion window bounds unacknowledged data. Slow start doubles it each RTT; \
     congestion avoidance grows it about one segment per RTT. Loss sets ssthresh to about \
     half the window.";

fn option_score(text: &str) -> i32 {
    let text = text.to_lowercase();
    let good = [
        "retransmit",
        "halves",
        "half of",
        "congestion avoidance",
        "doubles every",
        "very small",
        "sets the cwnd to half",
        "sets ssthresh to half",
        "increases by 1 segment per round-trip",
        "increases by one segment",
        "adjust its",
    ];
    let bad = [
        "forever",
        "immediately closes",
        "increases the sending rate",
        "maximum possible",
        "ignores",
        "remains constant",
        "fixed amount every second",
        "leaves the cwnd unchanged",
        "it doubles the cwnd",
        "sets cwnd to 1 segment (like a timeout)",
        "no effect",
        "never",
    ];
    let mut score = 0;
    for word in good {
        if text.contains(word) {
            score += 3;
        }
    }
    for word in bad {
        if text.contains(word) {
            score -= 4;
        }
    }
    score
}

fn choose(q: &LearnerQuestion, wrong: bool) -> Answer {
    if matches!(q.kind, QuestionKind::Single | QuestionKind::Multiple) {
        let options: Vec<_> = q
            .options
            .iter()
            .filter(|o| {
                let text = o.text.to_lowercase();
                !text.contains("don't know") && !text.contains("not sure")
            })
            .collect();
        let chosen = if wrong {
            options.iter().min_by_key(|o| option_score(&o.text))
        } else {
            options.iter().max_by_key(|o| option_score(&o.text))
        };
        return Answer {
            choices: chosen.map(|o| vec![o.id.clone()]).unwrap_or_default(),
            confidence_pct: Some(65),
            ..Answer::default()
        };
    }
    let prompt = q.prompt.to_lowercase();
    let meta = [
        "experience",
        "already know",
        "background",
        "tell me about your",
        "your knowledge",
    ]
    .iter()
    .any(|w| prompt.contains(w));
    let text = if meta {
        "I know basic TCP and acknowledgements, and I want to understand how congestion \
         control reacts to loss."
    } else if wrong {
        MISCONCEPTION
    } else {
        CORRECT_ANSWER
    };
    Answer {
        text: Some(text.into()),
        confidence_pct: Some(65),
        ..Answer::default()
    }
}

fn active_question(view: &SessionView) -> Option<&LearnerQuestion> {
    let id = view.active_question.as_deref()?;
    view.transcript.iter().find_map(|entry| match entry {
        Entry::Question(q) if q.id == id => Some(q.as_ref()),
        _ => None,
    })
}

fn absorb(
    view: &SessionView,
    counted: &mut HashSet<String>,
    incorrect: &mut usize,
    want_wrong: &mut bool,
) {
    for entry in &view.transcript {
        let Entry::Question(q) = entry else {
            continue;
        };
        if counted.contains(&q.id) || (q.grade.is_none() && q.judgment.is_none()) {
            continue;
        }
        counted.insert(q.id.clone());
        if q.tool.as_deref() == Some("ask_user_question") {
            continue;
        }
        let wrong = q.grade == Some(Grade::Incorrect)
            || q.judgment
                .as_ref()
                .is_some_and(|j| j.verdict != Verdict::Correct);
        if wrong {
            *incorrect += 1;
        }
        *want_wrong = *incorrect == 0;
    }
}

#[derive(Clone, Copy)]
struct Snapshot {
    running: bool,
    question: bool,
    mode: Mode,
    plan_revision: Option<u64>,
    entries: usize,
    answered: usize,
    incorrect: usize,
}

async fn drive(
    engine: &Engine,
    session: &str,
    want_wrong: bool,
    max_questions: usize,
    minutes: u64,
) -> Result<(SessionView, Vec<Snapshot>, usize, usize), String> {
    let started = Instant::now();
    let mut snapshots = Vec::new();
    let mut counted = HashSet::new();
    let mut answered = 0usize;
    let mut incorrect = 0usize;
    let mut want_wrong = want_wrong;
    let mut repair_quiz = false;
    let mut seen_plan = false;
    let mut nudges = 0usize;
    loop {
        let view = engine.view(session).map_err(|e| e.to_string())?;
        absorb(&view, &mut counted, &mut incorrect, &mut want_wrong);
        // Probing is over once a plan exists. Fail the first teaching quiz on purpose so
        // the recorded journey contains a repair, then answer later quizzes correctly.
        if view.plan.is_some() && !seen_plan {
            seen_plan = true;
            want_wrong = false;
            repair_quiz = true;
        }
        let running = engine.is_running(session);
        snapshots.push(Snapshot {
            running,
            question: view.active_question.is_some(),
            mode: view.mode,
            plan_revision: view.plan.as_ref().map(|p| p.revision),
            entries: view.transcript.len(),
            answered,
            incorrect,
        });
        if started.elapsed() > Duration::from_secs(minutes * 60) {
            // Keep the partial record: a slow local model may not finish teaching.
            return Ok((view, snapshots, answered, incorrect));
        }
        if let Some(q) = active_question(&view) {
            if answered >= max_questions {
                return Ok((view, snapshots, answered, incorrect));
            }
            let tool = q.tool.as_deref().unwrap_or_default().to_string();
            let wrong = want_wrong || (repair_quiz && tool == "quiz");
            let answer = choose(q, wrong);
            if repair_quiz && tool == "quiz" {
                repair_quiz = false;
            }
            engine
                .answer(session, &q.id, answer)
                .map_err(|e| format!("answer {}: {e}", q.id))?;
            answered += 1;
            engine
                .run_turn(session)
                .await
                .map_err(|e| format!("turn after answer: {e}"))?;
            continue;
        }
        if running {
            return Err(format!(
                "session {session} is still running without a turn handle"
            ));
        }
        if view.needs_turn {
            engine.run_turn(session).await.map_err(|e| e.to_string())?;
            continue;
        }
        // A small local model may answer in prose instead of calling a tool, or probe past
        // its own 2–4 question guidance. A compliant model never needs this branch.
        if nudges < 3 && view.plan.is_none() && answered >= 3 {
            nudges += 1;
            engine
                .message(
                    session,
                    &format!("journey-probe-{nudges}"),
                    "I have answered enough diagnostic questions. Stop probing now, call \
                     probe_done with the summary and gaps, and hand off to teaching.",
                )
                .map_err(|e| e.to_string())?;
            engine.run_turn(session).await.map_err(|e| e.to_string())?;
            continue;
        }
        return Ok((view, snapshots, answered, incorrect));
    }
}

fn summary(view: &SessionView) -> Value {
    json!({
        "session": view.session,
        "goal": view.goal,
        "kind": view.kind,
        "mode": view.mode,
        "probe": view.probe,
        "plan": view.plan,
        "progress": view.progress,
        "input_tokens": view.input_tokens,
        "output_tokens": view.output_tokens,
        "turns": view.turns.iter().map(|t| json!({
            "turn": t.turn, "role": t.role, "input_tokens": t.input_tokens,
            "output_tokens": t.output_tokens,
        })).collect::<Vec<_>>(),
        "graph_context": view.graph_context,
        "active_question": view.active_question,
        "last_failure": view.last_failure,
        "transcript": view.transcript,
    })
}

fn snapshots(values: &[Snapshot]) -> Vec<Value> {
    values
        .iter()
        .map(|s| {
            json!({
                "running": s.running, "question": s.question, "mode": s.mode,
                "plan_revision": s.plan_revision, "entries": s.entries,
                "answered": s.answered, "incorrect": s.incorrect,
            })
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a live local llama.cpp server and model"]
async fn recorded_real_local_journey() {
    let endpoint = std::env::var("SAIL_LLM_URL")
        .expect("set SAIL_LLM_URL to a loopback /v1/chat/completions endpoint");
    assert!(
        endpoint.starts_with("http://127.0.0.1") || endpoint.starts_with("http://localhost"),
        "the journey is local-only"
    );
    let output = std::env::var("SAIL_JOURNEY_OUT")
        .unwrap_or_else(|_| "/tmp/arena-learn-journey.json".into());
    let mut config = LocalModelConfig::from_env().expect("local model config");
    config.managed = false;
    config.endpoint = endpoint.clone();
    let limits = ResourceLimits {
        // A real CPU-only model needs far longer than the interactive default.
        inference_timeout_ms: 1_800_000,
        max_output_tokens: 1024,
        ..ResourceLimits::default()
    };
    let timeout_ms = limits.inference_timeout_ms;
    let max_output_tokens = limits.max_output_tokens;
    let mind = MindService::new(config.clone(), limits).expect("mind service");
    mind.start().await.expect("model ready");
    let store = Arc::new(SqliteEventStore::open_in_memory().expect("event store"));
    let memory = Arc::new(MemoryStore::open_in_memory().expect("memory store"));
    let knowledge = Arc::new(ArenaKnowledge::new(memory).expect("learning grant"));
    let engine = Engine::new(store, Arc::new(MindModel::new(mind))).with_knowledge(knowledge);

    let first = engine
        .create_session_with_options("Understand TCP congestion control", "teach", Vec::new())
        .expect("create first session");
    let (first_view, first_trace, first_answered, first_incorrect) =
        drive(&engine, &first.session, true, 6, 10)
            .await
            .expect("first session");
    let mut graph = engine.graph_snapshot().expect("graph snapshot");
    if first_view.plan.is_some() {
        engine
            .extract_session_to_graph(&first.session)
            .expect("extract graph");
        graph = engine.graph_snapshot().expect("graph snapshot");
    }
    let context = graph
        .propositions
        .iter()
        .take(3)
        .map(|p| GraphContextProp {
            name: p.name.clone(),
            statement: p.statement.clone(),
        })
        .collect::<Vec<_>>();
    let second = engine
        .create_session_with_options(
            "How does TCP congestion control affect web page latency?",
            "teach",
            context.clone(),
        )
        .expect("create second session");
    let (second_view, second_trace, second_answered, second_incorrect) =
        match drive(&engine, &second.session, false, 4, 5).await {
            Ok(result) => result,
            Err(error) => {
                eprintln!("second session stopped: {error}");
                let view = engine.view(&second.session).expect("second view");
                (view, Vec::new(), 0, 0)
            }
        };

    let result = json!({
        "recorded_at": chrono::Utc::now().to_rfc3339(),
        "endpoint": endpoint,
        "model": config.model_path.file_name().and_then(|n| n.to_str()),
        "limits": {
            "inference_timeout_ms": timeout_ms,
            "max_output_tokens": max_output_tokens,
        },
        "sessions": [summary(&first_view), summary(&second_view)],
        "trace": {"first": snapshots(&first_trace), "second": snapshots(&second_trace)},
        "graph": graph,
        "reuse": {"graph_context": context},
        "counts": {
            "first_answered": first_answered, "first_incorrect": first_incorrect,
            "second_answered": second_answered, "second_incorrect": second_incorrect,
            "graph_propositions": graph.propositions.len(),
            "graph_chains": graph.chains.len(),
        },
    });
    std::fs::write(&output, serde_json::to_vec_pretty(&result).unwrap()).expect("write journey");
    println!("journey written to {output}");
    println!(
        "first plan={} incorrect={} | second plan={} graph_context={} | graph={} props/{} chains",
        first_view.plan.is_some(),
        first_incorrect,
        second_view.plan.is_some(),
        second_view.graph_context.len(),
        graph.propositions.len(),
        graph.chains.len()
    );
    assert!(
        first_view.plan.is_some(),
        "the recorded journey requires a plan from the teacher"
    );
    assert!(
        first_incorrect > 0,
        "the recorded journey requires one observed misconception"
    );
    assert!(
        !graph.propositions.is_empty(),
        "the recorded journey requires extracted graph propositions"
    );
}
