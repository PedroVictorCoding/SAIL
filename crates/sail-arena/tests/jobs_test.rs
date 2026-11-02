use sail_arena::{MemoryStore, NewRecord, knowledge::*};
fn record(s: &MemoryStore, user: &str) -> (i64, i64) {
    let id = s
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
    (id, s.get(id).unwrap().unwrap().created_at_ms + 31_000)
}
#[test]
fn durable_jobs_are_idempotent_leased_and_recover_after_restart() {
    let path = std::env::temp_dir().join(format!("jobs-{}.db", uuid::Uuid::new_v4()));
    let s = MemoryStore::open(&path).unwrap();
    let (source, now) = record(&s, "Remember: SAIL is local");
    let k = s.knowledge();
    let id = k
        .enqueue_consolidation("voice", &Access::owner(), now, 30000)
        .unwrap()
        .unwrap();
    assert!(
        k.enqueue_consolidation("voice", &Access::owner(), now, 30000)
            .unwrap()
            .is_none()
    );
    let first = k.lease_job(now).unwrap().unwrap();
    assert_eq!(first.id, id);
    assert!(k.lease_job(now + 1).unwrap().is_none());
    drop(s);
    let s = MemoryStore::open(&path).unwrap();
    let k = s.knowledge();
    let next = k.lease_job(now + 300001).unwrap().unwrap();
    assert_eq!(next.attempts, 2);
    assert!(k.run_job(&first, &Access::owner(), now + 300001).is_err());
    assert_eq!(
        k.run_job(&next, &Access::owner(), now + 300001)
            .unwrap()
            .released,
        1
    );
    assert!(s.get(source).unwrap().is_none());
    assert_eq!(k.job_summaries(&Access::owner()).unwrap()[0].state, "done");
    drop(s);
    let _ = std::fs::remove_file(path);
}
#[test]
fn reused_archive_ids_cannot_change_the_sources_of_an_existing_job() {
    let s = MemoryStore::open_in_memory().unwrap();
    let (old, now) = record(&s, "Remember: Old is old");
    let k = s.knowledge();
    k.enqueue_consolidation("voice", &Access::owner(), now, 0)
        .unwrap();
    let job = k.lease_job(now).unwrap().unwrap();
    s.delete_session("voice", "s").unwrap();
    let (new, _) = record(&s, "Remember: New is new");
    assert_eq!(old, new);
    assert_eq!(k.run_job(&job, &Access::owner(), now).unwrap().released, 0);
    assert!(s.get(new).unwrap().is_some());
    assert!(
        k.find_entities("New", None, &Access::owner())
            .unwrap()
            .is_empty()
    );
}
#[test]
fn deletion_makes_jobs_obsolete_and_failed_candidates_remain_until_retention() {
    let s = MemoryStore::open_in_memory().unwrap();
    let (_, now) = record(&s, "Remember: A is B");
    let k = s.knowledge();
    k.enqueue_consolidation("voice", &Access::owner(), now, 0)
        .unwrap();
    let job = k.lease_job(now).unwrap().unwrap();
    let entity = k
        .write(Actor::User, |tx| {
            tx.create_entity(&NewEntity {
                kind: EntityKind::Concept,
                name: "X",
                aliases: &[],
                scope: "voice",
                sensitivity: Sensitivity::Private,
            })
        })
        .unwrap();
    k.write(Actor::User, |tx| tx.delete_entity(entity, "forget"))
        .unwrap();
    assert!(matches!(
        k.run_job(&job, &Access::owner(), now),
        Err(KnowledgeError::StaleGeneration { .. })
    ));
    assert_eq!(
        k.job_summaries(&Access::owner()).unwrap()[0].state,
        "obsolete"
    );
    assert_eq!(
        k.expire("voice", &Access::owner(), now + 86_400_000, 60_000)
            .unwrap()
            .archive_records,
        1
    );
    assert!(s.check_integrity().unwrap().is_empty());
}
#[test]
fn pinned_entities_keep_transient_claims_until_unpinned() {
    let s = MemoryStore::open_in_memory().unwrap();
    record(&s, "Remember: A is B");
    let k = s.knowledge();
    let claim = k
        .consolidate("voice", &Access::owner(), 0, 10)
        .unwrap()
        .claims[0];
    let c = k.claim(claim, &Access::owner()).unwrap().unwrap();
    let evidence = k.evidence(claim, &Access::owner()).unwrap()[0].evidence.id;
    let temp = k
        .write(Actor::User, |tx| {
            tx.pin_entity(c.subject, true)?;
            tx.assert_claim(
                &NewClaim {
                    subject: c.subject,
                    predicate: "temporary",
                    object: ClaimObject::Value("expires".into()),
                    category: ClaimCategory::Fact,
                    basis: None,
                    status: EpistemicStatus::UserReported,
                    inference_confidence: None,
                    observed_at: 1,
                    valid_from: None,
                    valid_until: None,
                    retention: Retention::Transient { expires_at: 2 },
                    scope: "voice",
                    sensitivity: Sensitivity::Private,
                },
                &[(evidence, Stance::Supports)],
            )
        })
        .unwrap();
    assert_eq!(
        k.expire("voice", &Access::owner(), 3, 60_000)
            .unwrap()
            .expired_claims,
        0
    );
    k.write(Actor::User, |tx| tx.pin_entity(c.subject, false))
        .unwrap();
    assert_eq!(
        k.expire("voice", &Access::owner(), 3, 60_000)
            .unwrap()
            .expired_claims,
        1
    );
    assert!(k.claim(temp, &Access::owner()).unwrap().is_none());
}
#[test]
fn storage_failure_uses_backoff_without_releasing_sources() {
    let path = std::env::temp_dir().join(format!("job-retry-{}.db", uuid::Uuid::new_v4()));
    let s = MemoryStore::open(&path).unwrap();
    let (source, now) = record(&s, "Remember: A is B");
    let k = s.knowledge();
    k.enqueue_consolidation("voice", &Access::owner(), now, 0)
        .unwrap();
    let job = k.lease_job(now).unwrap().unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_evidence BEFORE INSERT ON kn_evidence BEGIN SELECT RAISE(ABORT,'temporary storage failure'); END;").unwrap();
    assert!(k.run_job(&job, &Access::owner(), now).is_err());
    assert!(s.get(source).unwrap().is_some());
    assert_eq!(
        k.job_summaries(&Access::owner()).unwrap()[0].state,
        "pending"
    );
    assert!(k.lease_job(now + 1).unwrap().is_none());
    conn.execute_batch("DROP TRIGGER fail_evidence").unwrap();
    let retry = k.lease_job(now + 60_001).unwrap().unwrap();
    assert_eq!(
        k.run_job(&retry, &Access::owner(), now + 60_001)
            .unwrap()
            .released,
        1
    );
    drop(conn);
    drop(s);
    let _ = std::fs::remove_file(path);
}
