use sail_arena::{ArenaSubsystem, ContextBudget};
use serde_json::json;

fn reply(i: usize) -> String {
    format!(
        "Turn {i}: here is a detailed answer about scheduling, budgets and the garden. {}",
        "It covers several practical points in plain language without repeating the question. "
            .repeat(9)
    )
}

/// Characters the old Dialogue policy would send: up to 12 history messages within 24k
/// chars, plus up to 3 retrieved records of up to 2000 chars each.
fn old_policy_chars(turns: &[(String, String)]) -> usize {
    let mut history: Vec<usize> = Vec::new();
    for (u, a) in turns {
        history.push(u.len() + 30);
        history.push(a.len() + 35);
        while history.len() > 12 || history.iter().sum::<usize>() > 24_000 {
            history.drain(..2);
        }
    }
    history.iter().sum::<usize>() + 3 * 2000
}

#[test]
fn long_conversation_stays_small_and_recalls_old_facts() {
    let arena = ArenaSubsystem::new().with_budget(ContextBudget::default());
    let mut turns = Vec::new();
    for i in 1..=40 {
        let user = if i == 2 {
            "Remember that my favourite mountain is Serra da Estrela.".to_string()
        } else {
            format!("Question {i} about the weekly plan and the garden budget?")
        };
        let answer = reply(i);
        arena.ensure_test_learning("s1");
        arena.record_turn("s1", i as u64, &user, &answer);
        turns.push((user, answer));
    }
    let ctx = arena.conversation_context("s1", "Which mountain did I say was my favourite?");
    let budget = arena.budget();
    assert_eq!(ctx.hot.len(), budget.hot_turns);
    assert_eq!(ctx.hot.last().unwrap().turn, 40);
    let hot_chars: usize = ctx
        .hot
        .iter()
        .map(|t| t.user.chars().count() + t.assistant.chars().count())
        .sum();
    let memory_chars = ctx.memory.chars();
    assert!(hot_chars <= budget.hot_chars);
    assert!(
        memory_chars <= budget.digest_chars + budget.recall_chars + 200,
        "{memory_chars}"
    );
    assert!(
        ctx.memory
            .recalled
            .iter()
            .any(|r| r.excerpt.contains("Serra da Estrela")),
        "{:#?}",
        ctx.memory.recalled
    );
    // Breadcrumbs are newest-first and point at archived ids.
    assert_eq!(ctx.memory.breadcrumbs[0].turn, 37);
    let old = old_policy_chars(&turns);
    let new = hot_chars + memory_chars;
    eprintln!("context chars after 40 turns: old policy {old}, compacted {new}");
    assert!(new * 2 < old, "old {old}, new {new}");
}

#[test]
fn large_tool_results_are_offloaded_and_recallable_only_in_session() {
    let arena = ArenaSubsystem::new();
    let mut diff = String::new();
    for i in 0..400 {
        diff.push_str(&format!("+ line {i} of an unrelated change\n"));
    }
    diff.push_str("+ fn evict_oldest(&mut self) -> Option<Entry> { self.map.pop_first() }\n");
    for i in 400..800 {
        diff.push_str(&format!("+ line {i} of an unrelated change\n"));
    }
    let content = json!({"tool":"forge_status","success":true,"output":{"diff":diff}}).to_string();
    arena.ensure_test_learning("s1");
    let small = arena.offload_tool_result("s1", 3, "local_time", true, "{\"utc\":\"now\"}");
    assert_eq!(small, "{\"utc\":\"now\"}");

    arena.ensure_test_learning("s1");
    let stub = arena.offload_tool_result("s1", 3, "forge_status", true, &content);
    assert!(
        stub.len() < arena.budget().tool_inline_chars + 600,
        "{}",
        stub.len()
    );
    let stub: serde_json::Value = serde_json::from_str(&stub).unwrap();
    let handle = stub["archived"].as_str().unwrap().to_string();

    let (ok, out) = arena.recall_tool("s1", &json!({"id": handle, "query": "evict_oldest"}));
    assert!(ok, "{out}");
    assert!(out["text"].as_str().unwrap().contains("pop_first"), "{out}");
    assert!(out["text"].as_str().unwrap().chars().count() <= 6_000);

    let (ok, page) = arena.recall_tool("s1", &json!({"id": handle, "offset": 0, "max_chars": 500}));
    assert!(ok);
    assert_eq!(page["next_offset"], 500);

    let (ok, out) = arena.recall_tool("intruder", &json!({"id": handle}));
    assert!(!ok, "other sessions must not read this record: {out}");

    let (ok, found) = arena.recall_tool("s1", &json!({"query": "forge_status evict_oldest"}));
    assert!(ok);
    assert_eq!(found["matches"][0]["id"], handle);
}

#[test]
fn reset_forgets_everything_but_disconnect_keeps_the_archive() {
    let arena = ArenaSubsystem::new();
    arena.ensure_test_learning("keep");
    arena.record_turn("keep", 1, "The boat is called Andorinha", "Noted.");
    arena.ensure_test_learning("wipe");
    arena.record_turn("wipe", 1, "The boat is called Andorinha", "Noted.");

    arena.release_session("keep");
    assert!(arena.conversation_context("keep", "boat").hot.is_empty());
    assert_eq!(
        arena
            .retrieve_voice_context("keep", "boat Andorinha", 3)
            .len(),
        1
    );

    arena.clear_voice_session("wipe");
    assert!(
        arena
            .retrieve_voice_context("wipe", "boat Andorinha", 3)
            .is_empty()
    );
    assert_eq!(arena.store().stats().unwrap().records, 1);
}

#[test]
fn hot_window_spills_oversized_turns_to_breadcrumbs() {
    let budget = ContextBudget {
        hot_chars: 1_000,
        ..ContextBudget::default()
    };
    let arena = ArenaSubsystem::new().with_budget(budget);
    arena.ensure_test_learning("s");
    arena.record_turn("s", 1, "short", "ok");
    arena.ensure_test_learning("s");
    arena.record_turn("s", 2, "long", &"x".repeat(900));
    arena.ensure_test_learning("s");
    arena.record_turn("s", 3, "again", &"y".repeat(900));
    let ctx = arena.conversation_context("s", "anything");
    assert_eq!(ctx.hot.len(), 1, "a single over-budget turn is still kept");
    assert_eq!(ctx.hot[0].turn, 3);
    assert_eq!(
        ctx.memory
            .breadcrumbs
            .iter()
            .map(|b| b.turn)
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert_eq!(arena.last_user_prompt("s").as_deref(), Some("again"));
}

trait TestLearning {
    fn ensure_test_learning(&self, session: &str);
}
impl TestLearning for sail_arena::ArenaSubsystem {
    fn ensure_test_learning(&self, session: &str) {
        if self.learning_epoch(session).is_none() {
            self.begin_voice_session(session);
            self.set_session_learning(session, true).unwrap();
        }
    }
}

#[test]
fn every_completed_reply_compacts_before_the_next_query() {
    let arena = ArenaSubsystem::new().with_budget(ContextBudget {
        hot_turns: 1,
        ..ContextBudget::default()
    });
    arena.ensure_test_learning("cycle");
    let consent = arena.learning_epoch("cycle");
    let first = arena
        .complete_response_context_at_epoch(
            "cycle",
            1,
            "The boat is Andorinha.",
            "I will remember the boat name.",
            consent,
        )
        .unwrap();
    assert_eq!(
        (
            first.hot_turns_before,
            first.hot_turns_after,
            first.evicted_turns
        ),
        (1, 1, 0)
    );
    let second = arena
        .complete_response_context_at_epoch(
            "cycle",
            2,
            "What is tomorrow's plan?",
            "Check the weather before sailing.",
            consent,
        )
        .unwrap();
    assert_eq!(
        (
            second.hot_turns_before,
            second.hot_turns_after,
            second.evicted_turns
        ),
        (2, 1, 1)
    );
    assert!(second.hot_chars_after < second.hot_chars_before);
    let next = arena.conversation_context("cycle", "What is the boat called?");
    assert_eq!(next.hot[0].turn, 2);
    assert!(
        next.memory
            .recalled
            .iter()
            .any(|r| r.excerpt.contains("Andorinha"))
    );
    // Reading context is not the trigger, and does not change the retained hot turn.
    assert_eq!(
        arena.conversation_context("cycle", "anything").hot[0].turn,
        2
    );
}

#[test]
fn post_reply_compaction_never_grants_learning_consent() {
    let arena = ArenaSubsystem::new().with_budget(ContextBudget {
        hot_turns: 1,
        ..ContextBudget::default()
    });
    arena.begin_voice_session("private");
    for turn in 1..=4 {
        let result = arena
            .complete_response_context_at_epoch(
                "private",
                turn,
                "private prompt",
                "private completed reply",
                None,
            )
            .unwrap();
        assert_eq!(result.record_id, None);
        assert_eq!(result.hot_turns_after, 1);
    }
    assert_eq!(arena.store().stats().unwrap().records, 0);
    assert_eq!(
        arena.last_user_prompt("private").as_deref(),
        Some("private prompt")
    );
    assert!(
        arena
            .complete_response_context_at_epoch("missing", 1, "query", "reply", None)
            .is_none()
    );
}
