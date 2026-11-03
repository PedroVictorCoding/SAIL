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

fn turn(store: &MemoryStore, user: &str) -> (i64, i64) {
    let id = store
        .put(&NewRecord {
            scope: "voice",
            session: "s",
            kind: "turn",
            turn: None,
            gist: "",
            body: &format!("User: {user}\nSAIL: ok"),
            provenance: None,
        })
        .unwrap();
    (id, store.get(id).unwrap().unwrap().created_at_ms + 31_000)
}

#[test]
fn model_proposals_run_only_when_idle_and_commit_only_validated_knowledge() {
    use std::sync::{Mutex, atomic::AtomicUsize};
    let store = Arc::new(MemoryStore::open_in_memory().unwrap());
    let (id, now) = turn(&store, "My sister Ana lives in Porto now.");
    let idle = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let reply: Arc<Mutex<Result<String, String>>> =
        Arc::new(Mutex::new(Err("model offline".into())));
    let prompts: Arc<Mutex<Vec<String>>> = Arc::default();
    let (signal, counter, answer, seen) =
        (idle.clone(), calls.clone(), reply.clone(), prompts.clone());
    let worker = sail_flow::memory::MemoryMaintenance::new(
        store.clone(),
        Access::scoped(&["voice"], Sensitivity::Private),
        vec!["voice".into()],
        Arc::new(move || signal.load(Ordering::SeqCst)),
        30,
    )
    .with_proposer(Arc::new(move |prompt: &str| {
        counter.fetch_add(1, Ordering::SeqCst);
        assert!(
            !prompt.contains("SAIL: ok"),
            "assistant text is never offered"
        );
        seen.lock().unwrap().push(prompt.to_string());
        answer.lock().unwrap().clone()
    }));

    // Busy: no model call at all.
    assert!(!worker.tick(now).unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // Idle but the model is down: the turn stays queued and is retried on the next tick.
    idle.store(true, Ordering::SeqCst);
    assert!(worker.tick(now).unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(prompts.lock().unwrap()[0].contains("Ana lives in Porto"));
    assert!(store.get(id).unwrap().is_some());

    // The model answers with a grounded proposal: it is validated and committed.
    *reply.lock().unwrap() = Ok(format!(
        r#"{{"version":1,"operations":[{{"op":"assert","source":{id},"quote":"My sister Ana lives in Porto","subject":"Ana","subject_kind":"person","predicate":"lives_in","value":"Porto"}}]}}"#
    ));
    assert!(worker.tick(now).unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(
        store.get(id).unwrap().is_none(),
        "consumed turn is released"
    );
    let ana = store
        .knowledge()
        .find_entities("Ana", None, &Access::owner())
        .unwrap();
    let claims = store
        .knowledge()
        .claims_about(ana[0].id, &Access::owner(), false)
        .unwrap();
    assert_eq!(claims[0].status, EpistemicStatus::Inferred);

    // An unusable reply marks the turn as offered; it is never sent to the model again.
    let (other, now) = turn(&store, "Hmm, let me think about dinner.");
    *reply.lock().unwrap() = Ok("I cannot help with that".into());
    assert!(worker.tick(now).unwrap());
    assert!(worker.tick(now).unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let last = prompts.lock().unwrap()[2].clone();
    let turns = last.split("User turns (JSON):").nth(1).unwrap();
    assert!(
        turns.contains("dinner") && !turns.contains("Ana"),
        "only the new turn is offered: {turns}"
    );
    assert!(store.get(other).unwrap().is_some(), "kept until retention");
}
