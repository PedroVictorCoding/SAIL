//! End-to-end checks of Forge's test and mutation tooling inside the real Bubblewrap sandbox.
//!
//! These run a tiny crate through `run_staged_test_command` (nextest + JUnit) and the
//! mutation gate. They need `cargo-nextest` and `cargo-mutants` on `PATH`. When a tool is
//! missing the test fails if `SAIL_REQUIRE_VERIFICATION_TOOLS=1` (set in CI) and otherwise
//! prints why it did nothing, so a developer machine without the tools still builds.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use sail_forge::mutation::{self, MutationGateMode, MutationOutcome};
use sail_forge::solver::run_staged_test_command;
use sail_forge::test_report::binary_on_path;
use sail_forge::worktree::IsolatedWorktree;
use sail_guardian::sandbox::SandboxPolicy;
use uuid::Uuid;

// The tests change SAIL_FORGE_TEST_TOOL; run them one at a time.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn tools_present(tools: &[&str]) -> bool {
    let missing: Vec<&&str> = tools.iter().filter(|t| !binary_on_path(t)).collect();
    if missing.is_empty() {
        return true;
    }
    if std::env::var("SAIL_REQUIRE_VERIFICATION_TOOLS").as_deref() == Ok("1") {
        panic!("required verification tools missing: {missing:?}");
    }
    eprintln!(
        "skipping: {missing:?} not on PATH (set SAIL_REQUIRE_VERIFICATION_TOOLS=1 to require)"
    );
    false
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A committed one-crate repository with no dependencies, so it builds offline.
fn demo_repo() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("forge_verify_repo_{}", Uuid::new_v4()));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(super::add(2, 3), 5);\n    }\n}\n",
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "target/\nCargo.lock\n").unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "."]);
    git(
        &root,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t.local",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    root
}

fn worktree_for(repo: &Path) -> IsolatedWorktree {
    let job = format!("verify-{}", Uuid::new_v4().simple());
    IsolatedWorktree::create(&job, repo, "HEAD", Arc::new(SandboxPolicy::new())).unwrap()
}

const WITH_DOUBLE_UNTESTED: &str = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn double(x: i32) -> i32 {\n    x * 2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(super::add(2, 3), 5);\n    }\n}\n";

const WITH_DOUBLE_TESTED: &str = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn double(x: i32) -> i32 {\n    x * 2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(super::add(2, 3), 5);\n    }\n\n    #[test]\n    fn doubles() {\n        assert_eq!(super::double(3), 6);\n        assert_eq!(super::double(-4), -8);\n    }\n}\n";

#[test]
fn nextest_failures_reach_the_model_by_name() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !tools_present(&["cargo-nextest"]) {
        return;
    }
    // SAFETY: serialized by ENV_LOCK; no other thread in this binary reads it concurrently.
    unsafe { std::env::set_var("SAIL_FORGE_TEST_TOOL", "nextest") };

    let repo = demo_repo();
    let wt = worktree_for(&repo);
    let logs = wt.root_dir.join("target/forge-logs");

    let (ok, ok_text) = run_staged_test_command(&wt, &logs, 300, 1, None).unwrap();
    assert_eq!(ok.exit_code, 0, "{ok_text}");
    assert!(ok.command.starts_with("cargo nextest run"));
    assert!(
        ok_text.contains("tests: 1 run, 1 passed, 0 failed"),
        "{ok_text}"
    );

    wt.write_file(
        Path::new("src/lib.rs"),
        &WITH_DOUBLE_TESTED.replace(
            "assert_eq!(super::double(3), 6)",
            "assert_eq!(super::double(3), 7)",
        ),
    )
    .unwrap();
    let (bad, bad_text) = run_staged_test_command(&wt, &logs, 300, 2, None).unwrap();
    assert_ne!(bad.exit_code, 0);
    assert!(bad_text.contains("1 failed"), "{bad_text}");
    assert!(
        bad_text.contains("FAILED demo :: tests::doubles"),
        "{bad_text}"
    );
    assert!(bad_text.contains("left: 6"), "{bad_text}");

    unsafe { std::env::remove_var("SAIL_FORGE_TEST_TOOL") };
    let _ = fs::remove_dir_all(&repo);
}

#[test]
fn mutation_gate_tells_tested_from_untested_code() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !tools_present(&["cargo-mutants", "cargo-nextest"]) {
        return;
    }
    unsafe { std::env::set_var("SAIL_FORGE_TEST_TOOL", "nextest") };
    let repo = demo_repo();

    // `double` is added without a test: breaking it fails nothing.
    let wt = worktree_for(&repo);
    wt.write_file(Path::new("src/lib.rs"), WITH_DOUBLE_UNTESTED)
        .unwrap();
    let diff = wt.get_diff().unwrap();
    let outcome = mutation::run(&wt, &diff, 600);
    let MutationOutcome::Completed(counts) = &outcome else {
        panic!("expected a completed run, got {outcome:?}");
    };
    assert!(!counts.missed.is_empty(), "{counts:?}");
    assert!(
        counts.missed.iter().any(|m| m.contains("double")),
        "{counts:?}"
    );
    assert!(mutation::judge(MutationGateMode::Required, &outcome).blocks);
    assert_eq!(
        wt.get_diff().unwrap(),
        diff,
        "worktree must be restored after mutation"
    );

    // With a real test, every mutant of `double` is caught.
    let wt2 = worktree_for(&repo);
    wt2.write_file(Path::new("src/lib.rs"), WITH_DOUBLE_TESTED)
        .unwrap();
    let diff2 = wt2.get_diff().unwrap();
    let outcome2 = mutation::run(&wt2, &diff2, 600);
    let MutationOutcome::Completed(counts2) = &outcome2 else {
        panic!("expected a completed run, got {outcome2:?}");
    };
    assert!(counts2.missed.is_empty(), "{counts2:?}");
    assert!(counts2.caught > 0, "{counts2:?}");
    assert!(!mutation::judge(MutationGateMode::Required, &outcome2).blocks);

    unsafe { std::env::remove_var("SAIL_FORGE_TEST_TOOL") };
    let _ = fs::remove_dir_all(&repo);
}
