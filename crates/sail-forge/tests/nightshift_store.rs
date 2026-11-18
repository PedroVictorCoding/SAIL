//! K1: errand store, transitions, persistence, patches and broadcast.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use sail_forge::nightshift::{
    Errand, ErrandId, ErrandSource, ErrandState, ErrandStore, MorningOutcome, NightResult,
    OwnerNote, RanOn, RanOnKind, Size, Transition, WorkOrder,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("sail-nightshift-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn now_at(offset_secs: i64) -> DateTime<Utc> {
    let base: DateTime<Utc> = "2026-11-18T20:00:00Z".parse().expect("fixed time");
    base + chrono::Duration::seconds(offset_secs)
}

fn host() -> RanOn {
    RanOn {
        kind: RanOnKind::Host,
        name: "host".into(),
        model: "local".into(),
    }
}

fn order(title: &str) -> WorkOrder {
    WorkOrder {
        title: title.into(),
        why: "because the owner asked".into(),
        acceptance: vec!["it works".into()],
        target_files: vec!["crates/sail-forge/src/lib.rs".into()],
        size: Size::S,
        notes_for_solver: String::new(),
    }
}

fn result(now: DateTime<Utc>) -> NightResult {
    NightResult {
        job_id: "job-1".into(),
        base_commit: "a".repeat(40),
        ran_on: host(),
        files: Vec::new(),
        tests: None,
        gates: Vec::new(),
        review: None,
        summary: "Changed 0 files. Tests did not run.".into(),
        model_note: None,
        patch_bytes: 0,
        finished_at: now,
    }
}

fn note(text: &str, now: DateTime<Utc>) -> OwnerNote {
    OwnerNote {
        at: now,
        text: text.into(),
    }
}

fn add(store: &ErrandStore, seed: u8) -> Errand {
    store
        .add(
            "Make the tab show dividends",
            ErrandSource::Web,
            now_at(seed as i64),
            [seed; 6],
        )
        .expect("add")
}

fn to_ready(store: &ErrandStore, id: &ErrandId, now: DateTime<Utc>) {
    store
        .transition(id, now, Transition::Proposed(order("Dividends")))
        .expect("propose");
    store
        .transition(id, now, Transition::Approve(None))
        .expect("approve");
    store
        .transition(
            id,
            now,
            Transition::Working {
                job_id: "job-1".into(),
                stage: "coding".into(),
                ran_on: host(),
            },
        )
        .expect("work");
    store
        .transition(
            id,
            now,
            Transition::Finished {
                result: result(now),
                outcome: MorningOutcome::Ready,
                question_or_reason: None,
            },
        )
        .expect("finish");
}

#[test]
fn add_persists_lists_newest_first_and_reopens() {
    let tmp = TempDir::new();
    let store = ErrandStore::open(&tmp.0).expect("open");
    let first = add(&store, 1);
    let second = add(&store, 2);
    assert!(first.id.as_str().starts_with("e-"));
    assert_eq!(first.source, ErrandSource::Web);
    assert_eq!(first.state, ErrandState::Shaping);

    let listed = store.list();
    assert_eq!(
        listed.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        vec![second.id.as_str(), first.id.as_str()]
    );
    assert!(tmp.0.join("nightshift/errands.json").is_file());
    drop(store);

    let reopened = ErrandStore::open(&tmp.0).expect("reopen");
    assert_eq!(reopened.list().len(), 2);
    assert_eq!(reopened.get(&first.id), Some(first));
}

#[test]
fn add_enforces_ask_and_open_limits() {
    let tmp = TempDir::new();
    let store = ErrandStore::open(&tmp.0).expect("open");
    assert!(
        store
            .add("   ", ErrandSource::Voice, now_at(0), [1; 6])
            .is_err()
    );
    let too_long = "a".repeat(2_001);
    assert!(
        store
            .add(&too_long, ErrandSource::Voice, now_at(0), [2; 6])
            .is_err()
    );
    let max = "a".repeat(2_000);
    assert!(
        store
            .add(&max, ErrandSource::Voice, now_at(0), [3; 6])
            .is_ok()
    );

    let mut ids = Vec::new();
    for seed in 10..29u8 {
        let errand = store
            .add("work", ErrandSource::Web, now_at(seed as i64), [seed; 6])
            .expect("under the limit");
        ids.push(errand.id);
    }
    // 1 max-length ask + 19 seeded = 20 open.
    assert_eq!(store.list().len(), 20);
    let rejected = store.add("work", ErrandSource::Web, now_at(40), [99; 6]);
    assert!(
        rejected
            .expect_err("21st is rejected")
            .contains("at most 20")
    );

    store
        .transition(&ids[0], now_at(50), Transition::Drop)
        .expect("drop frees a slot");
    assert!(
        store
            .add("work", ErrandSource::Web, now_at(51), [100; 6])
            .is_ok()
    );
}

#[test]
fn transition_happy_path_tracks_attempts_and_notes() {
    let tmp = TempDir::new();
    let store = ErrandStore::open(&tmp.0).expect("open");
    let errand = add(&store, 1);
    let id = errand.id.clone();

    let proposed = store
        .transition(&id, now_at(1), Transition::Proposed(order("Dividends")))
        .expect("propose");
    assert!(matches!(proposed.state, ErrandState::Proposed { .. }));

    let queued = store
        .transition(&id, now_at(2), Transition::Approve(None))
        .expect("approve");
    assert!(matches!(queued.state, ErrandState::Queued { .. }));
    assert_eq!(queued.attempts, 0);

    let working = store
        .transition(
            &id,
            now_at(3),
            Transition::Working {
                job_id: "job-1".into(),
                stage: "coding".into(),
                ran_on: host(),
            },
        )
        .expect("work");
    assert_eq!(working.attempts, 1);

    let staged = store
        .transition(&id, now_at(4), Transition::Stage("testing".into()))
        .expect("stage");
    match staged.state {
        ErrandState::Working { stage, .. } => assert_eq!(stage, "testing"),
        other => panic!("expected working, got {other:?}"),
    }

    let ready = store
        .transition(
            &id,
            now_at(5),
            Transition::Finished {
                result: result(now_at(5)),
                outcome: MorningOutcome::Ready,
                question_or_reason: None,
            },
        )
        .expect("finish");
    assert!(matches!(ready.state, ErrandState::Ready { .. }));

    let redone = store
        .transition(
            &id,
            now_at(6),
            Transition::Redo(note("try smaller", now_at(6))),
        )
        .expect("redo");
    assert_eq!(redone.notes.len(), 1);
    assert_eq!(redone.notes[0].text, "try smaller");
    assert!(matches!(redone.state, ErrandState::Queued { .. }));

    let working_again = store
        .transition(
            &id,
            now_at(7),
            Transition::Working {
                job_id: "job-2".into(),
                stage: "planning".into(),
                ran_on: host(),
            },
        )
        .expect("work again");
    assert_eq!(working_again.attempts, 2);

    let needs_you = store
        .transition(
            &id,
            now_at(8),
            Transition::Finished {
                result: result(now_at(8)),
                outcome: MorningOutcome::NeedsYou,
                question_or_reason: Some("Which tab?".into()),
            },
        )
        .expect("needs you");
    match &needs_you.state {
        ErrandState::NeedsYou { question, .. } => assert_eq!(question, "Which tab?"),
        other => panic!("expected needs you, got {other:?}"),
    }

    store
        .transition(
            &id,
            now_at(9),
            Transition::Redo(note("ship it after review", now_at(9))),
        )
        .expect("redo after needs-you");
    store
        .transition(
            &id,
            now_at(10),
            Transition::Working {
                job_id: "job-3".into(),
                stage: "reviewing".into(),
                ran_on: host(),
            },
        )
        .expect("work after redo");
    store
        .transition(
            &id,
            now_at(11),
            Transition::Finished {
                result: result(now_at(11)),
                outcome: MorningOutcome::Ready,
                question_or_reason: None,
            },
        )
        .expect("ready after redo");
    let shipping = store
        .transition(&id, now_at(12), Transition::Shipping)
        .expect("ship");
    assert!(matches!(shipping.state, ErrandState::Shipping { .. }));
    let shipped = store
        .transition(
            &id,
            now_at(13),
            Transition::Shipped("https://example.invalid/pr/9".into()),
        )
        .expect("shipped");
    match shipped.state {
        ErrandState::Shipped {
            pr_url, shipped_at, ..
        } => {
            assert_eq!(pr_url, "https://example.invalid/pr/9");
            assert_eq!(shipped_at, now_at(13));
        }
        other => panic!("expected shipped, got {other:?}"),
    }

    let marked = store
        .transition(&id, now_at(14), Transition::MarkExternalUsed)
        .expect_err("shipped errands cannot be marked external");
    assert!(marked.contains("can't mark external while shipped"));
}

#[test]
fn reshape_validates_notes_and_returns_to_shaping() {
    let tmp = TempDir::new();
    let store = ErrandStore::open(&tmp.0).expect("open");
    let errand = add(&store, 1);
    store
        .transition(
            &errand.id,
            now_at(1),
            Transition::Unshapeable(vec!["too vague".into()]),
        )
        .expect("unshapeable");

    let error = store
        .transition(
            &errand.id,
            now_at(2),
            Transition::Reshape(note("  ", now_at(2))),
        )
        .expect_err("blank note");
    assert!(error.contains("note must be"));
    let too_long = "n".repeat(1_001);
    assert!(
        store
            .transition(
                &errand.id,
                now_at(3),
                Transition::Reshape(note(&too_long, now_at(3)))
            )
            .is_err()
    );

    let reshaped = store
        .transition(
            &errand.id,
            now_at(4),
            Transition::Reshape(note("make it smaller", now_at(4))),
        )
        .expect("reshape");
    assert!(matches!(reshaped.state, ErrandState::Shaping));
    assert_eq!(reshaped.notes.len(), 1);
}

#[test]
fn illegal_transitions_name_the_state() {
    let tmp = TempDir::new();
    let store = ErrandStore::open(&tmp.0).expect("open");
    let errand = add(&store, 1);

    let error = store
        .transition(&errand.id, now_at(1), Transition::Approve(None))
        .expect_err("cannot approve while shaping");
    assert_eq!(error, "can't approve while shaping");

    let error = store
        .transition(&errand.id, now_at(2), Transition::Stage("coding".into()))
        .expect_err("cannot stage while shaping");
    assert_eq!(error, "can't change stage while shaping");

    let error = store
        .transition(
            &errand.id,
            now_at(3),
            Transition::Working {
                job_id: "j".into(),
                stage: "coding".into(),
                ran_on: host(),
            },
        )
        .expect_err("cannot work while shaping");
    assert_eq!(error, "can't start working while shaping");

    // A dropped errand cannot be approved.
    store
        .transition(&errand.id, now_at(4), Transition::Drop)
        .expect("drop");
    let error = store
        .transition(&errand.id, now_at(5), Transition::Approve(None))
        .expect_err("cannot approve dropped");
    assert_eq!(error, "can't approve while dropped");
}

#[test]
fn ship_failure_returns_to_ready_and_reports_the_reason() {
    let tmp = TempDir::new();
    let store = ErrandStore::open(&tmp.0).expect("open");
    let errand = add(&store, 1);
    to_ready(&store, &errand.id, now_at(10));
    store
        .transition(&errand.id, now_at(11), Transition::Shipping)
        .expect("shipping");
    let error = store
        .transition(
            &errand.id,
            now_at(12),
            Transition::ShipFailed("git push was rejected".into()),
        )
        .expect_err("reports the failure");
    assert_eq!(error, "git push was rejected");
    assert!(matches!(
        store.get(&errand.id).expect("errand").state,
        ErrandState::Ready { .. }
    ));
}

#[test]
fn restart_recovers_working_to_queued_and_shipping_to_ready() {
    let tmp = TempDir::new();
    let store = ErrandStore::open(&tmp.0).expect("open");
    let working = add(&store, 1);
    store
        .transition(&working.id, now_at(1), Transition::Proposed(order("One")))
        .expect("propose");
    store
        .transition(&working.id, now_at(2), Transition::Approve(None))
        .expect("approve");
    store
        .transition(
            &working.id,
            now_at(3),
            Transition::Working {
                job_id: "job-1".into(),
                stage: "testing".into(),
                ran_on: host(),
            },
        )
        .expect("work");

    let shipping = add(&store, 2);
    to_ready(&store, &shipping.id, now_at(10));
    store
        .transition(&shipping.id, now_at(11), Transition::Shipping)
        .expect("ship");
    drop(store);

    let reopened = ErrandStore::open(&tmp.0).expect("reopen");
    let recovered_working = reopened.get(&working.id).expect("working errand");
    assert_eq!(recovered_working.attempts, 1);
    match recovered_working.state {
        ErrandState::Queued { approved_at, .. } => assert_eq!(approved_at, now_at(3)),
        other => panic!("expected queued, got {other:?}"),
    }
    assert!(matches!(
        reopened.get(&shipping.id).expect("shipping errand").state,
        ErrandState::Ready { .. }
    ));

    // The recovery is persisted: another open sees the same states.
    let twice = ErrandStore::open(&tmp.0).expect("reopen twice");
    assert!(matches!(
        twice.get(&working.id).expect("working errand").state,
        ErrandState::Queued { .. }
    ));
}

#[tokio::test]
async fn every_change_is_broadcast() {
    let tmp = TempDir::new();
    let store = ErrandStore::open(&tmp.0).expect("open");
    // No receiver yet: publishing is still not an error.
    let errand = add(&store, 1);
    let mut events = store.subscribe();

    store
        .transition(&errand.id, now_at(1), Transition::Proposed(order("One")))
        .expect("propose");
    let event = events.recv().await.expect("broadcast");
    assert_eq!(event.id, errand.id);
    assert!(matches!(event.state, ErrandState::Proposed { .. }));

    store
        .transition(&errand.id, now_at(2), Transition::Approve(None))
        .expect("approve");
    let event = events.recv().await.expect("broadcast");
    assert!(matches!(event.state, ErrandState::Queued { .. }));
}

#[test]
fn patches_round_trip_survive_restart_and_enforce_the_limit() {
    let tmp = TempDir::new();
    let store = ErrandStore::open(&tmp.0).expect("open");
    let errand = add(&store, 1);
    assert_eq!(store.patch(&errand.id), None);

    let patch = "diff --git a/x b/x\n+hello\n";
    assert_eq!(store.save_patch(&errand.id, patch), Ok(patch.len() as u64));
    assert_eq!(store.patch(&errand.id).as_deref(), Some(patch));
    drop(store);

    let reopened = ErrandStore::open(&tmp.0).expect("reopen");
    assert_eq!(reopened.patch(&errand.id).as_deref(), Some(patch));

    let max = "p".repeat(512 * 1024);
    assert_eq!(
        reopened.save_patch(&errand.id, &max),
        Ok((512 * 1024) as u64)
    );
    let over = "p".repeat(512 * 1024 + 1);
    assert_eq!(
        reopened.save_patch(&errand.id, &over),
        Err("patch too large to review".into())
    );

    let unknown = ErrandId::new([0xff; 6]);
    assert_eq!(
        reopened.save_patch(&unknown, "patch"),
        Err("no such errand".into())
    );
    assert_eq!(reopened.patch(&unknown), None);
}

#[test]
fn corrupt_state_is_moved_aside_and_the_store_starts_empty() {
    let tmp = TempDir::new();
    let dir = tmp.0.join("nightshift");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("errands.json"), "{not json").expect("write corrupt");

    let store = ErrandStore::open(&tmp.0).expect("open empty");
    assert!(store.list().is_empty());
    let names: Vec<String> = std::fs::read_dir(&dir)
        .expect("dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names
            .iter()
            .any(|name| name.starts_with("errands.json.corrupt-")),
        "corrupt file was not moved aside: {names:?}"
    );
    assert!(!names.iter().any(|name| name == "errands.json"));
}

#[test]
fn invalid_utf8_state_is_quarantined_too() {
    let tmp = TempDir::new();
    let dir = tmp.0.join("nightshift");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("errands.json"), [0xff, 0xfe, 0x00]).expect("write");
    let store = ErrandStore::open(&tmp.0).expect("open empty");
    assert!(store.list().is_empty());
    let names: Vec<String> = std::fs::read_dir(&dir)
        .expect("dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names
            .iter()
            .any(|name| name.starts_with("errands.json.corrupt-"))
    );
}

#[test]
fn mark_external_used_is_persisted() {
    let tmp = TempDir::new();
    let store = ErrandStore::open(&tmp.0).expect("open");
    let errand = add(&store, 1);
    store
        .transition(&errand.id, now_at(1), Transition::Proposed(order("One")))
        .expect("propose");
    store
        .transition(&errand.id, now_at(2), Transition::Approve(None))
        .expect("approve");
    store
        .transition(
            &errand.id,
            now_at(3),
            Transition::Working {
                job_id: "job-1".into(),
                stage: "coding".into(),
                ran_on: host(),
            },
        )
        .expect("work");
    store
        .transition(
            &errand.id,
            now_at(4),
            Transition::Finished {
                result: result(now_at(4)),
                outcome: MorningOutcome::Failed,
                question_or_reason: Some("tests failed".into()),
            },
        )
        .expect("failed");
    let marked = store
        .transition(&errand.id, now_at(5), Transition::MarkExternalUsed)
        .expect("mark");
    assert!(marked.external_used);
    drop(store);

    let reopened = ErrandStore::open(&tmp.0).expect("reopen");
    assert!(reopened.get(&errand.id).expect("errand").external_used);
}

#[test]
fn unknown_ids_are_errors_not_panics() {
    let tmp = TempDir::new();
    let store = ErrandStore::open(&tmp.0).expect("open");
    let unknown = ErrandId::new([7; 6]);
    assert_eq!(
        store.transition(&unknown, now_at(1), Transition::Drop),
        Err("no such errand".into())
    );
    assert_eq!(store.get(&unknown), None);
    assert_eq!(store.patch(&unknown), None);
    assert_eq!(
        store.save_patch(&unknown, "patch"),
        Err("no such errand".into())
    );
}

#[test]
fn open_creates_the_nightshift_layout_and_accepts_a_bare_state_dir() {
    let tmp = TempDir::new();
    let store = ErrandStore::open(&tmp.0).expect("open");
    add(&store, 1);
    assert!(Path::new(&tmp.0.join("nightshift")).is_dir());
    assert!(tmp.0.join("nightshift/patches").is_dir());
    assert!(tmp.0.join("nightshift/errands.json").is_file());
}
