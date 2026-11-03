use sail_arena::{ArenaSubsystem, MemoryStore, knowledge::*};
use std::sync::Arc;

#[test]
fn no_consent_keeps_bounded_live_context_without_archiving() {
    let arena = ArenaSubsystem::new();
    arena.begin_voice_session("s");
    for turn in 0..40 {
        assert!(
            arena
                .record_turn("s", turn, "private words", "reply")
                .is_none()
        );
    }
    arena.remember_voice_turn("s", 41, "private freeform".into());
    let output = arena.offload_tool_result("s", 42, "test", true, &"private".repeat(2000));
    assert!(!output.contains("\"archived\":"));
    assert_eq!(arena.store().stats().unwrap().records, 0);
    assert!(arena.conversation_context("s", "private").hot.len() <= arena.budget().hot_turns);
    assert!(arena.learning_epoch("s").is_none());
}

#[test]
fn revocation_purges_sources_and_rejects_old_turns_and_proposals() {
    let arena = ArenaSubsystem::new();
    arena.begin_voice_session("s");
    arena.set_session_learning("s", true).unwrap();
    let epoch = arena.learning_epoch("s");
    let id = arena
        .record_turn("s", 1, "Remember: Boat is blue", "ok")
        .unwrap();
    let k = arena.store().knowledge();
    let generation = k.deletion_generation().unwrap();
    let now = i64::MAX / 2;
    k.enqueue_consolidation("voice", &Access::owner(), now, 0)
        .unwrap();
    let job = k.lease_job(now).unwrap().unwrap();
    arena.set_session_learning("s", false).unwrap();
    assert!(arena.store().get(id).unwrap().is_none());
    assert!(k.deletion_generation().unwrap() > generation);
    assert!(matches!(
        k.run_job(&job, &Access::owner(), now),
        Err(KnowledgeError::StaleGeneration { .. })
    ));
    arena.set_session_learning("s", true).unwrap();
    assert!(
        arena
            .record_turn_at_epoch("s", 2, "old words", "ok", epoch)
            .is_none()
    );
    assert!(
        arena
            .record_turn_at_epoch("s", 3, "pre-consent words", "ok", None)
            .is_none()
    );
    arena.release_session("s");
    assert!(
        arena
            .record_turn_at_epoch(
                "s",
                4,
                "disconnected words",
                "ok",
                arena.learning_epoch("s")
            )
            .is_none()
    );
    assert_eq!(arena.store().stats().unwrap().records, 0);
}

#[test]
fn consent_does_not_survive_restart_but_consolidated_knowledge_does() {
    let dir = std::env::temp_dir().join(format!("sail-voice-consent-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("arena.db");
    {
        let arena = ArenaSubsystem::with_store(Arc::new(MemoryStore::open(&path).unwrap()));
        arena.begin_voice_session("s");
        arena.set_session_learning("s", true).unwrap();
        arena.record_turn("s", 1, "I prefer studying in the morning", "ok");
        arena
            .store()
            .knowledge()
            .consolidate("voice", &Access::owner(), 0, 32)
            .unwrap();
        arena.set_session_learning("s", false).unwrap();
        assert!(
            arena
                .knowledge_context("studying morning", &Access::owner(), 1500)
                .is_some()
        );
    }
    {
        let arena = ArenaSubsystem::with_store(Arc::new(MemoryStore::open(&path).unwrap()));
        arena.begin_voice_session("s");
        assert!(arena.learning_epoch("s").is_none());
        assert!(
            arena
                .knowledge_context("studying morning", &Access::owner(), 1500)
                .is_some()
        );
        assert!(arena.set_session_learning("unknown", true).is_err());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn reset_of_a_disconnected_session_erases_pending_archive_and_old_epochs_never_match() {
    let arena = ArenaSubsystem::new();
    arena.begin_voice_session("s");
    arena.set_session_learning("s", true).unwrap();
    let old_epoch = arena.learning_epoch("s");
    let id = arena
        .record_turn("s", 1, "Remember: Boat is blue", "ok")
        .unwrap();
    arena.release_session("s");
    arena.clear_voice_session("s");
    assert!(arena.store().get(id).unwrap().is_none());
    arena.begin_voice_session("s");
    arena.set_session_learning("s", true).unwrap();
    assert_ne!(arena.learning_epoch("s"), old_epoch);
    assert!(
        arena
            .record_turn_at_epoch("s", 2, "old words", "ok", old_epoch)
            .is_none()
    );
}

#[test]
fn revoking_one_session_does_not_strand_another_sessions_queued_sources() {
    let arena = ArenaSubsystem::new();
    for session in ["revoked", "keep"] {
        arena.begin_voice_session(session);
        arena.set_session_learning(session, true).unwrap();
        arena.record_turn(session, 1, &format!("Remember: {session} is blue"), "ok");
    }
    let k = arena.store().knowledge();
    let now = i64::MAX / 2;
    k.enqueue_consolidation("voice", &Access::owner(), now, 0)
        .unwrap();
    let old = k.lease_job(now).unwrap().unwrap();
    arena.set_session_learning("revoked", false).unwrap();
    assert!(k.run_job(&old, &Access::owner(), now).is_err());
    assert!(
        k.enqueue_consolidation("voice", &Access::owner(), now, 0)
            .unwrap()
            .is_some()
    );
    let new = k.lease_job(now).unwrap().unwrap();
    let report = k.run_job(&new, &Access::owner(), now).unwrap();
    assert_eq!(report.claims.len(), 1);
    assert!(
        arena
            .knowledge_context("keep", &Access::owner(), 1500)
            .is_some()
    );
    assert!(
        arena
            .knowledge_context("revoked", &Access::owner(), 1500)
            .is_none()
    );
}
