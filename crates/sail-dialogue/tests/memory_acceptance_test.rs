//! Master vision §4.6 memory acceptance test, through the production Dialogue path:
//! conversation turns become compact evidence-attributed knowledge, raw turns are released,
//! a later turn retrieves the current fact, a correction changes what is retrieved,
//! out-of-scope knowledge does not leak, and deletion survives a restart.
use sail_arena::{
    ArenaSubsystem, MemoryStore,
    knowledge::{consolidation::ConsolidationReport, *},
};
use sail_dialogue::server::owner_knowledge_context;
use sail_guardian::memory::memory_grant;
use sail_protocol::memory::MemoryPurpose;
use std::{path::PathBuf, sync::Arc};

struct TempDb(PathBuf);
impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open(path: &std::path::Path) -> (Arc<MemoryStore>, ArenaSubsystem) {
    let store = Arc::new(MemoryStore::open(path).unwrap());
    (store.clone(), ArenaSubsystem::with_store(store))
}

fn consolidate(store: &MemoryStore, scope: &str) -> ConsolidationReport {
    let grant = memory_grant(&MemoryPurpose::Maintenance).unwrap();
    let access = Access::from_grant(&grant);
    let generation = store.knowledge().deletion_generation().unwrap();
    store
        .knowledge()
        .consolidate(scope, &access, generation, 32)
        .unwrap()
}

#[test]
fn conversation_becomes_scoped_correctable_knowledge_that_stays_deleted() {
    let dir =
        TempDb(std::env::temp_dir().join(format!("sail-mem-accept-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&dir.0).unwrap();
    let db = dir.0.join("arena.db");
    let (store, arena) = open(&db);

    // 1. An ordinary turn plus a statement SAIL should remember.
    arena.ensure_test_learning("s1");
    arena.record_turn("s1", 1, "What's the weather like?", "I can't see outside.");
    arena.ensure_test_learning("s1");
    arena.record_turn("s1", 2, "Remember: Thesis deadline is June 30", "Noted.");
    let report = consolidate(&store, "voice");
    assert_eq!(
        report.claims.len(),
        1,
        "only the explicit statement becomes knowledge"
    );
    assert_eq!(report.released, 1, "the consolidated raw turn is released");

    // 2. A later turn retrieves the fact through the Guardian-scoped Dialogue path.
    let ctx = owner_knowledge_context(&arena, "When is my thesis deadline?")
        .expect("relevant knowledge is supplied");
    assert!(ctx.contains("June 30"), "{ctx}");
    assert!(
        ctx.contains("untrusted data"),
        "knowledge is labelled as data"
    );
    assert!(!ctx.contains("weather"), "unrelated turns are not memory");
    assert!(owner_knowledge_context(&arena, "Recommend a film").is_none());

    // 3. Knowledge is not leaked across scopes: Forge sees only its repository's facts.
    let forge = Access::from_grant(
        &memory_grant(&MemoryPurpose::Forge {
            repo: "PedroVictorCoding/SAIL".into(),
        })
        .unwrap(),
    );
    assert!(
        arena
            .knowledge_context("thesis deadline", &forge, 1500)
            .is_none()
    );
    store
        .put(&sail_arena::NewRecord {
            scope: "project:pedrovictorcoding/sail",
            session: "forge",
            kind: "turn",
            turn: None,
            gist: "",
            body: "User: Remember: SAIL CI runner is ubuntu-latest\nSAIL: ok",
            provenance: None,
        })
        .unwrap();
    let owner = Access::owner();
    let generation = store.knowledge().deletion_generation().unwrap();
    store
        .knowledge()
        .consolidate("project:pedrovictorcoding/sail", &owner, generation, 32)
        .unwrap();
    let repo = arena
        .knowledge_context("SAIL CI runner", &forge, 1500)
        .unwrap();
    assert!(repo.context.contains("ubuntu-latest"));
    assert!(
        repo.hits
            .iter()
            .all(|h| h.scope == "project:pedrovictorcoding/sail")
    );
    assert!(
        owner_knowledge_context(&arena, "SAIL CI runner").is_none(),
        "project facts stay out of the personal Dialogue grant"
    );

    // 4. Contradicting new evidence supersedes the old value.
    arena.ensure_test_learning("s2");
    arena.record_turn(
        "s2",
        1,
        "Correction: Thesis deadline is July 15",
        "Updated.",
    );
    consolidate(&store, "voice");
    let ctx = owner_knowledge_context(&arena, "When is my thesis deadline?").unwrap();
    assert!(ctx.contains("July 15"), "{ctx}");
    assert!(
        !ctx.contains("June 30"),
        "superseded value is not offered: {ctx}"
    );

    // 5. Explicit deletion removes it, and it stays deleted after a restart.
    let entity = store
        .knowledge()
        .find_entities("Thesis deadline", None, &owner)
        .unwrap()
        .first()
        .expect("entity exists")
        .id;
    store
        .knowledge()
        .write(Actor::User, |tx| {
            tx.delete_entity(entity, "owner forgot it")
        })
        .unwrap();
    assert!(owner_knowledge_context(&arena, "When is my thesis deadline?").is_none());
    drop(arena);
    drop(store);

    let (store, arena) = open(&db);
    assert!(owner_knowledge_context(&arena, "When is my thesis deadline?").is_none());
    assert!(
        store
            .knowledge()
            .find_entities("Thesis deadline", None, &owner)
            .unwrap()
            .is_empty()
    );
    // Re-running consolidation after restart cannot resurrect it: its sources were released.
    assert!(consolidate(&store, "voice").claims.is_empty());
    assert!(owner_knowledge_context(&arena, "When is my thesis deadline?").is_none());
}

/// Retrieval stays small: only relevant facts, in compact lines, a few at a time, with
/// the rest reachable through `memory_search`; acknowledgements retrieve nothing and short
/// follow-ups borrow the previous turn's words.
#[test]
fn retrieval_is_relevant_compact_and_bounded() {
    let dir = TempDb(std::env::temp_dir().join(format!("sail-mem-lean-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&dir.0).unwrap();
    let (store, arena) = open(&dir.0.join("arena.db"));
    let mut statements = vec![
        "Remember: Thesis deadline is June 30".to_string(),
        "Remember: Garden budget is 200 euros".to_string(),
        "Remember: Alex birthday is May 2".to_string(),
    ];
    for (i, name) in [
        "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel",
    ]
    .iter()
    .enumerate()
    {
        statements.push(format!("Remember: Project {name} codename is C{i}"));
    }
    for (turn, statement) in statements.iter().enumerate() {
        arena.ensure_test_learning("s");
        arena.record_turn("s", turn as u64 + 1, statement, "Noted.");
    }
    assert_eq!(consolidate(&store, "voice").claims.len(), statements.len());

    // Only the relevant fact, as one compact line without ids or scores.
    let ctx = owner_knowledge_context(&arena, "When is my thesis deadline?").unwrap();
    assert!(ctx.contains("| June 30 ["), "{ctx}");
    for unrelated in ["200 euros", "May 2", "codename", "claim_id", "evidence_ids"] {
        assert!(!ctx.contains(unrelated), "{unrelated} leaked into: {ctx}");
    }
    assert!(!ctx.contains(sail_dialogue::vlm::KNOWLEDGE_MORE_NOTE));
    // Common words alone match nothing.
    assert!(owner_knowledge_context(&arena, "What is the time?").is_none());

    // Many matches: a few are shown, and the model is told it can search for the rest.
    let ctx = owner_knowledge_context(&arena, "project codename").unwrap();
    assert_eq!(ctx.matches("codename |").count(), 5, "{ctx}");
    assert!(ctx.contains(sail_dialogue::vlm::KNOWLEDGE_MORE_NOTE));
    let (ok, found) = sail_dialogue::server::owner_knowledge_search(
        &arena,
        &serde_json::json!({"query":"project codename","limit":20}),
    );
    assert!(ok, "{found}");
    assert_eq!(found["facts"].as_array().unwrap().len(), 8, "{found}");
    let (ok, none) = sail_dialogue::server::owner_knowledge_search(
        &arena,
        &serde_json::json!({"query":"submarine"}),
    );
    assert!(ok && none["facts"].as_array().unwrap().is_empty(), "{none}");
    assert!(!sail_dialogue::server::owner_knowledge_search(&arena, &serde_json::json!({})).0);

    // Acknowledgements retrieve nothing; short follow-ups use the previous turn's words.
    arena.ensure_test_learning("chat");
    arena.record_turn(
        "chat",
        1,
        "Tell me about the Garden budget",
        "It is 200 euros.",
    );
    assert_eq!(arena.conversation_context("chat", "ok thanks!").query, None);
    let query = arena
        .conversation_context("chat", "and is it enough?")
        .query
        .expect("follow-up is searched");
    assert!(query.contains("Garden budget"), "{query}");
    let ctx = owner_knowledge_context(&arena, &query).unwrap();
    assert!(ctx.contains("200 euros"), "{ctx}");
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
