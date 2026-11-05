//! Message projection repairs and the request window (#239), and view replay (#233).
use sail_arena_learn::core::events::{
    AgentRole, Envelope, LearningEvent, SCHEMA_VERSION, ToolCallRecord,
};
use sail_arena_learn::core::messages::{INTERRUPTED, MAX_MESSAGES, to_llm_messages, window};
use sail_arena_learn::core::view::{SessionView, project_view};
use sail_protocol::{ResourceLimits, Role};

fn log(events: Vec<LearningEvent>) -> Vec<Envelope> {
    events
        .into_iter()
        .enumerate()
        .map(|(i, event)| Envelope {
            session: "s".into(),
            seq: i as u64 + 1,
            at_ms: 0,
            key: format!("k{i}"),
            version: SCHEMA_VERSION,
            event,
        })
        .collect()
}

fn call(id: &str) -> ToolCallRecord {
    ToolCallRecord {
        id: id.into(),
        name: "explain".into(),
        arguments: "{}".into(),
    }
}

#[test]
fn interrupted_calls_get_results_and_orphans_are_dropped() {
    let events = log(vec![
        LearningEvent::SessionCreated { goal: "g".into() },
        LearningEvent::AssistantMessage {
            role: AgentRole::Teacher,
            text: String::new(),
            tool_calls: vec![call("a"), call("b")],
        },
        LearningEvent::ToolResult {
            call_id: "a".into(),
            content: "done".into(),
            ok: true,
        },
        LearningEvent::ToolResult {
            call_id: "zzz".into(),
            content: "orphan".into(),
            ok: true,
        },
        LearningEvent::LearnerMessage {
            text: "hello".into(),
        },
        LearningEvent::AssistantMessage {
            role: AgentRole::Teacher,
            text: String::new(),
            tool_calls: vec![call("c")],
        },
    ]);
    let messages = to_llm_messages("system", &events);
    let tools: Vec<(String, String)> = messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| {
            (
                m.tool_call_id.clone().unwrap(),
                m.content.text().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        tools,
        [
            ("a".into(), "done".into()),
            ("b".into(), INTERRUPTED.into()),
            ("c".into(), INTERRUPTED.into())
        ]
    );
    assert!(!messages.iter().any(|m| m.content.text() == Some("orphan")));
    sail_mind::validate_request(&req(messages), &ResourceLimits::default()).unwrap();
}

fn req(messages: Vec<sail_protocol::Message>) -> sail_protocol::InferenceRequest {
    let mut r = sail_protocol::InferenceRequest::text("x");
    r.messages = messages;
    r
}

#[test]
fn long_history_is_windowed_on_a_user_boundary() {
    let mut events = vec![LearningEvent::SessionCreated { goal: "g".into() }];
    for i in 0..150 {
        events.push(LearningEvent::LearnerMessage {
            text: format!("question {i}"),
        });
        let id = format!("c{i}");
        events.push(LearningEvent::AssistantMessage {
            role: AgentRole::Teacher,
            text: String::new(),
            tool_calls: vec![call(&id)],
        });
        events.push(LearningEvent::ToolResult {
            call_id: id,
            content: "x".repeat(100),
            ok: true,
        });
    }
    let messages = window(to_llm_messages("system", &log(events)));
    assert!(messages.len() <= MAX_MESSAGES);
    assert_eq!(messages[0].role, Role::System);
    assert_eq!(messages[1].content.text(), Some("My learning goal: g"));
    assert_eq!(messages[3].role, Role::User);
    assert_eq!(messages.last().unwrap().role, Role::Tool);
    sail_mind::validate_request(&req(messages), &ResourceLimits::default()).unwrap();
}

#[test]
fn incremental_apply_equals_replay() {
    let events = log(vec![
        LearningEvent::SessionCreated { goal: "g".into() },
        LearningEvent::LearnerMessage { text: "hi".into() },
        LearningEvent::Paused,
        LearningEvent::Resumed,
    ]);
    let mut view = SessionView::empty("s");
    let mut specs = Vec::new();
    for e in &events {
        view.apply(e, &mut specs);
    }
    assert_eq!(view, project_view("s", &events));
    assert_eq!(view.last_seq, 4);
    assert!(view.needs_turn);
}

#[test]
fn view_has_no_mastery_or_score_fields() {
    let events = log(vec![LearningEvent::SessionCreated { goal: "g".into() }]);
    let json = serde_json::to_string(&project_view("s", &events)).unwrap();
    for word in ["mastery", "mastered", "score", "ability"] {
        assert!(!json.contains(word), "view exposes `{word}`: {json}");
    }
}

#[test]
fn compaction_cuts_on_an_input_boundary_and_keeps_tool_pairs() {
    use sail_arena_learn::core::messages::{compact_digest, compaction_cut};
    use sail_arena_learn::core::view::CONTEXT_BUDGET_TOKENS;

    let mut events = vec![LearningEvent::SessionCreated { goal: "g".into() }];
    for i in 0..200 {
        events.push(LearningEvent::LearnerMessage {
            text: format!("question {i} ").repeat(20),
        });
        let id = format!("c{i}");
        events.push(LearningEvent::AssistantMessage {
            role: AgentRole::Teacher,
            text: String::new(),
            tool_calls: vec![call(&id)],
        });
        events.push(LearningEvent::ToolResult {
            call_id: id,
            content: "x".repeat(400),
            ok: true,
        });
    }
    let mut log = log(events);
    let cut = compaction_cut(&log, CONTEXT_BUDGET_TOKENS).expect("history is over budget");
    // The cut sits immediately before an input event, so no call/result pair straddles it.
    let after = log.iter().find(|e| e.seq == cut + 1).unwrap();
    assert!(after.event.is_input(), "cut must precede a learner input");

    let digest = compact_digest(&log, cut);
    assert!(digest.contains("teaching order") || digest.contains("goal"));
    log.push(Envelope {
        session: "s".into(),
        seq: log.len() as u64 + 1,
        at_ms: 0,
        key: "compact".into(),
        version: SCHEMA_VERSION,
        event: LearningEvent::ContextCompacted {
            up_to_seq: cut,
            digest: digest.clone(),
        },
    });
    let messages = to_llm_messages("system", &log);
    assert_eq!(messages[0].role, Role::System);
    assert!(messages[1].content.text().unwrap().contains("digest"));
    // Every tool result in the projection follows a matching assistant call.
    let mut calls = std::collections::HashSet::new();
    for message in &messages {
        for call in &message.tool_calls {
            calls.insert(call.id.clone());
        }
        if let Some(id) = &message.tool_call_id {
            assert!(calls.contains(id), "orphan tool result {id}");
        }
    }
    // A second cut does not re-cut the same history forever.
    assert!(compaction_cut(&log, CONTEXT_BUDGET_TOKENS).is_none());
}

#[test]
fn the_view_exposes_a_context_meter_without_a_cost_estimate() {
    let events = log(vec![
        LearningEvent::SessionCreated { goal: "g".into() },
        LearningEvent::LearnerMessage {
            text: "x".repeat(4_000),
        },
    ]);
    let view = project_view("s", &events);
    assert_eq!(view.context_budget_tokens, 24_000);
    assert!(view.context_fraction > 0.0 && view.context_fraction <= 1.0);
    let json = serde_json::to_string(&view).unwrap();
    assert!(json.contains("context_fraction"));
    assert!(!json.to_lowercase().contains("cost"));
}
