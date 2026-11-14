use sail_flow::gateway::outbox::{OutMsg, Outbox};
use std::fs;

fn outbox() -> (tempfile::TempDir, Outbox) {
    let dir = tempfile::tempdir().expect("tempdir");
    let outbox = Outbox {
        dir: dir.path().to_path_buf(),
    };
    (dir, outbox)
}

fn msg(key: &str, created_at: i64) -> OutMsg {
    OutMsg {
        key: key.to_string(),
        chat_id: 1,
        text: format!("reply {key}"),
        reply_to: None,
        buttons: Vec::new(),
        attempts: 0,
        next_attempt_at: created_at,
        created_at,
    }
}

#[test]
fn enqueue_dedups_by_key_and_after_sent() {
    let (_dir, outbox) = outbox();
    assert!(outbox.enqueue(msg("a", 1)).expect("enqueue"));
    assert!(!outbox.enqueue(msg("a", 2)).expect("enqueue"));
    assert!(outbox.enqueue(msg("b", 3)).expect("enqueue"));
    outbox.sent("a", 10).expect("sent");
    assert!(!outbox.enqueue(msg("a", 11)).expect("enqueue"));
    let due = outbox.due(20, 10).expect("due");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].key, "b");
}

#[test]
fn due_is_oldest_first_and_respects_time_and_limit() {
    let (_dir, outbox) = outbox();
    outbox.enqueue(msg("late", 5)).expect("enqueue");
    outbox.enqueue(msg("early", 1)).expect("enqueue");
    outbox.enqueue(msg("future", 100)).expect("enqueue");
    let due = outbox.due(10, 10).expect("due");
    assert_eq!(
        due.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(),
        vec!["early", "late"]
    );
    let limited = outbox.due(10, 1).expect("due");
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].key, "early");
    assert!(outbox.due(0, 10).expect("due").is_empty());
}

#[test]
fn failures_back_off_exactly() {
    let (_dir, outbox) = outbox();
    outbox.enqueue(msg("a", 1000)).expect("enqueue");
    assert!(!outbox.failed("a", 1000).expect("failed"));
    assert!(outbox.due(1004, 10).expect("due").is_empty());
    assert_eq!(outbox.due(1005, 10).expect("due").len(), 1);

    assert!(!outbox.failed("a", 1005).expect("failed"));
    assert_eq!(outbox.due(1005 + 30 - 1, 10).expect("due").len(), 0);
    assert_eq!(outbox.due(1005 + 30, 10).expect("due").len(), 1);

    assert!(!outbox.failed("a", 1035).expect("failed"));
    assert_eq!(outbox.due(1035 + 120, 10).expect("due").len(), 1);

    assert!(!outbox.failed("a", 1155).expect("failed"));
    assert_eq!(outbox.due(1155 + 600, 10).expect("due").len(), 1);

    assert!(!outbox.failed("a", 1755).expect("failed"));
    assert_eq!(outbox.due(1755 + 1800, 10).expect("due").len(), 1);

    assert!(!outbox.failed("a", 3555).expect("failed"));
    assert_eq!(outbox.due(3555 + 1800, 10).expect("due").len(), 1);
}

#[test]
fn drops_after_twenty_attempts() {
    let (_dir, outbox) = outbox();
    outbox.enqueue(msg("a", 1000)).expect("enqueue");
    for attempt in 1..20 {
        assert!(
            !outbox.failed("a", 1000).expect("failed"),
            "attempt {attempt}"
        );
    }
    assert!(outbox.failed("a", 1000).expect("failed"));
    assert!(outbox.due(10_000, 10).expect("due").is_empty());
    assert!(outbox.failed("a", 1000).is_err());
}

#[test]
fn drops_after_a_day() {
    let (_dir, outbox) = outbox();
    outbox.enqueue(msg("a", 0)).expect("enqueue");
    assert!(!outbox.failed("a", 86_399).expect("failed"));
    assert!(outbox.failed("a", 86_400).expect("failed"));
    assert!(outbox.due(100_000, 10).expect("due").is_empty());
}

#[test]
fn offset_survives_a_new_store() {
    let (dir, outbox) = outbox();
    assert_eq!(outbox.offset().expect("offset"), 0);
    outbox.set_offset(42).expect("offset");
    assert_eq!(outbox.offset().expect("offset"), 42);
    let reopened = Outbox {
        dir: dir.path().to_path_buf(),
    };
    assert_eq!(reopened.offset().expect("offset"), 42);
}

#[test]
fn prune_removes_sent_keys_older_than_seven_days() {
    let (_dir, outbox) = outbox();
    outbox.sent("old", 1000).expect("sent");
    outbox.sent("edge", 2000).expect("sent");
    let removed = outbox.prune(2000 + 7 * 24 * 60 * 60).expect("prune");
    assert_eq!(removed, 1);
    let removed = outbox.prune(2000 + 7 * 24 * 60 * 60 + 1).expect("prune");
    assert_eq!(removed, 1);
    assert_eq!(outbox.prune(100_000).expect("prune"), 0);
    assert!(outbox.enqueue(msg("old", 1)).expect("enqueue"));
}

#[test]
fn corrupt_file_is_an_error_and_is_not_replaced() {
    let (dir, outbox) = outbox();
    let path = dir.path().join("outbox.json");
    fs::write(&path, b"broken").expect("write");
    let before = fs::read(&path).expect("read");
    assert!(outbox.due(0, 1).is_err());
    assert!(outbox.enqueue(msg("a", 1)).is_err());
    assert_eq!(fs::read(&path).expect("read"), before);
}

#[cfg(unix)]
#[test]
fn outbox_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, outbox) = outbox();
    outbox.enqueue(msg("a", 1)).expect("enqueue");
    let mode = fs::metadata(dir.path().join("outbox.json"))
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}
