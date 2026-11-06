use sail_arena::{ArenaSubsystem, MemoryStore, knowledge::*, memory::MemoryClient};
use sail_common::types::SubsystemId;
use sail_guardian::memory::{MemoryAuthority, MemoryPolicy};
use sail_protocol::memory::MemoryQuery;
use std::sync::Arc;
use uuid::Uuid;
fn query() -> MemoryQuery {
    MemoryQuery {
        version: 1,
        text: "quiet".into(),
        limit: 10,
        token_budget: 4000,
        expand_graph: true,
    }
}
#[test]
fn cross_session_recall_and_discovery_approval_share_one_store_without_leaking() {
    let store = Arc::new(MemoryStore::open_in_memory().unwrap());
    let authority = Arc::new(MemoryAuthority::new());
    for (principal, approved) in [
        (SubsystemId::Dialogue, false),
        (SubsystemId::Discovery, true),
    ] {
        authority
            .configure(
                principal,
                MemoryPolicy {
                    scopes: vec!["voice".into()],
                    max_sensitivity: 1,
                    observe: false,
                    maintain: false,
                    approved_only: approved,
                },
            )
            .unwrap();
    }
    let client = |p| MemoryClient::new(store.clone(), authority.clone(), p, vec!["voice".into()]);
    let arena =
        ArenaSubsystem::with_store(store.clone()).with_voice_memory(client(SubsystemId::Dialogue));
    arena.record_turn("session-one", 1, "I prefer quiet controls", "ok");
    store
        .knowledge()
        .consolidate("voice", &Access::owner(), 0, 10)
        .unwrap();
    arena.release_session("session-one");
    assert!(
        arena
            .long_term_context(Uuid::new_v4(), "quiet")
            .contains("quiet controls")
    );
    let discovery = client(SubsystemId::Discovery);
    assert!(
        discovery
            .search(Uuid::new_v4(), &query(), 1)
            .unwrap()
            .hits
            .is_empty()
    );
    let claim = client(SubsystemId::Dialogue)
        .search(Uuid::new_v4(), &query(), 1)
        .unwrap()
        .hits[0]
        .claim_id;
    store
        .knowledge()
        .write(Actor::User, |tx| tx.approve_insight(claim, true))
        .unwrap();
    assert_eq!(
        discovery
            .search(Uuid::new_v4(), &query(), 1)
            .unwrap()
            .hits
            .len(),
        1
    );
    store
        .knowledge()
        .write(Actor::User, |tx| tx.approve_insight(claim, false))
        .unwrap();
    assert!(
        discovery
            .search(Uuid::new_v4(), &query(), 1)
            .unwrap()
            .hits
            .is_empty()
    );
    store
        .knowledge()
        .write(Actor::User, |tx| tx.approve_insight(claim, true))
        .unwrap();
    store
        .knowledge()
        .write(Actor::User, |tx| tx.delete_claim(claim, "forget"))
        .unwrap();
    assert!(
        discovery
            .search(Uuid::new_v4(), &query(), 1)
            .unwrap()
            .hits
            .is_empty()
    );
    assert!(
        client(SubsystemId::Dialogue)
            .maintenance_access(Uuid::new_v4(), 1)
            .is_err()
    );
    let forged = MemoryClient::new(
        store,
        authority,
        SubsystemId::Dialogue,
        vec!["project:secret".into()],
    );
    assert!(forged.search(Uuid::new_v4(), &query(), 1).is_err());
}
