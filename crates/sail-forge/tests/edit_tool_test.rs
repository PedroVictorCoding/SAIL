use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use uuid::Uuid;

use sail_forge::solver::{RepoToolCall, execute_repo_tool, parse_model_action};
use sail_forge::worktree::IsolatedWorktree;
use sail_guardian::sandbox::SandboxPolicy;

fn worktree() -> (IsolatedWorktree, std::path::PathBuf) {
    let repo = std::env::temp_dir().join(format!("edit_tool_{}", Uuid::new_v4()));
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn a() -> i32 {\n    1\n}\n\npub fn b() -> i32 {\n    2\n}\n",
    )
    .unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "."],
        &[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
    }
    let wt = IsolatedWorktree::create(
        &format!("edit-{}", Uuid::new_v4()),
        &repo,
        "HEAD",
        Arc::new(SandboxPolicy::new()),
    )
    .unwrap();
    (wt, repo)
}

fn run(wt: &IsolatedWorktree, call: &RepoToolCall) -> sail_forge::solver::RepoToolResult {
    let logs = std::env::temp_dir().join(format!("edit_logs_{}", Uuid::new_v4()));
    execute_repo_tool(wt, call, &logs, 60, 1)
}

#[test]
fn search_replace_reply_parses_and_applies() {
    let (wt, repo) = worktree();
    let reply = "Fixing both.\nsrc/lib.rs\n<<<<<<< SEARCH\n    1\n=======\n    10\n>>>>>>> REPLACE\n<<<<<<< SEARCH\n    2\n=======\n    20\n>>>>>>> REPLACE\n";
    let call = parse_model_action(reply).expect("parsed");
    assert!(matches!(call, RepoToolCall::EditBlocks { ref blocks } if blocks.len() == 2));
    let res = run(&wt, &call);
    assert!(res.ok, "{}", res.output);
    assert_eq!(res.modified_files, vec!["src/lib.rs".to_string()]);
    let content = wt.read_file(Path::new("src/lib.rs")).unwrap();
    assert!(content.contains("    10\n") && content.contains("    20\n"));
    let _ = fs::remove_dir_all(repo);
}

#[test]
fn json_edit_alias_is_accepted() {
    let call = parse_model_action(
        r#"{"tool": "repo:edit", "path": "src/lib.rs", "old_str": "    1", "new_str": "    5"}"#,
    )
    .expect("parsed");
    assert!(matches!(call, RepoToolCall::Edit { .. }));
}

#[test]
fn failing_block_rolls_back_the_whole_reply() {
    let (wt, repo) = worktree();
    let reply = "src/lib.rs\n<<<<<<< SEARCH\n    1\n=======\n    10\n>>>>>>> REPLACE\n<<<<<<< SEARCH\n    999\n=======\n    0\n>>>>>>> REPLACE\n";
    let res = run(&wt, &parse_model_action(reply).unwrap());
    assert!(!res.ok);
    assert!(res.output.contains("block 2 of 2"), "{}", res.output);
    let content = wt.read_file(Path::new("src/lib.rs")).unwrap();
    assert!(content.contains("    1\n"), "first block must not be written");
    let _ = fs::remove_dir_all(repo);
}

#[test]
fn path_escape_is_rejected() {
    let (wt, repo) = worktree();
    let reply = "../outside.rs\n<<<<<<< SEARCH\n=======\npub fn evil() {}\n>>>>>>> REPLACE\n";
    let res = run(&wt, &parse_model_action(reply).unwrap());
    assert!(!res.ok, "{}", res.output);
    assert!(!repo.parent().unwrap().join("outside.rs").exists());
    let _ = fs::remove_dir_all(repo);
}
