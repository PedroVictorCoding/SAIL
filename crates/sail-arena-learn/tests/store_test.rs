use sail_arena_learn::core::events::{LearningEvent, NewEvent};
use sail_arena_learn::{EventStore, SqliteEventStore, StoreError, project_view};

fn temp() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("sail-learn-{}.db", uuid::Uuid::new_v4()))
}

fn message(key: &str, text: &str) -> NewEvent {
    NewEvent::new(key, LearningEvent::LearnerMessage { text: text.into() })
}

#[test]
fn events_survive_restart_and_replay_identically() {
    let path = temp();
    let before = {
        let store = SqliteEventStore::open(&path).unwrap();
        store.create("s1", "Learn TCP", 10).unwrap();
        store
            .append("s1", 1, 11, &[message("a", "hi"), message("b", "more")])
            .unwrap();
        project_view("s1", &store.events("s1").unwrap())
    };
    let store = SqliteEventStore::open(&path).unwrap();
    let events = store.events("s1").unwrap();
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(project_view("s1", &events), before);
    assert_eq!(store.sessions(10).unwrap()[0].goal, "Learn TCP");
}

#[test]
fn duplicate_keys_are_noops_and_stale_writers_conflict() {
    let store = SqliteEventStore::open_in_memory().unwrap();
    store.create("s", "g", 0).unwrap();
    assert_eq!(
        store.append("s", 1, 1, &[message("k", "x")]).unwrap().len(),
        1
    );
    // Same key again (a retried request): nothing new.
    assert!(
        store
            .append("s", 2, 2, &[message("k", "x")])
            .unwrap()
            .is_empty()
    );
    // A writer that saw seq 1 lost the race.
    assert!(matches!(
        store.append("s", 1, 3, &[message("other", "y")]),
        Err(StoreError::Conflict {
            expected: 1,
            found: 2,
            ..
        })
    ));
    assert!(matches!(
        store.append("missing", 0, 0, &[]),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn newer_versions_are_refused_and_delete_removes_events() {
    let path = temp();
    {
        let store = SqliteEventStore::open(&path).unwrap();
        store.create("s", "g", 0).unwrap();
        assert!(store.delete("s").unwrap());
        assert!(matches!(store.events("s"), Err(StoreError::NotFound(_))));
        store.create("t", "g", 0).unwrap();
    }
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute("UPDATE learn_events SET version=99", [])
        .unwrap();
    drop(conn);
    let store = SqliteEventStore::open(&path).unwrap();
    assert!(matches!(
        store.events("t"),
        Err(StoreError::NewerVersion { found: 99, .. })
    ));
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute("UPDATE learn_meta SET value=99 WHERE key='db_version'", [])
        .unwrap();
    drop(conn);
    assert!(matches!(
        SqliteEventStore::open(&path),
        Err(StoreError::NewerVersion { .. })
    ));
    let leftover: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM learn_events WHERE session='s'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0);
}
