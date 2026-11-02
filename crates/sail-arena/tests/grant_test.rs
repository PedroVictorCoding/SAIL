use sail_arena::{ArenaSubsystem, MemoryStore, knowledge::Access};
use sail_protocol::memory::{MemoryGrant, MemoryPurpose, MemorySensitivity, VERSION};
use std::sync::Arc;

fn grant(scopes: &[&str], max_sensitivity: MemorySensitivity) -> MemoryGrant {
    MemoryGrant {
        version: VERSION,
        purpose: MemoryPurpose::Dialogue,
        scopes: scopes.iter().map(|s| s.to_string()).collect(),
        max_sensitivity,
    }
}

#[test]
fn grants_bound_retrieval_by_scope_and_sensitivity_and_empty_sees_nothing() {
    let store = Arc::new(MemoryStore::open_in_memory().unwrap());
    let arena = ArenaSubsystem::with_store(store.clone());
    arena.record_turn("s", 1, "Remember: Boat name is Sail", "ok");
    store
        .knowledge()
        .consolidate("voice", &Access::owner(), 0, 8)
        .unwrap();

    let visible = Access::from_grant(&grant(&["voice"], MemorySensitivity::Private));
    assert!(
        arena
            .knowledge_context("boat name", &visible, 1500)
            .is_some()
    );
    // Consolidated conversation knowledge is Private; a Normal grant must not read it.
    let normal = Access::from_grant(&grant(&["voice"], MemorySensitivity::Normal));
    assert!(
        arena
            .knowledge_context("boat name", &normal, 1500)
            .is_none()
    );
    let other = Access::from_grant(&grant(&["life"], MemorySensitivity::Sensitive));
    assert!(arena.knowledge_context("boat name", &other, 1500).is_none());
    let empty = Access::from_grant(&grant(&[], MemorySensitivity::Sensitive));
    assert!(arena.knowledge_context("boat name", &empty, 1500).is_none());
    // A budget too small for one complete record yields no partial record.
    assert!(arena.knowledge_context("boat name", &visible, 8).is_none());
}
