use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

use sail_forge::worktree::{IsolatedWorktree, WorktreeError};
use sail_guardian::sandbox::SandboxPolicy;

#[test]
fn test_isolated_worktree_creation_and_file_safety() {
    let job_id = format!("test-job-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("test_repo_{}", Uuid::new_v4()));
    fs::create_dir_all(&temp_repo).unwrap();

    let policy = Arc::new(SandboxPolicy::new());
    let worktree = IsolatedWorktree::create(&job_id, &temp_repo, "HEAD", policy).unwrap();

    // Verify root directory is isolated
    assert!(worktree.root_dir.exists());
    assert_ne!(worktree.root_dir, temp_repo);

    // Verify writing file inside worktree
    let file_path = Path::new("src/main.rs");
    worktree.write_file(file_path, "fn main() { println!(\"isolated\"); }\n").unwrap();
    let content = worktree.read_file(file_path).unwrap();
    assert_eq!(content, "fn main() { println!(\"isolated\"); }\n");

    // Clean up
    let _ = fs::remove_dir_all(&temp_repo);
}

#[test]
fn test_path_traversal_prevention_outside_worktree() {
    let job_id = format!("test-traversal-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("test_repo_{}", Uuid::new_v4()));
    fs::create_dir_all(&temp_repo).unwrap();

    let policy = Arc::new(SandboxPolicy::new());
    let worktree = IsolatedWorktree::create(&job_id, &temp_repo, "HEAD", policy).unwrap();

    // Attempt path traversal to escape worktree
    let evil_path = Path::new("../../../etc/passwd");
    let result = worktree.write_file(evil_path, "malicious payload");
    assert!(matches!(result, Err(WorktreeError::PathTraversal { .. })));

    let evil_read = worktree.read_file(evil_path);
    assert!(matches!(evil_read, Err(WorktreeError::PathTraversal { .. })));

    let _ = fs::remove_dir_all(&temp_repo);
}

#[test]
fn test_patch_application_and_diff_generation() {
    let job_id = format!("test-patch-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("test_repo_{}", Uuid::new_v4()));
    fs::create_dir_all(&temp_repo).unwrap();

    let policy = Arc::new(SandboxPolicy::new());
    let worktree = IsolatedWorktree::create(&job_id, &temp_repo, "HEAD", policy).unwrap();

    // Initial file
    worktree.write_file(Path::new("src/lib.rs"), "pub fn add(a: i32, b: i32) -> i32 { a - b }\n").unwrap();

    // Apply patch to fix subtraction to addition
    let patch = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,1 @@\n-pub fn add(a: i32, b: i32) -> i32 { a - b }\n+pub fn add(a: i32, b: i32) -> i32 { a + b }\n";
    worktree.apply_patch(patch).unwrap();

    let modified = worktree.read_file(Path::new("src/lib.rs")).unwrap();
    assert!(modified.contains("pub fn add(a: i32, b: i32) -> i32 { a + b }"));

    let _ = fs::remove_dir_all(&temp_repo);
}

#[test]
fn test_worktree_retained_on_failure_for_inspection() {
    let job_id = format!("test-retain-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("test_repo_{}", Uuid::new_v4()));
    fs::create_dir_all(&temp_repo).unwrap();

    let saved_path: PathBuf;
    {
        let policy = Arc::new(SandboxPolicy::new());
        let mut worktree = IsolatedWorktree::create(&job_id, &temp_repo, "HEAD", policy).unwrap();
        saved_path = worktree.root_dir.clone();
        worktree.write_file(Path::new("debug.log"), "Failure reason: syntax error").unwrap();
        // Mark for retention
        worktree.retain_for_inspection();
    }

    // Worktree should still exist after drop because it was retained for user inspection
    assert!(saved_path.exists());
    let log_content = fs::read_to_string(saved_path.join("debug.log")).unwrap();
    assert_eq!(log_content, "Failure reason: syntax error");

    // Clean up
    let _ = fs::remove_dir_all(&saved_path);
    let _ = fs::remove_dir_all(&temp_repo);
}
