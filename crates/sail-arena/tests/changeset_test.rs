//! Model-proposed change sets (master vision §4.3): proposals are validated deterministically
//! against the user's own words and commit atomically, or not at all.
use sail_arena::{
    ArenaSubsystem, MemoryStore,
    knowledge::{changeset::*, *},
};
use std::sync::Arc;

const SCOPE: &str = "voice";

fn arena() -> (Arc<MemoryStore>, ArenaSubsystem) {
    let store = Arc::new(MemoryStore::open_in_memory().unwrap());
    (store.clone(), ArenaSubsystem::with_store(store))
}

/// Run the deterministic pass the way background maintenance does, so turns become eligible
/// for the proposer only after it.
fn deterministic_pass(store: &MemoryStore, now: i64) {
    let k = store.knowledge();
    k.enqueue_consolidation(SCOPE, &Access::owner(), now, 0)
        .unwrap();
    while let Some(job) = k.lease_job(now).unwrap() {
        k.run_job(&job, &Access::owner(), now).unwrap();
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        + 1_000
}

fn offered(store: &MemoryStore) -> Vec<SourceTurn> {
    store
        .knowledge()
        .proposal_sources(SCOPE, &Access::owner(), 8)
        .unwrap()
}

fn assert_op(source: i64, quote: &str, subject: &str, predicate: &str, value: &str) -> ChangeOp {
    ChangeOp::Assert {
        source,
        quote: quote.into(),
        subject: subject.into(),
        subject_kind: Some(EntityKind::Person),
        predicate: predicate.into(),
        value: value.into(),
        category: None,
    }
}

fn apply(
    store: &MemoryStore,
    turns: &[SourceTurn],
    ops: Vec<ChangeOp>,
) -> KnowledgeResult<ChangeSetReport> {
    let k = store.knowledge();
    let generation = k.deletion_generation().unwrap();
    k.apply_changeset(
        SCOPE,
        &Access::owner(),
        generation,
        turns,
        &MemoryChangeSet {
            version: CHANGESET_VERSION,
            operations: ops,
        },
    )
}

#[test]
fn grounded_proposal_commits_an_inferred_claim_and_releases_its_turn() {
    let (store, arena) = arena();
    arena.ensure_test_learning("s");
    arena.record_turn("s", 1, "Remember: Boat is blue", "ok");
    arena.ensure_test_learning("s");
    arena.record_turn("s", 2, "My sister Ana lives in Porto now.", "Nice!");
    arena.ensure_test_learning("s");
    arena.record_turn("s", 3, "What time is it?", "Noon.");
    deterministic_pass(&store, now());

    let turns = offered(&store);
    assert_eq!(
        turns.len(),
        2,
        "the grammar turn was already consolidated: {turns:?}"
    );
    assert!(
        turns.iter().all(|t| !t.user.contains("Noon")),
        "assistant text is never offered"
    );
    let ana = turns.iter().find(|t| t.user.contains("Ana")).unwrap().id;

    let report = apply(
        &store,
        &turns,
        vec![assert_op(
            ana,
            "My sister Ana lives in Porto",
            "Ana",
            "lives_in",
            "Porto",
        )],
    )
    .unwrap();
    assert_eq!(report.rejected, None);
    assert_eq!((report.claims.len(), report.released), (1, 1));

    let claim = store
        .knowledge()
        .claim(report.claims[0], &Access::owner())
        .unwrap()
        .unwrap();
    assert_eq!(
        claim.status,
        EpistemicStatus::Inferred,
        "a model reading is an inference"
    );
    let evidence = store
        .knowledge()
        .evidence(claim.id, &Access::owner())
        .unwrap();
    assert_eq!(
        evidence[0].evidence.receipt.as_deref(),
        Some("My sister Ana lives in Porto")
    );
    let hit = arena
        .knowledge_context("Where does Ana live?", &Access::owner(), 1500)
        .unwrap();
    assert!(hit.context.contains("Porto"));

    // Every offered turn is marked: the small-talk turn is not proposed again but stays
    // in the archive until retention expires it.
    assert!(offered(&store).is_empty());
    assert_eq!(store.stats().unwrap().records, 1);
}

#[test]
fn ungrounded_or_assistant_sourced_proposals_commit_nothing() {
    let (store, arena) = arena();
    arena.ensure_test_learning("s");
    arena.record_turn(
        "s",
        1,
        "My sister Ana lives in Porto now.",
        "Your dentist is Dr. Silva.",
    );
    deterministic_pass(&store, now());
    let turns = offered(&store);
    let id = turns[0].id;

    for ops in [
        // Hallucinated value.
        vec![assert_op(
            id,
            "My sister Ana lives in Porto",
            "Ana",
            "lives_in",
            "Lisbon",
        )],
        // Quote taken from the assistant's reply, not the user.
        vec![assert_op(
            id,
            "Your dentist is Dr. Silva",
            "Owner",
            "dentist",
            "Dr. Silva",
        )],
        // One bad operation refuses the whole set, including the grounded one.
        vec![
            assert_op(
                id,
                "My sister Ana lives in Porto",
                "Ana",
                "lives_in",
                "Porto",
            ),
            assert_op(
                id,
                "My sister Ana lives in Porto",
                "Ana",
                "Lives In",
                "Porto",
            ),
        ],
        // Correcting a claim that does not exist.
        vec![ChangeOp::Correct {
            source: id,
            quote: "Porto".into(),
            claim: 999,
            value: "Porto".into(),
        }],
    ] {
        let report = apply(&store, &turns, ops).unwrap();
        assert!(report.rejected.is_some(), "{report:?}");
        assert!(report.claims.is_empty());
        assert!(
            store
                .knowledge()
                .entities(&Access::owner(), 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.stats().unwrap().records,
            1,
            "rejected turns stay for retention"
        );
    }
    assert!(
        offered(&store).is_empty(),
        "a rejected turn is not proposed again"
    );
}

#[test]
fn proposals_can_correct_existing_knowledge_but_not_resurrect_deleted_knowledge() {
    let (store, arena) = arena();
    arena.ensure_test_learning("s");
    arena.record_turn("s", 1, "Remember: Thesis deadline is June 30", "ok");
    deterministic_pass(&store, now());
    let old = store
        .knowledge()
        .find_entities("Thesis deadline", None, &Access::owner())
        .unwrap()[0]
        .id;
    let old_claim = store
        .knowledge()
        .claims_about(old, &Access::owner(), false)
        .unwrap()[0]
        .id;

    arena.ensure_test_learning("s");

    arena.record_turn(
        "s",
        2,
        "Actually the thesis deadline moved to July 15.",
        "Got it.",
    );
    deterministic_pass(&store, now());
    let turns = offered(&store);
    let report = apply(
        &store,
        &turns,
        vec![ChangeOp::Correct {
            source: turns[0].id,
            quote: "the thesis deadline moved to July 15".into(),
            claim: old_claim,
            value: "July 15".into(),
        }],
    )
    .unwrap();
    assert_eq!(report.rejected, None, "{report:?}");
    let previous = store
        .knowledge()
        .claim(old_claim, &Access::owner())
        .unwrap()
        .unwrap();
    assert_eq!(previous.status, EpistemicStatus::Superseded);
    let current = arena
        .knowledge_context("thesis deadline", &Access::owner(), 1500)
        .unwrap();
    assert!(current.context.contains("July 15") && !current.context.contains("June 30"));

    // The owner deletes it; a later proposal restating it is refused, not resurrected.
    store
        .knowledge()
        .write(Actor::User, |tx| {
            tx.delete_claim(report.claims[0], "owner forgot it")
        })
        .unwrap();
    arena.ensure_test_learning("s");
    arena.record_turn(
        "s",
        3,
        "Actually the thesis deadline moved to July 15.",
        "Got it.",
    );
    deterministic_pass(&store, now());
    let turns = offered(&store);
    let again = apply(
        &store,
        &turns,
        vec![ChangeOp::Correct {
            source: turns[0].id,
            quote: "the thesis deadline moved to July 15".into(),
            claim: old_claim,
            value: "July 15".into(),
        }],
    )
    .unwrap();
    assert!(again.rejected.is_some(), "{again:?}");
    assert!(
        arena
            .knowledge_context("thesis deadline July", &Access::owner(), 1500)
            .is_none()
    );
}

#[test]
fn a_deletion_during_proposal_invalidates_it_and_the_turn_is_offered_again() {
    let (store, arena) = arena();
    arena.ensure_test_learning("s");
    arena.record_turn("s", 1, "Remember: Boat is blue", "ok");
    arena.ensure_test_learning("s");
    arena.record_turn("s", 2, "My sister Ana lives in Porto now.", "Nice!");
    deterministic_pass(&store, now());
    let turns = offered(&store);
    let k = store.knowledge();
    let generation = k.deletion_generation().unwrap();

    // While the model was thinking, the owner deleted something.
    let boat = k.find_entities("Boat", None, &Access::owner()).unwrap()[0].id;
    k.write(Actor::User, |tx| tx.delete_entity(boat, "forget"))
        .unwrap();

    let stale = k.apply_changeset(
        SCOPE,
        &Access::owner(),
        generation,
        &turns,
        &MemoryChangeSet {
            version: CHANGESET_VERSION,
            operations: vec![assert_op(
                turns[0].id,
                "Ana lives in Porto",
                "Ana",
                "lives_in",
                "Porto",
            )],
        },
    );
    assert!(
        matches!(stale, Err(KnowledgeError::StaleGeneration { .. })),
        "{stale:?}"
    );
    assert!(k.entities(&Access::owner(), 10).unwrap().is_empty());
    assert_eq!(
        offered(&store),
        turns,
        "unmarked; proposed again from fresh state"
    );
}

#[test]
fn proposals_need_private_access_to_the_source_scope() {
    let (store, arena) = arena();
    arena.ensure_test_learning("s");
    arena.record_turn("s", 1, "My sister Ana lives in Porto now.", "Nice!");
    deterministic_pass(&store, now());
    let narrow = Access::scoped(&["life"], Sensitivity::Sensitive);
    assert!(
        store
            .knowledge()
            .proposal_sources(SCOPE, &narrow, 8)
            .is_err()
    );
    let turns = offered(&store);
    assert!(
        store
            .knowledge()
            .apply_changeset(
                SCOPE,
                &narrow,
                0,
                &turns,
                &MemoryChangeSet {
                    version: 1,
                    operations: vec![]
                }
            )
            .is_err()
    );
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
