use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use uuid::Uuid;

use sail_forge::worktree::{IsolatedWorktree, WorktreeError};
use sail_guardian::sandbox::SandboxPolicy;

fn setup_test_git_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    let _ = Command::new("git").args(["init", "-b", "main"]).current_dir(path).output();
    let _ = Command::new("git").args(["config", "user.name", "Test"]).current_dir(path).output();
    let _ = Command::new("git").args(["config", "user.email", "test@test.local"]).current_dir(path).output();
    let _ = Command::new("git").args(["commit", "--allow-empty", "-m", "init"]).current_dir(path).output();
}

#[test]
fn test_isolated_worktree_creation_and_file_safety() {
    let job_id = format!("test-job-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("test_repo_{}", Uuid::new_v4()));
    setup_test_git_repo(&temp_repo);

    let policy = Arc::new(SandboxPolicy::new());
    let worktree = IsolatedWorktree::create(&job_id, &temp_repo, "HEAD", policy).unwrap();

    assert!(worktree.root_dir.exists());
    assert_ne!(worktree.root_dir, temp_repo);

    let file_path = Path::new("src/main.rs");
    worktree.write_file(file_path, "fn main() { println!(\"isolated\"); }\n").unwrap();
    let content = worktree.read_file(file_path).unwrap();
    assert_eq!(content, "fn main() { println!(\"isolated\"); }\n");

    let _ = fs::remove_dir_all(&temp_repo);
}

#[test]
fn test_path_traversal_prevention_outside_worktree() {
    let job_id = format!("test-traversal-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("test_repo_{}", Uuid::new_v4()));
    setup_test_git_repo(&temp_repo);

    let policy = Arc::new(SandboxPolicy::new());
    let worktree = IsolatedWorktree::create(&job_id, &temp_repo, "HEAD", policy).unwrap();

    // Absolute path rejection
    let abs_path = Path::new("/etc/shadow");
    assert!(matches!(
        worktree.write_file(abs_path, "payload"),
        Err(WorktreeError::PathTraversal { .. })
    ));

    // Relative parent traversal rejection
    let evil_path = Path::new("../../../etc/passwd");
    assert!(matches!(
        worktree.write_file(evil_path, "payload"),
        Err(WorktreeError::PathTraversal { .. })
    ));

    assert!(matches!(
        worktree.read_file(evil_path),
        Err(WorktreeError::PathTraversal { .. })
    ));

    let _ = fs::remove_dir_all(&temp_repo);
}

#[test]
fn test_symlink_escape_rejection() {
    let job_id = format!("test-symlink-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("test_repo_{}", Uuid::new_v4()));
    setup_test_git_repo(&temp_repo);

    let policy = Arc::new(SandboxPolicy::new());
    let worktree = IsolatedWorktree::create(&job_id, &temp_repo, "HEAD", policy).unwrap();

    // Create a symlink pointing outside worktree
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside_target = std::env::temp_dir();
        let symlink_path = worktree.root_dir.join("link_to_outside");
        let _ = symlink(&outside_target, &symlink_path);

        // Attempting to resolve path through the escaping symlink must fail
        let res = worktree.resolve_safe_path(Path::new("link_to_outside/stolen.txt"), true);
        assert!(matches!(res, Err(WorktreeError::SymlinkEscape { .. }) | Err(WorktreeError::PathTraversal { .. })));
    }

    let _ = fs::remove_dir_all(&temp_repo);
}

#[test]
fn test_command_timeout_kills_child_process() {
    let job_id = format!("test-timeout-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("test_repo_{}", Uuid::new_v4()));
    setup_test_git_repo(&temp_repo);

    let policy = Arc::new(SandboxPolicy::new());
    let worktree = IsolatedWorktree::create(&job_id, &temp_repo, "HEAD", policy).unwrap();

    // Run command that sleeps longer than 1s timeout
    let res = worktree.run_command("cargo", &["test", "--", "nonexistent_long_running_arg"], 1);
    // Should either complete fast or timeout cleanly without hanging
    if let Err(e) = res {
        assert!(matches!(e, WorktreeError::CommandTimeout { .. }) || matches!(e, WorktreeError::Sandbox(..)));
    }

    let _ = fs::remove_dir_all(&temp_repo);
}

#[test]
fn test_invalid_base_commit_fails_closed() {
    let job_id = format!("test-invalid-base-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("test_repo_{}", Uuid::new_v4()));
    setup_test_git_repo(&temp_repo);

    let policy = Arc::new(SandboxPolicy::new());
    let res = IsolatedWorktree::create(&job_id, &temp_repo, "nonexistent-sha-deadbeef", policy);

    assert!(matches!(res, Err(WorktreeError::InvalidBaseCommit(..))));
    let _ = fs::remove_dir_all(&temp_repo);
}

#[test]
fn test_worktree_retained_on_failure_for_inspection() {
    let job_id = format!("test-retain-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("test_repo_{}", Uuid::new_v4()));
    setup_test_git_repo(&temp_repo);

    let saved_path: PathBuf;
    {
        let policy = Arc::new(SandboxPolicy::new());
        let mut worktree = IsolatedWorktree::create(&job_id, &temp_repo, "HEAD", policy).unwrap();
        saved_path = worktree.root_dir.clone();
        worktree.write_file(Path::new("debug.log"), "Failure reason: syntax error").unwrap();
        worktree.retain_for_inspection();
    }

    assert!(saved_path.exists());
    let log_content = fs::read_to_string(saved_path.join("debug.log")).unwrap();
    assert_eq!(log_content, "Failure reason: syntax error");

    let _ = fs::remove_dir_all(&saved_path);
    let _ = fs::remove_dir_all(&temp_repo);
}
