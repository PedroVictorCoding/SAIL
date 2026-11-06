//! Extraction and gated retrieval (#249, #251) without a live model or database.
mod common;
use common::*;
use sail_arena_learn::agent::extraction::{self, Extraction};
use sail_arena_learn::core::events::{Envelope, LearningEvent, TurnOutcome};
use sail_arena_learn::core::ports::{
    KnowledgeChain, KnowledgeGraph, KnowledgeHit, KnowledgeItem, KnowledgeSource,
};
use sail_arena_learn::{Engine, SqliteEventStore};
use sail_protocol::{FunctionCall, ToolCall};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Fake {
    items: Mutex<Vec<(String, String, String)>>,
    covered: Mutex<i64>,
}

impl KnowledgeGraph for Fake {
    fn search(
        &self,
        _topic: Option<&str>,
        query: &str,
        _limit: usize,
        _budget: usize,
    ) -> Result<Vec<KnowledgeHit>, String> {
        let terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        Ok(self
            .items
            .lock()
            .unwrap()
            .iter()
            .filter(|(statement, _, _)| {
                let text = statement.to_lowercase();
                terms.iter().any(|t| text.contains(t))
            })
            .enumerate()
            .map(|(i, (statement, topic, quote))| KnowledgeHit {
                id: i as i64 + 1,
                statement: statement.clone(),
                topic: topic.clone(),
                quote: quote.clone(),
            })
            .collect())
    }

    fn store_extraction(
        &self,
        topic: &str,
        _session: Option<&str>,
        _source_id: Option<i64>,
        items: &[KnowledgeItem],
        _chains: &[KnowledgeChain],
    ) -> Result<usize, String> {
        let mut store = self.items.lock().unwrap();
        for item in items {
            if !store
                .iter()
                .any(|(statement, _, _)| statement == &item.statement)
            {
                store.push((
                    item.statement.clone(),
                    topic.to_string(),
                    item.quote.clone(),
                ));
            }
        }
        Ok(items.len())
    }

    fn covered_through(&self, _session: &str) -> Result<i64, String> {
        Ok(*self.covered.lock().unwrap())
    }

    fn set_covered_through(&self, _session: &str, seq: i64) -> Result<(), String> {
        *self.covered.lock().unwrap() = seq;
        Ok(())
    }

    fn add_source(
        &self,
        _topic: &str,
        name: &str,
        _content: &str,
    ) -> Result<KnowledgeSource, String> {
        Ok(KnowledgeSource {
            id: 1,
            name: name.into(),
            topic: "tcp".into(),
        })
    }

    fn delete_source(&self, _id: i64) -> Result<(), String> {
        Ok(())
    }

    fn sources(&self, _topic: Option<&str>) -> Result<Vec<KnowledgeSource>, String> {
        Ok(Vec::new())
    }

    fn proposition_count(&self, _topic: Option<&str>) -> Result<usize, String> {
        Ok(self.items.lock().unwrap().len())
    }
}

fn extraction_call(args: serde_json::Value) -> Step {
    let mut m = sail_protocol::Message::text(sail_protocol::Role::Assistant, "");
    m.tool_calls = vec![ToolCall {
        id: "e".into(),
        kind: "function".into(),
        function: FunctionCall {
            name: "extract_propositions".into(),
            arguments: args.to_string(),
        },
    }];
    Step::Reply(m)
}

#[tokio::test]
async fn extraction_requires_verbatim_quotes_and_advances_coverage() {
    let text = "A full queue drops new packets. Dropping packets signals congestion.";
    let model = Arc::new(Scripted::new(vec![
        extraction_call(json!({"propositions":[
            {"statement":"A full queue drops new packets.","quote":"A full queue drops new packets."}
        ]})),
        // Ungrounded on the first try, grounded on the retry.
        extraction_call(
            json!({"propositions":[{"statement":"Invented","quote":"not in the source"}]}),
        ),
        extraction_call(json!({"propositions":[
            {"statement":"Dropping packets signals congestion.","quote":"Dropping packets signals congestion."}
        ]})),
    ]));
    let parsed = extraction::extract(model.as_ref(), text, "prompt", None, &Default::default())
        .await
        .unwrap();
    assert_eq!(parsed.items.len(), 1);
    let fake = Fake::default();
    let stored = extraction::apply(&fake, "tcp", Some("s1"), None, 12, &parsed).unwrap();
    assert_eq!(stored, 1);
    assert_eq!(fake.covered_through("s1").unwrap(), 12);

    let retried = extraction::extract(model.as_ref(), text, "prompt", None, &Default::default())
        .await
        .unwrap();
    assert_eq!(
        retried.items[0].quote,
        "Dropping packets signals congestion."
    );
    extraction::apply(&fake, "tcp", Some("s1"), None, 20, &retried).unwrap();
    assert_eq!(fake.covered_through("s1").unwrap(), 20);
    // Replaying the same batch is deterministic and does not duplicate.
    extraction::apply(&fake, "tcp", Some("s1"), None, 20, &retried).unwrap();
    assert_eq!(fake.items.lock().unwrap().len(), 2);
}

#[test]
fn uncovered_text_keeps_whole_messages_after_the_cut() {
    fn env(seq: u64, event: LearningEvent) -> Envelope {
        Envelope {
            session: "s".into(),
            seq,
            at_ms: 0,
            key: format!("k{seq}"),
            version: sail_arena_learn::core::events::SCHEMA_VERSION,
            event,
        }
    }
    let events = vec![
        env(1, LearningEvent::SessionCreated { goal: "TCP".into() }),
        env(
            2,
            LearningEvent::LearnerMessage {
                text: "first message".into(),
            },
        ),
        env(
            3,
            LearningEvent::AssistantMessage {
                role: sail_arena_learn::core::events::AgentRole::Teacher,
                text: "second message".into(),
                tool_calls: Vec::new(),
            },
        ),
        env(
            4,
            LearningEvent::LearnerMessage {
                text: "third message".into(),
            },
        ),
    ];
    let (text, last) = extraction::uncovered_text(&events, 2);
    assert!(!text.contains("first message"));
    assert!(text.contains("second message") && text.contains("third message"));
    assert_eq!(last, 4);
    assert_eq!(
        extraction::topic_slug("Understand TCP congestion control!"),
        "understand-tcp-congestion-control"
    );
}

#[tokio::test]
async fn the_gated_read_tool_puts_prior_learning_in_the_next_prompt() {
    let fake = Arc::new(Fake::default());
    fake.items.lock().unwrap().push((
        "A smaller window lowers the sending rate.".into(),
        "tcp".into(),
        "Rate follows the window.".into(),
    ));
    let model = Arc::new(Scripted::new(vec![
        call("search_knowledge", json!({"query":"congestion window"})),
        say("Let's build on that."),
    ]));
    let store = Arc::new(SqliteEventStore::open_in_memory().unwrap());
    let engine = Arc::new(Engine::new(store, model.clone()).with_knowledge(fake));
    let s = engine.create_session("Understand TCP").unwrap().session;
    assert_eq!(engine.run_turn(&s).await.unwrap(), TurnOutcome::Completed);
    // The model's second call saw the retrieved proposition as a tool result.
    let second = &model.requests.lock().unwrap()[1];
    assert!(
        second.messages.iter().any(|m| m
            .content
            .text()
            .is_some_and(|t| t.contains("A smaller window lowers the sending rate.")
                && t.contains("untrusted data"))),
        "{:?}",
        second.messages
    );
    let _ = Extraction {
        items: Vec::new(),
        chains: Vec::new(),
    };
}
