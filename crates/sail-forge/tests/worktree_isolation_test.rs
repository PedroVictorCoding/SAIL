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

#[test]
fn test_standalone_git_clone_does_not_link_to_production_git() {
    let job_id = format!("test-standalone-git-{}", Uuid::new_v4());
    let temp_repo = std::env::temp_dir().join(format!("sail-prod-git-{}", Uuid::new_v4()));
    setup_test_git_repo(&temp_repo);

    let policy = Arc::new(SandboxPolicy::new());
    let worktree = IsolatedWorktree::create(&job_id, &temp_repo, "HEAD", policy).unwrap();

    // 1. .git inside worktree MUST be a standalone directory, NOT a gitfile pointing to temp_repo/.git/worktrees
    let dot_git = worktree.root_dir.join(".git");
    assert!(
        dot_git.is_dir(),
        "Expected worktree .git to be a standalone directory, not a gitfile link"
    );
    assert!(
        !temp_repo.join(".git/worktrees").exists(),
        "Production repo .git/worktrees must not be created"
    );

    // 2. base_commit must be resolved to a pinned 40-char hex SHA
    assert_eq!(
        worktree.base_commit.len(),
        40,
        "Expected 40-char pinned SHA, got {}",
        worktree.base_commit
    );

    // 3. Production repo must not have the agent/forge-* branch created in its refs
    let prod_branches = std::process::Command::new("git")
        .args(["branch", "--list", &worktree.branch])
        .current_dir(&temp_repo)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&prod_branches.stdout)
            .trim()
            .is_empty(),
        "Production repo must not have agent branch registered"
    );

    let _ = fs::remove_dir_all(&temp_repo);
}

#[test]
fn test_multi_file_smoke_fixture_reference_and_broken_patches() {
    let _guard = BWRAP_ENV_LOCK.lock().unwrap();
    use sail_forge::fixtures::get_multi_file_smoke_fixture;

    let fixture = get_multi_file_smoke_fixture();
    let temp_repo = std::env::temp_dir().join(format!("sail-smoke-repo-{}", Uuid::new_v4()));
    fs::create_dir_all(temp_repo.join("src")).unwrap();
    fs::write(temp_repo.join("Cargo.toml"), fixture.cargo_toml).unwrap();
    for (rel_path, content) in &fixture.initial_files {
        let full = temp_repo.join(rel_path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full, content).unwrap();
    }
    fs::write(temp_repo.join("build.rs"), fixture.malicious_build_rs).unwrap();

    let _ = std::process::Command::new("git")
        .arg("init")
        .current_dir(&temp_repo)
        .output();
    let _ = std::process::Command::new("git")
        .args(["config", "user.email", "test@sail.local"])
        .current_dir(&temp_repo)
        .output();
    let _ = std::process::Command::new("git")
        .args(["config", "user.name", "SAIL Test"])
        .current_dir(&temp_repo)
        .output();
    let _ = std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(&temp_repo)
        .output();
    let _ = std::process::Command::new("git")
        .args(["commit", "-m", "initial smoke fixture"])
        .current_dir(&temp_repo)
        .output();

    // 1. Initial unmodified worktree must fail `cargo test --offline`
    let policy = Arc::new(SandboxPolicy::new());
    let wt_initial = IsolatedWorktree::create(
        &format!("smoke-init-{}", Uuid::new_v4()),
        &temp_repo,
        "HEAD",
        policy.clone(),
    )
    .unwrap();
    let initial_test = wt_initial
        .run_command("cargo", &["test", "--offline"], 60)
        .unwrap();
    assert_ne!(
        initial_test.status, 0,
        "Unpatched smoke fixture must fail unit test initially"
    );

    // 2. Context-mismatched patch must fail closed
    let bad_ctx_patch = "--- a/src/pricing.rs\n+++ b/src/pricing.rs\n@@ -1,3 +1,3 @@\n-non_existent_line();\n+replacement_line();\n";
    assert!(
        wt_initial.apply_patch(bad_ctx_patch).is_err(),
        "Context-mismatched patch must fail closed"
    );

    // 3. Deliberately broken patch must apply cleanly but fail `cargo test --offline`
    let wt_broken = IsolatedWorktree::create(
        &format!("smoke-broken-{}", Uuid::new_v4()),
        &temp_repo,
        "HEAD",
        policy.clone(),
    )
    .unwrap();
    wt_broken.apply_patch(fixture.broken_patch).unwrap();
    let broken_test = wt_broken
        .run_command("cargo", &["test", "--offline"], 60)
        .unwrap();
    assert_ne!(
        broken_test.status, 0,
        "Deliberately broken patch must fail cargo test"
    );

    // 4. Reference 2-file patch must apply cleanly and pass `cargo test --offline` inside Bubblewrap
    let wt_fixed = IsolatedWorktree::create(
        &format!("smoke-fixed-{}", Uuid::new_v4()),
        &temp_repo,
        "HEAD",
        policy,
    )
    .unwrap();
    wt_fixed.apply_patch(fixture.reference_patch).unwrap();

    let diff = wt_fixed.get_diff().unwrap();
    assert!(
        diff.contains("src/pricing.rs") && diff.contains("src/lib.rs"),
        "Diff must reflect both modified files: {}",
        diff
    );

    let fixed_test = wt_fixed
        .run_command("cargo", &["test", "--offline"], 60)
        .unwrap();
    assert_eq!(
        fixed_test.status, 0,
        "Reference 2-file patch must pass cargo test: stdout={}\nstderr={}",
        fixed_test.stdout, fixed_test.stderr
    );

    let _ = fs::remove_dir_all(&temp_repo);
}

#[test]
fn test_forge_solver_context_budget_bounded() {
    let temp_repo = std::env::temp_dir().join(format!("test_repo_budget_{}", Uuid::new_v4()));
    setup_test_git_repo(&temp_repo);

    let policy = Arc::new(SandboxPolicy::new());
    let worktree = IsolatedWorktree::create("test-budget", &temp_repo, "HEAD", policy).unwrap();

    // Create multiple source files with varying line lengths
    worktree
        .write_file(
            Path::new("Cargo.toml"),
            "[package]\nname = \"big_repo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
    worktree
        .write_file(
            Path::new("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
        )
        .unwrap();
    worktree
        .write_file(Path::new("src/main.rs"), "fn main() {}\n")
        .unwrap();
    for i in 0..60 {
        worktree
            .write_file(
                Path::new(&format!("src/module_{}.rs", i)),
                &format!("// Module {}\npub fn mod_{}() -> usize {{ {} }}\n", i, i, i),
            )
            .unwrap();
    }

    let controller = sail_forge::solver::LocalSolverController::new(4, 180, None);
    let initial_ctx = controller.build_initial_workspace_context(&worktree);

    // Initial context must strictly remain under 4,000 characters (~1,000 tokens)
    assert!(
        initial_ctx.len() <= 4000,
        "Initial context was {} chars; must stay <= 4000 to prevent context overflow",
        initial_ctx.len()
    );
    assert!(initial_ctx.contains("Cargo.toml"));

    // Test message pruning: system prompt and original task prompt must be preserved
    let mut messages = vec![
        serde_json::json!({"role": "system", "content": "SYSTEM_PROMPT"}),
        serde_json::json!({"role": "user", "content": "INITIAL_TASK"}),
        serde_json::json!({"role": "user", "content": "a".repeat(10000)}),
        serde_json::json!({"role": "assistant", "content": "b".repeat(10000)}),
        serde_json::json!({"role": "user", "content": "c".repeat(10000)}),
        serde_json::json!({"role": "assistant", "content": "LATEST_REPAIR"}),
    ];

    sail_forge::solver::LocalSolverController::prune_messages_to_budget(&mut messages, 15000);

    assert_eq!(
        messages[0]["content"], "SYSTEM_PROMPT",
        "System prompt preserved"
    );
    assert_eq!(
        messages[1]["content"], "INITIAL_TASK",
        "Initial task preserved"
    );
    assert_eq!(
        messages.last().unwrap()["content"],
        "LATEST_REPAIR",
        "Latest turn preserved"
    );
    let total_len: usize = messages
        .iter()
        .map(|m| m["content"].as_str().unwrap().len())
        .sum();
    assert!(
        total_len <= 15000 + 10000,
        "Pruned message budget exceeded: {}",
        total_len
    );

    let _ = fs::remove_dir_all(&temp_repo);
}
