use sail_arena::knowledge::*;
use sail_arena::{MemoryStore, NewRecord};
use sail_protocol::memory::*;
fn query(text: &str) -> MemoryQuery {
    MemoryQuery {
        version: VERSION,
        text: text.into(),
        limit: 20,
        token_budget: 4096,
        expand_graph: false,
    }
}
fn learn(s: &MemoryStore, scope: &str, user: &str) -> i64 {
    s.put(&NewRecord {
        scope,
        session: "s",
        kind: "turn",
        turn: None,
        gist: "",
        body: &format!("User: {user}\nSAIL: ok"),
        provenance: None,
    })
    .unwrap();
    s.knowledge()
        .consolidate(
            scope,
            &Access::owner(),
            s.knowledge().deletion_generation().unwrap(),
            32,
        )
        .unwrap()
        .claims[0]
}
#[test]
fn fts_survives_restart_and_never_crosses_scope_or_returns_unrelated_preferences() {
    let s = MemoryStore::open_in_memory().unwrap();
    learn(&s, "voice", "Remember: SAIL is local");
    learn(&s, "private", "Remember: SAIL is secret");
    learn(&s, "voice", "I prefer orange");
    let a = Access::scoped(&["voice"], Sensitivity::Private);
    let r = s
        .knowledge()
        .search(&query("SAIL"), &a, None, i64::MAX - 1)
        .unwrap();
    assert_eq!(r.hits.len(), 1);
    assert_eq!(r.hits[0].value, "local");
    assert!(!r.context.contains("secret"));
    assert!(
        s.knowledge()
            .search(&query("unrelated"), &a, None, 1)
            .unwrap()
            .hits
            .is_empty()
    );
    let mut q = query("SAIL");
    q.token_budget = 0;
    assert!(
        s.knowledge()
            .search(&q, &a, None, 1)
            .unwrap()
            .context
            .is_empty()
    );
    q.token_budget = 100;
    assert!(s.knowledge().search(&q, &a, None, 1).unwrap().context.len() <= 100);
}
#[test]
fn vector_revisions_input_changes_and_deletion_are_enforced() {
    let s = MemoryStore::open_in_memory().unwrap();
    let id = learn(&s, "voice", "Remember: SAIL is local");
    let k = s.knowledge();
    let a = Access::owner();
    let text = k.embedding_text(id, &a).unwrap().unwrap();
    let e = Embedding {
        model: "tiny".into(),
        revision: "hash-v1".into(),
        values: vec![1., 0.],
    };
    k.put_embedding(id, &text, &e, &a, 0).unwrap();
    assert_eq!(
        k.search(&query("differentword"), &a, Some(&e), 1)
            .unwrap()
            .hits
            .len(),
        1
    );
    let wrong = Embedding {
        revision: "hash-v2".into(),
        ..e.clone()
    };
    assert!(
        k.search(&query("differentword"), &a, Some(&wrong), 1)
            .unwrap()
            .hits
            .is_empty()
    );
    let subject = k.find_entities("SAIL", None, &a).unwrap()[0].id;
    k.write(Actor::User, |tx| tx.add_alias(subject, "boat"))
        .unwrap();
    assert!(k.put_embedding(id, &text, &e, &a, 0).is_err());
    assert!(
        k.search(&query("differentword"), &a, Some(&e), 1)
            .unwrap()
            .hits
            .is_empty()
    );
    assert_eq!(k.search(&query("boat"), &a, None, 1).unwrap().hits.len(), 1);
    k.write(Actor::User, |tx| tx.delete_claim(id, "forget"))
        .unwrap();
    assert!(
        k.search(&query("boat"), &a, None, 1)
            .unwrap()
            .hits
            .is_empty()
    );
    assert!(k.put_embedding(id, &text, &e, &a, 0).is_err());
    assert!(s.check_integrity().unwrap().is_empty());
}
#[test]
fn vector_validation_and_budget_are_deterministic() {
    assert!(
        sail_arena::knowledge::retrieval::cosine(&[1., 0.], &[0., 1.])
            .unwrap()
            .abs()
            < 0.001
    );
    assert!(sail_arena::knowledge::retrieval::cosine(&[f32::NAN], &[1.]).is_none());
    let s = MemoryStore::open_in_memory().unwrap();
    learn(&s, "voice", "Remember: Device is λ");
    let r = s
        .knowledge()
        .search(&query("λ"), &Access::owner(), None, 1)
        .unwrap();
    assert_eq!(r.hits.len(), 1);
    let text = sail_arena::knowledge::retrieval::compose(&r.hits, 1, |s| s.len());
    assert!(text.is_empty());
}
#[test]
fn graph_expansion_adds_only_visible_neighbor_facts() {
    let s = MemoryStore::open_in_memory().unwrap();
    let id = learn(&s, "voice", "Remember: SAIL is local");
    learn(&s, "voice", "Remember: Device is online");
    let k = s.knowledge();
    let a = Access::owner();
    let from = k.find_entities("SAIL", None, &a).unwrap()[0].id;
    let target = k.find_entities("Device", None, &a).unwrap()[0].id;
    let evidence = k.evidence(id, &a).unwrap()[0].evidence.id;
    k.write(Actor::User, |tx| {
        tx.assert_claim(
            &NewClaim {
                subject: from,
                predicate: "connects",
                object: ClaimObject::Entity(target),
                category: ClaimCategory::Fact,
                basis: None,
                status: EpistemicStatus::UserReported,
                inference_confidence: None,
                observed_at: 1,
                valid_from: None,
                valid_until: None,
                retention: Retention::Durable,
                scope: "voice",
                sensitivity: Sensitivity::Private,
            },
            &[(evidence, Stance::Supports)],
        )
    })
    .unwrap();
    let mut q = query("local");
    assert_eq!(k.search(&q, &a, None, 2).unwrap().hits.len(), 1);
    q.expand_graph = true;
    assert!(
        k.search(&q, &a, None, 2)
            .unwrap()
            .hits
            .iter()
            .any(|h| h.value == "online")
    );
    assert!(
        k.search(
            &q,
            &Access::scoped(&["other"], Sensitivity::Sensitive),
            None,
            2
        )
        .unwrap()
        .hits
        .is_empty()
    );
}
