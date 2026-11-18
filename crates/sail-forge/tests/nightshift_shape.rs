//! K2: shaping prompt, JSON parsing, validation and the repair loop.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sail_forge::nightshift::shape::{parse_order, run_shaper, shaper_prompt, validate};
use sail_forge::nightshift::{
    ErrandSource, ErrandState, ErrandStore, OwnerNote, ShaperModel, Size, WorkOrder,
};
use tokio_util::sync::CancellationToken;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("sail-nightshift-shape-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn now() -> DateTime<Utc> {
    "2026-11-18T21:00:00Z".parse().expect("fixed time")
}

fn valid_order() -> WorkOrder {
    WorkOrder {
        title: "Show the next dividend date".into(),
        why: "The owner wants it on the Capital Today tab.".into(),
        acceptance: vec!["The card shows the next dividend date".into()],
        target_files: vec!["crates/demo/src/lib.rs".into()],
        size: Size::S,
        notes_for_solver: "Keep it small.".into(),
    }
}

fn exists(path: &str) -> bool {
    path == "crates/demo/src/lib.rs"
}

struct FakeShaper {
    replies: Mutex<VecDeque<Result<String, String>>>,
    calls: Mutex<Vec<(String, String)>>,
}

impl FakeShaper {
    fn new(replies: Vec<Result<String, String>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().expect("fake lock").clone()
    }
}

#[async_trait]
impl ShaperModel for FakeShaper {
    async fn complete(
        &self,
        system: &str,
        user: &str,
        _cancel: CancellationToken,
    ) -> Result<String, String> {
        self.calls
            .lock()
            .expect("fake lock")
            .push((system.to_string(), user.to_string()));
        self.replies
            .lock()
            .expect("fake lock")
            .pop_front()
            .unwrap_or_else(|| Err("no reply queued".into()))
    }
}

fn repo_root(tmp: &TempDir) -> PathBuf {
    let root = tmp.0.join("repo");
    std::fs::create_dir_all(root.join("crates/demo/src")).expect("repo");
    std::fs::write(
        root.join("crates/demo/Cargo.toml"),
        "[package]\nname = \"demo\"\n",
    )
    .expect("cargo");
    std::fs::write(
        root.join("crates/demo/src/lib.rs"),
        "pub fn dividend() -> u32 { 0 }\n",
    )
    .expect("source");
    root
}

fn add_errand(store: &ErrandStore) -> sail_forge::nightshift::Errand {
    store
        .add(
            "Make the Capital Today tab show the next dividend date.",
            ErrandSource::Web,
            now(),
            [1, 2, 3, 4, 5, 6],
        )
        .expect("add")
}

#[test]
fn shaper_prompt_is_deterministic_and_contains_the_context() {
    let note = OwnerNote {
        at: now(),
        text: "Use the card, not a new tab.".into(),
    };
    let crates = vec!["sail-dialogue".to_string(), "sail-forge".to_string()];
    let (system, user) = shaper_prompt(
        "show the next dividend date",
        std::slice::from_ref(&note),
        "crates/sail-dialogue/web/capital-core.js:\n  1: function render()",
        &crates,
    );
    assert!(system.contains("work order"));
    assert!(system.contains("\"acceptance\""));
    assert!(user.contains("show the next dividend date"));
    assert!(user.contains("Use the card, not a new tab."));
    assert!(user.contains("sail-dialogue, sail-forge"));
    assert!(user.contains("capital-core.js"));
    assert_eq!(
        (system.clone(), user.clone()),
        shaper_prompt(
            "show the next dividend date",
            std::slice::from_ref(&note),
            "crates/sail-dialogue/web/capital-core.js:\n  1: function render()",
            &crates
        )
    );
}

#[test]
fn parse_accepts_plain_fenced_and_prose_wrapped_objects() {
    let order = valid_order();
    let json = serde_json::to_string(&order).expect("serialize");
    assert_eq!(parse_order(&json).expect("plain"), order);
    assert_eq!(
        parse_order(&format!("```json\n{json}\n```")).expect("fenced"),
        order
    );
    assert_eq!(
        parse_order(&format!("Here is the order:\n{json}\nLet me know.")).expect("prose"),
        order
    );

    let mut braces = valid_order();
    braces.why = "A string with {braces} and a quote \" inside.".into();
    let json = serde_json::to_string(&braces).expect("serialize");
    assert_eq!(
        parse_order(&format!("prefix {json} suffix")).expect("braces in string"),
        braces
    );
}

#[test]
fn parse_rejects_non_objects_with_plain_reasons() {
    for reply in ["", "I cannot help with that.", "[]", "{}"] {
        let reasons = parse_order(reply).expect_err("must reject");
        assert!(!reasons.is_empty());
        assert!(reasons.iter().all(|r| !r.trim().is_empty()));
    }
    let huge = "x".repeat(64_001);
    let reasons = parse_order(&huge).expect_err("oversized reply");
    assert!(reasons[0].contains("longer than"));
}

#[test]
fn validate_accepts_a_complete_order() {
    assert!(validate(&valid_order(), &exists).is_empty());
}

#[test]
fn validate_checks_every_field_limit() {
    let mut order = valid_order();
    order.title.clear();
    assert!(
        validate(&order, &exists)
            .iter()
            .any(|r| r.contains("title"))
    );
    order.title = "t".repeat(101);
    assert!(
        validate(&order, &exists)
            .iter()
            .any(|r| r.contains("title"))
    );

    let mut order = valid_order();
    order.why = "w".repeat(601);
    assert!(validate(&order, &exists).iter().any(|r| r.contains("why")));

    let mut order = valid_order();
    order.acceptance.clear();
    assert!(
        validate(&order, &exists)
            .iter()
            .any(|r| r.contains("acceptance checks"))
    );
    order.acceptance = vec!["ok".into(); 9];
    assert!(validate(&order, &exists).iter().any(|r| r.contains("1-8")));
    order.acceptance = vec!["a".repeat(201)];
    assert!(
        validate(&order, &exists)
            .iter()
            .any(|r| r.contains("1-200"))
    );
    order.acceptance = vec!["   ".into()];
    assert!(
        validate(&order, &exists)
            .iter()
            .any(|r| r.contains("1-200"))
    );

    let mut order = valid_order();
    order.notes_for_solver = "n".repeat(1_501);
    assert!(
        validate(&order, &exists)
            .iter()
            .any(|r| r.contains("solver notes"))
    );
}

#[test]
fn validate_rejects_bad_paths_protected_paths_duplicates_and_size_l() {
    let mut order = valid_order();
    order.size = Size::L;
    let reasons = validate(&order, &exists);
    assert!(reasons.contains(&"too big for one night; split it".to_string()));

    let mut order = valid_order();
    order.target_files = vec!["/etc/passwd".into()];
    assert!(
        validate(&order, &exists)
            .iter()
            .any(|r| r.contains("repo-relative"))
    );

    let mut order = valid_order();
    order.target_files = vec!["crates\\demo\\src\\lib.rs".into()];
    assert!(
        validate(&order, &exists)
            .iter()
            .any(|r| r.contains("'/' separators"))
    );

    let mut order = valid_order();
    order.target_files = vec!["crates/../etc/passwd".into()];
    assert!(validate(&order, &exists).iter().any(|r| r.contains("`..`")));

    let mut order = valid_order();
    order.target_files = vec!["crates//demo/src/lib.rs".into()];
    assert!(
        validate(&order, &exists)
            .iter()
            .any(|r| r.contains("empty path segment"))
    );

    let mut order = valid_order();
    order.target_files = vec![".github/workflows/ci.yml".into()];
    assert!(
        validate(&order, &exists)
            .iter()
            .any(|r| r.contains("protected path"))
    );

    let mut order = valid_order();
    order.target_files = vec![
        "crates/demo/src/lib.rs".into(),
        "crates/demo/src/lib.rs".into(),
    ];
    assert!(
        validate(&order, &exists)
            .iter()
            .any(|r| r.contains("more than once"))
    );

    let mut order = valid_order();
    order.target_files = vec!["crates/demo/src/missing.rs".into()];
    assert!(
        validate(&order, &exists)
            .iter()
            .any(|r| r.contains("does not exist"))
    );

    let mut order = valid_order();
    order.target_files = (0..7)
        .map(|i| format!("crates/demo/src/file{i}.rs"))
        .collect();
    assert!(validate(&order, &exists).iter().any(|r| r.contains("1-6")));
}

#[tokio::test]
async fn run_shaper_proposes_a_valid_order() {
    let tmp = TempDir::new();
    let root = repo_root(&tmp);
    let store = ErrandStore::open(&tmp.0.join("state")).expect("store");
    let errand = add_errand(&store);
    let reply = serde_json::to_string(&valid_order()).expect("json");
    let model = FakeShaper::new(vec![Ok(reply)]);

    run_shaper(
        &store,
        &model,
        &root,
        &errand.id,
        now(),
        CancellationToken::new(),
    )
    .await
    .expect("shaped");

    let calls = model.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].1.contains("Make the Capital Today tab"));
    assert!(calls[0].1.contains("crates/demo/src/lib.rs"));
    match store.get(&errand.id).expect("errand").state {
        ErrandState::Proposed { order } => assert_eq!(order, valid_order()),
        other => panic!("expected proposed, got {other:?}"),
    }
}

#[tokio::test]
async fn run_shaper_repairs_once_with_the_reasons() {
    let tmp = TempDir::new();
    let root = repo_root(&tmp);
    let store = ErrandStore::open(&tmp.0.join("state")).expect("store");
    let errand = add_errand(&store);
    let valid = serde_json::to_string(&valid_order()).expect("json");
    let model = FakeShaper::new(vec![Ok("not json".into()), Ok(valid)]);

    run_shaper(
        &store,
        &model,
        &root,
        &errand.id,
        now(),
        CancellationToken::new(),
    )
    .await
    .expect("shaped after repair");

    let calls = model.calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].1.contains("rejected"));
    assert!(calls[1].1.contains("JSON object"));
    assert!(matches!(
        store.get(&errand.id).expect("errand").state,
        ErrandState::Proposed { .. }
    ));
}

#[tokio::test]
async fn run_shaper_marks_unshapeable_after_the_repair_turn() {
    let tmp = TempDir::new();
    let root = repo_root(&tmp);
    let store = ErrandStore::open(&tmp.0.join("state")).expect("store");
    let errand = add_errand(&store);
    let mut broken = valid_order();
    broken.title.clear();
    let broken = serde_json::to_string(&broken).expect("json");
    let model = FakeShaper::new(vec![Ok(broken.clone()), Ok(broken)]);

    run_shaper(
        &store,
        &model,
        &root,
        &errand.id,
        now(),
        CancellationToken::new(),
    )
    .await
    .expect("handled");

    let calls = model.calls();
    assert_eq!(calls.len(), 2);
    match store.get(&errand.id).expect("errand").state {
        ErrandState::Unshapeable { reasons } => {
            assert!(reasons.iter().any(|r| r.contains("title")));
        }
        other => panic!("expected unshapeable, got {other:?}"),
    }
}

#[tokio::test]
async fn model_failure_leaves_the_errand_shaping() {
    let tmp = TempDir::new();
    let root = repo_root(&tmp);
    let store = ErrandStore::open(&tmp.0.join("state")).expect("store");
    let errand = add_errand(&store);
    let model = FakeShaper::new(vec![Err("the model server is down".into())]);

    let error = run_shaper(
        &store,
        &model,
        &root,
        &errand.id,
        now(),
        CancellationToken::new(),
    )
    .await
    .expect_err("infrastructure failure");
    assert!(error.contains("model server is down"));
    assert!(matches!(
        store.get(&errand.id).expect("errand").state,
        ErrandState::Shaping
    ));
}

#[tokio::test]
async fn repair_model_failure_also_leaves_the_errand_shaping() {
    let tmp = TempDir::new();
    let root = repo_root(&tmp);
    let store = ErrandStore::open(&tmp.0.join("state")).expect("store");
    let errand = add_errand(&store);
    let model = FakeShaper::new(vec![Ok("not json".into()), Err("server went away".into())]);

    assert!(
        run_shaper(
            &store,
            &model,
            &root,
            &errand.id,
            now(),
            CancellationToken::new(),
        )
        .await
        .is_err()
    );
    assert!(matches!(
        store.get(&errand.id).expect("errand").state,
        ErrandState::Shaping
    ));
}

#[tokio::test]
async fn run_shaper_rejects_wrong_states_and_pre_cancelled_tokens() {
    let tmp = TempDir::new();
    let root = repo_root(&tmp);
    let store = ErrandStore::open(&tmp.0.join("state")).expect("store");
    let errand = add_errand(&store);
    let model = FakeShaper::new(vec![Ok(
        serde_json::to_string(&valid_order()).expect("json")
    )]);

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        run_shaper(&store, &model, &root, &errand.id, now(), cancelled)
            .await
            .is_err()
    );
    assert!(model.calls().is_empty());

    store
        .transition(&errand.id, now(), sail_forge::nightshift::Transition::Drop)
        .expect("drop");
    assert!(
        run_shaper(
            &store,
            &model,
            &root,
            &errand.id,
            now(),
            CancellationToken::new(),
        )
        .await
        .is_err()
    );
}

#[test]
fn protected_paths_come_from_the_gate_policy() {
    let mut order = valid_order();
    order.target_files = vec!["crates/sail-forge/src/gates.rs".into()];
    let reasons = validate(&order, &exists);
    assert!(reasons.iter().any(|r| r.contains("protected path")));
}

#[test]
fn repo_root_helper_is_a_directory() {
    let tmp = TempDir::new();
    assert!(Path::new(&repo_root(&tmp)).is_dir());
}
