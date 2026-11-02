use sail_arena::{MemoryStore, NewRecord, knowledge::*};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[test]
fn foreground_inference_blocks_memory_admission_and_idle_tick_catches_up() {
    let store = Arc::new(MemoryStore::open_in_memory().unwrap());
    let id = store
        .put(&NewRecord {
            scope: "voice",
            session: "s",
            kind: "turn",
            turn: None,
            gist: "",
            body: "User: I prefer quiet controls\nSAIL: ok",
            provenance: None,
        })
        .unwrap();
    let now = store.get(id).unwrap().unwrap().created_at_ms + 31_000;
    let idle = Arc::new(AtomicBool::new(false));
    let signal = idle.clone();
    let worker = sail_flow::memory::MemoryMaintenance::new(
        store.clone(),
        Access::scoped(&["voice"], Sensitivity::Private),
        vec!["voice".into()],
        Arc::new(move || signal.load(Ordering::SeqCst)),
        30,
    );
    assert!(!worker.tick(now).unwrap());
    assert!(
        store
            .knowledge()
            .job_summaries(&Access::owner())
            .unwrap()
            .is_empty()
    );
    idle.store(true, Ordering::SeqCst);
    assert!(worker.tick(now).unwrap());
    assert!(store.get(id).unwrap().is_none());
    assert_eq!(
        store.knowledge().job_summaries(&Access::owner()).unwrap()[0].state,
        "done"
    );
}
