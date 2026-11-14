use sail_arena::knowledge::memory_scan::ScanFinding;
use sail_arena::knowledge::review::{PendingChange, ReviewQueue};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "sail-r22-{label}-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("tempdir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn queue() -> (TempDir, ReviewQueue) {
    let dir = TempDir::new("review");
    let queue = ReviewQueue {
        dir: dir.path().to_path_buf(),
    };
    (dir, queue)
}

fn change(id: &str, proposed_at: i64) -> PendingChange {
    PendingChange {
        id: id.to_string(),
        change: json!({"operations":[{"op":"remember","text":"tea"}]}),
        summary: vec!["Remember: prefers tea".to_string()],
        findings: Vec::new(),
        session: "session-1".to_string(),
        proposed_at,
    }
}

#[test]
fn review_is_off_by_default() {
    let (_dir, queue) = queue();
    assert!(!queue.enabled().expect("enabled"));
    assert!(queue.list().expect("list").is_empty());
}

#[test]
fn enable_survives_a_new_queue() {
    let (dir, queue) = queue();
    queue.set_enabled(true).expect("enable");
    assert!(queue.enabled().expect("enabled"));
    let reopened = ReviewQueue {
        dir: dir.path().to_path_buf(),
    };
    assert!(reopened.enabled().expect("enabled"));
    queue.set_enabled(false).expect("disable");
    assert!(!queue.enabled().expect("enabled"));
}

#[test]
fn list_is_newest_first_and_take_removes() {
    let (_dir, queue) = queue();
    for id in 1..=3 {
        queue.push(change(&format!("{id:032x}"), id)).expect("push");
    }
    let list = queue.list().expect("list");
    assert_eq!(
        list.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
        vec![
            format!("{:032x}", 3),
            format!("{:032x}", 2),
            format!("{:032x}", 1)
        ]
    );
    let taken = queue.take(&format!("{:032x}", 2)).expect("take");
    assert_eq!(taken.proposed_at, 2);
    assert!(queue.take(&format!("{:032x}", 2)).is_err());
    assert_eq!(queue.list().expect("list").len(), 2);
}

#[test]
fn reject_removes_only_the_named_change() {
    let (_dir, queue) = queue();
    queue.push(change(&"a".repeat(32), 1)).expect("push");
    queue.push(change(&"b".repeat(32), 2)).expect("push");
    queue.reject(&"a".repeat(32)).expect("reject");
    assert!(queue.reject(&"a".repeat(32)).is_err());
    let list = queue.list().expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "b".repeat(32));
}

#[test]
fn cap_drops_and_returns_the_oldest() {
    let (_dir, queue) = queue();
    for index in 0..100 {
        assert!(
            queue
                .push(change(&format!("{index:032x}"), index))
                .expect("push")
                .is_none()
        );
    }
    let dropped = queue
        .push(change(&format!("{:032x}", 100), 100))
        .expect("push")
        .expect("dropped");
    assert_eq!(dropped.id, format!("{:032x}", 0));
    let list = queue.list().expect("list");
    assert_eq!(list.len(), 100);
    assert_eq!(list.first().expect("first").id, format!("{:032x}", 100));
    assert_eq!(list.last().expect("last").id, format!("{:032x}", 1));
}

#[test]
fn prune_drops_only_changes_older_than_fourteen_days() {
    let (_dir, queue) = queue();
    let day = 24 * 60 * 60;
    queue.push(change(&"a".repeat(32), 1000)).expect("push");
    assert_eq!(queue.prune(1000 + 14 * day).expect("prune"), 0);
    assert_eq!(queue.prune(1000 + 14 * day + 1).expect("prune"), 1);
    assert!(queue.list().expect("list").is_empty());
}

#[test]
fn findings_round_trip() {
    let (_dir, queue) = queue();
    let mut item = change(&"c".repeat(32), 1);
    item.findings = vec![ScanFinding {
        rule: "leaked_token".to_string(),
        excerpt: "ghp_abcd…".to_string(),
    }];
    queue.push(item.clone()).expect("push");
    assert_eq!(queue.take(&item.id).expect("take"), item);
}

#[test]
fn corrupt_file_is_an_error_and_is_not_replaced() {
    let (dir, queue) = queue();
    let path = dir.path().join("memory-review.json");
    fs::write(&path, b"{broken").expect("write");
    let before = fs::read(&path).expect("read");
    assert!(queue.enabled().is_err());
    assert!(queue.list().is_err());
    assert!(queue.set_enabled(true).is_err());
    assert_eq!(fs::read(&path).expect("read"), before);
}

#[test]
fn wrong_version_is_an_error() {
    let (dir, queue) = queue();
    fs::write(
        dir.path().join("memory-review.json"),
        br#"{"version":2,"enabled":false,"pending":[]}"#,
    )
    .expect("write");
    assert!(queue.enabled().is_err());
}

#[cfg(unix)]
#[test]
fn review_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, queue) = queue();
    queue.push(change(&"d".repeat(32), 1)).expect("push");
    let mode = fs::metadata(dir.path().join("memory-review.json"))
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}
