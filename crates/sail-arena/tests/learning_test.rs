//! Arena Learn's proposition/chain graph (#248): provenance, scope isolation, integrity.
use sail_arena::MemoryStore;
use sail_arena::knowledge::learning::{ChainRelation, NewChain, NewProposition};
use sail_arena::knowledge::model::{Access, Sensitivity};

fn learn() -> Access {
    Access::scoped(&["learning:*"], Sensitivity::Private)
}

fn add(store: &MemoryStore, statement: &str, topic: &str, session: &str, quote: &str) -> i64 {
    store
        .learning()
        .add_proposition(
            &learn(),
            NewProposition {
                statement,
                topic,
                quote,
                session: Some(session),
                source_id: None,
            },
        )
        .unwrap()
        .id
}

#[test]
fn propositions_and_chains_carry_provenance_and_scope_isolation() {
    let store = MemoryStore::open_in_memory().unwrap();
    let a = add(
        &store,
        "Packet loss makes the congestion window shrink.",
        "tcp",
        "s1",
        "Window halves after loss.",
    );
    let b = add(
        &store,
        "A smaller window lowers the sending rate.",
        "tcp",
        "s1",
        "Rate follows the window.",
    );
    let chain = store
        .learning()
        .add_chain(
            &learn(),
            NewChain {
                conclusion: b,
                premises: &[a],
                relation: ChainRelation::Entails,
                session: Some("s1"),
            },
        )
        .unwrap();
    assert_eq!(chain.premises, vec![a]);
    assert_eq!(chain.relation, "entails");

    let props = store
        .learning()
        .propositions(&learn(), Some("tcp"))
        .unwrap();
    assert_eq!(props.len(), 2);
    assert_eq!(props[0].sessions, vec!["s1"]);
    assert!(!props[0].quotes.is_empty());

    // Re-adding the same statement unions provenance instead of duplicating it.
    let again = add(
        &store,
        "  Packet loss makes the congestion window shrink. ",
        "TCP",
        "s2",
        "Window halves.",
    );
    assert_eq!(again, a);
    let prop = store
        .learning()
        .propositions(&learn(), Some("tcp"))
        .unwrap()
        .into_iter()
        .find(|p| p.id == a)
        .unwrap();
    assert_eq!(prop.sessions.len(), 2);

    // A non-Learn grant (Dialogue-like) sees no learning scopes.
    let dialogue = Access::scoped(&["voice", "bus", "user", "life"], Sensitivity::Private);
    assert!(
        store
            .learning()
            .propositions(&dialogue, None)
            .unwrap()
            .is_empty()
    );
    assert!(store.learning().chains(&dialogue, None).unwrap().is_empty());
    // Another topic is isolated.
    assert!(
        store
            .learning()
            .propositions(&learn(), Some("philosophy"))
            .unwrap()
            .is_empty()
    );

    let hits = store
        .learning()
        .search(&learn(), "congestion window", Some("tcp"), 5, 1500)
        .unwrap();
    assert!(
        hits.iter()
            .any(|h| h.statement.contains("congestion window"))
    );
}

#[test]
fn forgetting_a_premise_removes_every_chain_that_needed_it() {
    let store = MemoryStore::open_in_memory().unwrap();
    let a = add(&store, "A premise.", "logic", "s1", "A premise.");
    let b = add(&store, "A conclusion.", "logic", "s1", "A conclusion.");
    store
        .learning()
        .add_chain(
            &learn(),
            NewChain {
                conclusion: b,
                premises: &[a],
                relation: ChainRelation::Entails,
                session: Some("s1"),
            },
        )
        .unwrap();
    assert_eq!(store.learning().chains(&learn(), None).unwrap().len(), 1);
    store.learning().forget(&learn(), a).unwrap();
    assert_eq!(store.learning().chains(&learn(), None).unwrap().len(), 0);
    let generation = store.knowledge().deletion_generation().unwrap();
    assert!(generation >= 1);
}

#[test]
fn deleting_a_source_keeps_claims_another_source_supports() {
    let store = MemoryStore::open_in_memory().unwrap();
    let first = store
        .learning()
        .add_source(&learn(), "notes-a.md", "Window halves after loss.", "tcp")
        .unwrap();
    let second = store
        .learning()
        .add_source(&learn(), "notes-b.md", "Some other notes.", "tcp")
        .unwrap();
    let shared = store
        .learning()
        .add_proposition(
            &learn(),
            NewProposition {
                statement: "Window halves after loss.",
                topic: "tcp",
                quote: "Window halves after loss.",
                session: None,
                source_id: Some(first.id),
            },
        )
        .unwrap();
    store
        .learning()
        .add_proposition(
            &learn(),
            NewProposition {
                statement: "Window halves after loss.",
                topic: "tcp",
                quote: "Window halves after loss.",
                session: None,
                source_id: Some(second.id),
            },
        )
        .unwrap();
    let unique = store
        .learning()
        .add_proposition(
            &learn(),
            NewProposition {
                statement: "Only notes-a says this.",
                topic: "tcp",
                quote: "Only notes-a says this.",
                session: None,
                source_id: Some(first.id),
            },
        )
        .unwrap();

    store.learning().delete_source(&learn(), first.id).unwrap();
    let props = store
        .learning()
        .propositions(&learn(), Some("tcp"))
        .unwrap();
    assert!(
        props.iter().any(|p| p.id == shared.id),
        "shared claim survives"
    );
    assert!(
        !props.iter().any(|p| p.id == unique.id),
        "unique claim is removed"
    );
    assert_eq!(
        store
            .learning()
            .propositions(&learn(), Some("tcp"))
            .unwrap()[0]
            .source_ids,
        vec![second.id]
    );
}
