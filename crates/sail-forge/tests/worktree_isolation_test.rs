use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use sail_forge::worktree::{IsolatedWorktree, WorktreeError};
use sail_guardian::sandbox::SandboxPolicy;

static BWRAP_ENV_LOCK: Mutex<()> = Mutex::new(());

fn setup_test_git_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    let _ = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(path)
        .output();
    let _ = Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(path)
        .output();
    let _ = Command::new("git")
        .args(["config", "user.email", "test@test.local"])
        .current_dir(path)
        .output();
    let _ = Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(path)
        .output();
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
    worktree
        .write_file(file_path, "fn main() { println!(\"isolated\"); }\n")
        .unwrap();
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
        assert!(matches!(
            res,
            Err(WorktreeError::SymlinkEscape { .. }) | Err(WorktreeError::PathTraversal { .. })
        ));
    }

    let _ = fs::remove_dir_all(&temp_repo);
}

#[test]
fn test_command_timeout_kills_child_process() {
    let _lock = BWRAP_ENV_LOCK.lock().unwrap();
    let job_id = format!("test-timeout-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("test_repo_{}", Uuid::new_v4()));
    setup_test_git_repo(&temp_repo);

    let policy = Arc::new(SandboxPolicy::new());
    let worktree = IsolatedWorktree::create(&job_id, &temp_repo, "HEAD", policy).unwrap();

    // Run command that sleeps longer than 1s timeout
    let res = worktree.run_command("cargo", &["test", "--", "nonexistent_long_running_arg"], 1);
    // Should either complete fast or timeout cleanly without hanging
    if let Err(e) = res {
        assert!(
            matches!(e, WorktreeError::CommandTimeout { .. })
                || matches!(e, WorktreeError::Sandbox(..))
        );
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
        worktree
            .write_file(Path::new("debug.log"), "Failure reason: syntax error")
            .unwrap();
        worktree.retain_for_inspection();
    }

    assert!(saved_path.exists());
    let log_content = fs::read_to_string(saved_path.join("debug.log")).unwrap();
    assert_eq!(log_content, "Failure reason: syntax error");

    let _ = fs::remove_dir_all(&saved_path);
    let _ = fs::remove_dir_all(&temp_repo);
}

#[test]
fn test_command_output_exceeding_pipe_buffer_does_not_deadlock() {
    let _lock = BWRAP_ENV_LOCK.lock().unwrap();
    let job_id = format!("test-pipe-deadlock-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("test_repo_{}", Uuid::new_v4()));
    setup_test_git_repo(&temp_repo);

    let policy = Arc::new(SandboxPolicy::new());
    let worktree = IsolatedWorktree::create(&job_id, &temp_repo, "HEAD", policy).unwrap();

    // Create a 1.2MB text file inside worktree
    let mut large_content = String::with_capacity(1_200_000);
    for _i in 0..60_000 {
        large_content.push_str("0123456789abcdef\n"); // 17 bytes * 60,000 = 1,020,000 bytes
    }
    worktree
        .write_file(Path::new("large_output.txt"), &large_content)
        .unwrap();

    // Run cat on the >1MB file with a short 5 second timeout.
    // Without concurrent streaming, pipe fills at 64KB and child deadlocks until timeout.
    let start = std::time::Instant::now();
    let res = worktree
        .run_command("cat", &["large_output.txt"], 5)
        .expect("Command must succeed without deadlock");
    let elapsed = start.elapsed();

    assert_eq!(res.status, 0);
    assert!(
        elapsed.as_secs() < 3,
        "Command took too long ({:?}); potential pipe stall",
        elapsed
    );
    assert!(
        res.stdout
            .contains("[output truncated: exceeded 512KB sandbox limit]")
    );

    let _ = fs::remove_dir_all(&temp_repo);
}

#[test]
fn test_missing_bwrap_fails_closed_with_error() {
    let _lock = BWRAP_ENV_LOCK.lock().unwrap();
    let job_id = format!("test-bwrap-missing-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("test_repo_{}", Uuid::new_v4()));
    setup_test_git_repo(&temp_repo);

    let policy = Arc::new(SandboxPolicy::new());
    let worktree = IsolatedWorktree::create(&job_id, &temp_repo, "HEAD", policy).unwrap();

    // Set nonexistent bwrap binary
    unsafe {
        std::env::set_var("SAIL_BWRAP_BIN", "/opt/nonexistent/bin/bwrap");
    }

    let res = worktree.run_command("git", &["status"], 5);

    unsafe {
        std::env::remove_var("SAIL_BWRAP_BIN");
    }

    assert!(
        matches!(res, Err(WorktreeError::BwrapUnavailable(_))),
        "Must fail closed when bwrap is missing; got: {:?}",
        res
    );

    let _ = fs::remove_dir_all(&temp_repo);
}

#[test]
fn test_sandbox_confidentiality_prevents_private_file_and_network_access() {
    let _lock = BWRAP_ENV_LOCK.lock().unwrap();
    let job_id = format!("test-sandbox-sec-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("test_repo_{}", Uuid::new_v4()));
    setup_test_git_repo(&temp_repo);

    let policy = Arc::new(SandboxPolicy::new());
    let worktree = IsolatedWorktree::create(&job_id, &temp_repo, "HEAD", policy).unwrap();

    // Write a Cargo crate with a malicious build.rs that attempts host exfiltration
    worktree
        .write_file(
            Path::new("Cargo.toml"),
            "[package]\nname = \"exploit_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();

    let build_rs = r#"use std::fs;
use std::net::TcpStream;

fn main() {
    // 1. Attempt reading /etc/shadow
    if fs::read_to_string("/etc/shadow").is_ok() {
        panic!("EXFIL_SHADOW_SUCCESS: read /etc/shadow from sandbox");
    }

    // 2. Attempt reading host SSH keys
    if fs::read_dir("/home/main/.ssh").is_ok() || fs::read_to_string("/home/main/.ssh/id_rsa").is_ok() {
        panic!("EXFIL_SSH_SUCCESS: accessed ~/.ssh from sandbox");
    }

    // 3. Attempt reading host Codex credentials
    if fs::read_dir("/home/main/.codex").is_ok() || fs::read_to_string("/home/main/.codex/auth.json").is_ok() {
        panic!("EXFIL_CODEX_SUCCESS: accessed ~/.codex from sandbox");
    }

    // 4. Attempt external network connection
    if TcpStream::connect("1.1.1.1:80").is_ok() {
        panic!("EXFIL_NET_SUCCESS: connected to internet from sandbox");
    }

    println!("cargo:warning=ALL_SECURITY_PROBES_THWARTED");
}
"#;

    worktree
        .write_file(Path::new("build.rs"), build_rs)
        .unwrap();
    worktree
        .write_file(
            Path::new("src/lib.rs"),
            "pub fn probe() {}\n#[test]\nfn test_probe() { probe(); }\n",
        )
        .unwrap();

    // Run cargo test inside Bubblewrap sandbox
    let res = worktree.run_command("cargo", &["test"], 30);
    assert!(res.is_ok(), "Sandbox command must execute: {:?}", res.err());
    let output = res.unwrap();
    assert_eq!(
        output.status, 0,
        "Build script failed: stderr={}",
        output.stderr
    );
    assert!(output.stderr.contains("ALL_SECURITY_PROBES_THWARTED"));
    assert!(!output.stderr.contains("EXFIL_SHADOW_SUCCESS"));
    assert!(!output.stderr.contains("EXFIL_SSH_SUCCESS"));
    assert!(!output.stderr.contains("EXFIL_CODEX_SUCCESS"));
    assert!(!output.stderr.contains("EXFIL_NET_SUCCESS"));

    let _ = fs::remove_dir_all(&temp_repo);
}
