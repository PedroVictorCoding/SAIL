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
    // The meter's budget leaves room for the prompt and reply inside the default window.
    assert_eq!(view.context_budget_tokens, 9_216);
    assert!(view.context_fraction > 0.0 && view.context_fraction <= 1.0);
    let json = serde_json::to_string(&view).unwrap();
    assert!(json.contains("context_fraction"));
    assert!(!json.to_lowercase().contains("cost"));
}

/// A long lesson whose latest learner answer replies to a question asked before the cut.
fn compacted_log() -> (Vec<Envelope>, u64) {
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
    events.push(LearningEvent::AssistantMessage {
        role: AgentRole::Teacher,
        text: String::new(),
        tool_calls: vec![ToolCallRecord {
            id: "q".into(),
            name: "recall".into(),
            arguments: "{}".into(),
        }],
    });
    events.push(LearningEvent::QuestionAsked {
        call_id: "q".into(),
        spec: serde_json::from_value(serde_json::json!({
            "kind": "free_recall",
            "prompt": "Why does ice float?",
        }))
        .unwrap(),
        follow_up_of: None,
    });
    let mut log = log(events);
    let push = |log: &mut Vec<Envelope>, event| {
        let seq = log.len() as u64 + 1;
        log.push(Envelope {
            session: "s".into(),
            seq,
            at_ms: 0,
            key: format!("k{seq}"),
            version: SCHEMA_VERSION,
            event,
        });
    };
    push(
        &mut log,
        LearningEvent::Answered {
            call_id: "q".into(),
            answer: serde_json::from_value(serde_json::json!({ "text": "less dense" })).unwrap(),
            grade: sail_arena_learn::core::question::Grade::NeedsJudgment,
        },
    );
    // The turn that compacts already holds the learner's answer.
    let cut = compaction_cut(&log, CONTEXT_BUDGET_TOKENS).expect("history is over budget");
    let digest = compact_digest(&log, cut);
    push(
        &mut log,
        LearningEvent::ContextCompacted {
            up_to_seq: cut,
            digest,
        },
    );
    (log, cut)
}

#[test]
fn compaction_keeps_the_recent_history_after_the_digest() {
    let (log, cut) = compacted_log();
    let messages = to_llm_messages("system", &log);
    assert!(messages[1].content.text().unwrap().contains("digest"));
    // The newest learner message after the cut is still in the prompt, as is the answer.
    let texts: Vec<String> = messages
        .iter()
        .filter_map(|m| m.content.text().map(str::to_string))
        .collect();
    assert!(texts.iter().any(|t| t.starts_with("question 199 ")));
    assert!(
        texts.iter().any(|t| t.contains("less dense")),
        "the learner's latest answer must reach the model"
    );
    // Nothing from before the cut is replayed besides the digest.
    let first_kept = log.iter().find(|e| e.seq == cut + 1).unwrap();
    assert!(first_kept.event.is_input());
    assert!(!texts.iter().any(|t| t.starts_with("question 0 ")));
    assert!(
        messages.len() > 10,
        "kept history was dropped: {}",
        messages.len()
    );
    let mut calls = std::collections::HashSet::new();
    for message in &messages {
        for call in &message.tool_calls {
            calls.insert(call.id.clone());
        }
        if let Some(id) = &message.tool_call_id {
            assert!(calls.contains(id), "orphan tool result {id}");
        }
    }
}

#[test]
fn the_context_meter_falls_after_compaction() {
    use sail_arena_learn::core::messages::compaction_cut;
    use sail_arena_learn::core::view::CONTEXT_BUDGET_TOKENS;

    let (log, _) = compacted_log();
    let before = project_view("s", &log[..log.len() - 1]);
    assert_eq!(before.context_fraction, 1.0);
    let after = project_view("s", &log);
    assert!(
        after.context_fraction < 0.9,
        "meter stuck at {} after compaction",
        after.context_fraction
    );
    assert!(
        after.context_fraction > 0.2,
        "kept history must still count"
    );
    // The kept history plus digest fits, so the next turn does not re-cut at once.
    assert!(compaction_cut(&log, CONTEXT_BUDGET_TOKENS).is_none());
}

#[test]
fn an_answer_to_a_question_asked_before_the_cut_still_reaches_the_model() {
    let (mut log, _) = compacted_log();
    // Re-cut exactly before the answer: the question's call is now behind the digest.
    let answer_seq = log
        .iter()
        .find(|e| matches!(e.event, LearningEvent::Answered { .. }))
        .unwrap()
        .seq;
    if let LearningEvent::ContextCompacted { up_to_seq, .. } = &mut log.last_mut().unwrap().event {
        *up_to_seq = answer_seq - 1;
    }
    let messages = to_llm_messages("system", &log);
    let texts: Vec<String> = messages
        .iter()
        .filter_map(|m| m.content.text().map(str::to_string))
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t.contains("asked before the digest") && t.contains("less dense")),
        "{texts:?}"
    );
    assert!(messages.iter().all(|m| m.tool_call_id.is_none()));
}
