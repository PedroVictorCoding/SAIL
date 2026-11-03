//! `code:find_symbol` / `code:find_references` against a real rust-analyzer SCIP index built
//! inside the sandbox (#119).
//!
//! Needs `rust-analyzer` (`rustup component add rust-analyzer`). Without it the test prints
//! why it did nothing, unless `SAIL_REQUIRE_SYMBOL_INDEX=1`, which makes it fail.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use sail_forge::solver::{RepoToolCall, execute_repo_tool};
use sail_forge::worktree::IsolatedWorktree;
use sail_guardian::sandbox::SandboxPolicy;
use uuid::Uuid;

fn rust_analyzer_works() -> bool {
    let ok = Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        if std::env::var("SAIL_REQUIRE_SYMBOL_INDEX").as_deref() == Ok("1") {
            panic!("rust-analyzer is required (SAIL_REQUIRE_SYMBOL_INDEX=1) but not usable");
        }
        eprintln!("skipping: rust-analyzer not usable here");
    }
    ok
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

fn demo_repo() -> PathBuf {
    let root = std::env::temp_dir().join(format!("forge_symbols_{}", Uuid::new_v4()));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"geo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub mod shapes;\n\npub fn total_area(items: &[shapes::Rect]) -> u32 {\n    items.iter().map(shapes::Rect::area).sum()\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("src/shapes.rs"),
        "/// An axis-aligned rectangle.\npub struct Rect {\n    pub w: u32,\n    pub h: u32,\n}\n\nimpl Rect {\n    /// Width times height.\n    pub fn area(&self) -> u32 {\n        self.w * self.h\n    }\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("tests/area_test.rs"),
        "use geo::shapes::Rect;\n\n#[test]\nfn area_multiplies() {\n    assert_eq!(Rect { w: 2, h: 3 }.area(), 6);\n}\n",
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

fn run(wt: &IsolatedWorktree, call: RepoToolCall) -> (bool, String) {
    let logs = wt.root_dir.join("target/forge-logs");
    let r = execute_repo_tool(wt, &call, &logs, 600, 1);
    (r.ok, r.output)
}

#[test]
fn finds_definitions_and_references_and_flags_edited_files() {
    if !rust_analyzer_works() {
        return;
    }
    let repo = demo_repo();
    let job = format!("symbols-{}", Uuid::new_v4().simple());
    let wt = IsolatedWorktree::create(&job, &repo, "HEAD", Arc::new(SandboxPolicy::new())).unwrap();

    let (ok, found) = run(
        &wt,
        RepoToolCall::FindSymbol {
            name: "area".into(),
        },
    );
    assert!(ok, "{found}");
    assert!(found.contains("area — src/shapes.rs:9"), "{found}");
    assert!(found.contains("Width times height."), "{found}");

    let (ok, refs) = run(
        &wt,
        RepoToolCall::FindReferences {
            name: "Rect::area".into(),
        },
    );
    assert!(ok, "{refs}");
    assert!(refs.contains("src/lib.rs: lines 4"), "{refs}");
    assert!(
        refs.contains("tests/area_test.rs [test]: lines 5"),
        "{refs}"
    );

    // An edit after indexing is flagged instead of silently giving old line numbers.
    let shapes = wt.read_file(Path::new("src/shapes.rs")).unwrap();
    wt.write_file(Path::new("src/shapes.rs"), &format!("// moved\n{shapes}"))
        .unwrap();
    let (_, again) = run(
        &wt,
        RepoToolCall::FindSymbol {
            name: "area".into(),
        },
    );
    assert!(again.contains("file changed since indexing"), "{again}");

    let (ok, none) = run(
        &wt,
        RepoToolCall::FindSymbol {
            name: "no_such_thing".into(),
        },
    );
    assert!(ok && none.contains("try repo:search"), "{none}");

    let _ = fs::remove_dir_all(&repo);
}

#[test]
fn index_of_an_unmodified_commit_is_reused_by_the_next_job() {
    if !rust_analyzer_works() {
        return;
    }
    let repo = demo_repo();
    let cache = std::env::temp_dir().join(format!("forge_symbol_cache_{}", Uuid::new_v4()));
    let new_wt = || {
        let job = format!("symcache-{}", Uuid::new_v4().simple());
        IsolatedWorktree::create(&job, &repo, "HEAD", Arc::new(SandboxPolicy::new())).unwrap()
    };

    let first = new_wt();
    let built = sail_forge::symbol_index::cached_or_build(&first, 600, &cache).unwrap();
    let cached_file = cache.join(format!("{}.scip", first.base_commit));
    assert!(
        cached_file.is_file(),
        "index of the base commit was not cached"
    );

    // A second job on the same commit reads the cache: no rust-analyzer run, so no index
    // file appears in its worktree.
    let second = new_wt();
    let reused = sail_forge::symbol_index::cached_or_build(&second, 600, &cache).unwrap();
    assert!(
        !second
            .root_dir
            .join(sail_forge::symbol_index::INDEX_REL_PATH)
            .exists()
    );
    assert_eq!(reused.len(), built.len());
    assert!(!reused.find("area", 1).is_empty());

    // An index built after edits is not cached under the base commit.
    let _ = fs::remove_file(&cached_file);
    let third = new_wt();
    third
        .write_file(
            Path::new("src/lib.rs"),
            "pub mod shapes;\npub fn edited() {}\n",
        )
        .unwrap();
    sail_forge::symbol_index::cached_or_build(&third, 600, &cache).unwrap();
    assert!(
        !cached_file.exists(),
        "an edited tree must not be cached as the base commit"
    );

    let _ = fs::remove_dir_all(&cache);
    let _ = fs::remove_dir_all(&repo);
}
