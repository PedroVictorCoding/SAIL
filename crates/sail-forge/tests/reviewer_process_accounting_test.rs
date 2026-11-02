use sail_forge::contracts::{ReviewPolicyConfig, ReviewRequest};
use sail_forge::reviewer::ReviewBroker;
use std::fs;
use uuid::Uuid;

fn accounting_request(issue: usize) -> ReviewRequest {
    ReviewRequest {
        job_id: format!("accounting-{issue}"),
        issue_id: format!("repo#{}", issue),
        base_commit: "base".into(),
        diff_patch: "verified patch".into(),
        test_manifest: "test result: ok".into(),
    }
}

fn accounting_config() -> ReviewPolicyConfig {
    ReviewPolicyConfig {
        enabled: true,
        max_requests_per_day: 100,
        max_requests_per_week: 100,
        ..ReviewPolicyConfig::default()
    }
}

#[test]
fn ledger_process_worker() {
    let Some(dir) = std::env::var_os("SAIL_TEST_REVIEW_LEDGER_PROCESS_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let broker = ReviewBroker::new(accounting_config())
        .with_ledger_path(dir.join("ledger.json"))
        .with_mock_mode(true);
    fs::write(dir.join(format!("ready-{}", std::process::id())), "1").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !dir.join("start").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "parent did not release workers"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(broker.review(&accounting_request(1)));
    if broker.invocation_count() > 0 {
        fs::write(dir.join(format!("invoked-{}", std::process::id())), "1").unwrap();
    }
}

#[test]
fn separate_processes_cannot_invoke_the_same_issue_twice() {
    let dir = std::env::temp_dir().join(format!("review-processes-{}", Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    let children: Vec<_> = (0..20)
        .map(|_| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "ledger_process_worker"])
                .env("SAIL_TEST_REVIEW_LEDGER_PROCESS_DIR", &dir)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while fs::read_dir(&dir)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("ready-")
        })
        .count()
        != 20
    {
        assert!(
            std::time::Instant::now() < deadline,
            "workers failed to initialize"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    fs::write(dir.join("start"), "1").unwrap();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(
        fs::read_dir(&dir)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("invoked-")
            })
            .count(),
        1
    );
    fs::remove_dir_all(dir).unwrap();
}
