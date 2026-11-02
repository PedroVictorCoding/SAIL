#![cfg(target_os = "linux")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use chrono::{Duration, Utc};
use sail_common::SubsystemId;
use sail_guardian::runtime::{AuthorizedAction, CapabilityProfile};
use sail_guardian::{Capability, FsCapability};
use uuid::Uuid;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("sail-nono-{}", Uuid::new_v4()));
        for name in ["read", "write", "outside"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("sentinel"), "original").unwrap();
        }
        Self(root)
    }
    fn action(&self, profile: CapabilityProfile) -> AuthorizedAction {
        AuthorizedAction {
            authorization_id: Uuid::new_v4(),
            token_id: None,
            correlation_id: None,
            caller: SubsystemId::Forge,
            profile,
            capability: Capability::Filesystem(FsCapability::Read {
                path: self.0.join("read"),
            }),
            allowed_read_paths: vec![self.0.join("read")],
            allowed_write_paths: vec![self.0.join("write")],
            network_mode: nono::NetworkMode::Blocked,
            memory_limit_bytes: None,
            max_processes: None,
            expires_at: Utc::now() + Duration::seconds(60),
        }
    }
    fn run(&self, action: AuthorizedAction, script: &str, paths: &[&Path]) -> Output {
        let mut args = vec!["-c".to_owned(), script.to_owned(), "probe".to_owned()];
        args.extend(paths.iter().map(|p| p.to_str().unwrap().to_owned()));
        let request = serde_json::json!({"action":action,"program":"/bin/bash","args":args,"cwd":self.0.join("read")});
        worker(&serde_json::to_vec(&request).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn worker(input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sail-nono-worker"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}
fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn enforces_filesystem_rules_across_exec_without_confining_parent() {
    let f = Fixture::new();
    let read = f.0.join("read/sentinel");
    let write = f.0.join("write/sentinel");
    let outside = f.0.join("outside/sentinel");
    let out = f.run(f.action(CapabilityProfile::Forge),
        "set -eu; test \"$(cat \"$1\")\" = original; printf changed > \"$2\"; if cat \"$3\"; then exit 10; fi; if printf bad > \"$1\"; then exit 11; fi; if printf bad > \"$3\"; then exit 12; fi",
        &[&read, &write, &outside]);
    assert_success(&out);
    assert!(String::from_utf8_lossy(&out.stderr).contains("Permission denied"));
    assert_eq!(std::fs::read_to_string(&write).unwrap(), "changed");
    for path in [&read, &outside] {
        assert_eq!(std::fs::read_to_string(path).unwrap(), "original");
    }
    // Parent can still access and change a path denied to the worker.
    std::fs::write(&outside, "parent").unwrap();
}

#[test]
fn read_only_profile_does_not_grant_requested_writes() {
    let f = Fixture::new();
    let out = f.run(
        f.action(CapabilityProfile::ReadOnly),
        "if printf bad > \"$1\"; then exit 10; fi",
        &[&f.0.join("write/sentinel")],
    );
    assert_success(&out);
    assert!(String::from_utf8_lossy(&out.stderr).contains("Permission denied"));
    assert_eq!(
        std::fs::read_to_string(f.0.join("write/sentinel")).unwrap(),
        "original"
    );
}

#[test]
fn blocks_tcp_to_a_reachable_listener() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // Establish reachability in the parent, so refusal cannot be a closed port.
    std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let f = Fixture::new();
    let out = f.run(
        f.action(CapabilityProfile::Forge),
        &format!("if echo probe > /dev/tcp/127.0.0.1/{port}; then exit 10; fi"),
        &[],
    );
    assert_success(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Permission denied") || stderr.contains("Operation not permitted"),
        "{stderr}"
    );
}

#[test]
fn expired_authorization_never_executes_target() {
    let f = Fixture::new();
    let mut action = f.action(CapabilityProfile::Forge);
    action.expires_at = Utc::now() - Duration::seconds(1);
    let out = f.run(
        action,
        "printf bad > \"$1\"",
        &[&f.0.join("write/sentinel")],
    );
    assert_eq!(out.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&out.stderr).contains("authorization expired"));
    assert_eq!(
        std::fs::read_to_string(f.0.join("write/sentinel")).unwrap(),
        "original"
    );
}

#[test]
fn malformed_request_is_refused() {
    let out = worker(b"{}");
    assert_eq!(out.status.code(), Some(125));
}

#[test]
fn ungranted_executable_is_refused_after_enforcement() {
    let f = Fixture::new();
    let program = f.0.join("outside/true");
    std::fs::copy("/bin/true", &program).unwrap();
    let request = serde_json::json!({"action":f.action(CapabilityProfile::Forge),"program":program,"args":[],"cwd":f.0.join("read")});
    let out = worker(&serde_json::to_vec(&request).unwrap());
    assert_eq!(out.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Permission denied"));
}

#[test]
fn oversized_request_is_refused() {
    let out = worker(&vec![b' '; 64 * 1024 + 1]);
    assert_eq!(out.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&out.stderr).contains("exceeds 64 KiB"));
}

#[test]
fn target_does_not_inherit_supervisor_environment() {
    let f = Fixture::new();
    let out = f.run(
        f.action(CapabilityProfile::Forge),
        "set -eu; test -z \"${HOME+x}\"; test -z \"${CARGO_MANIFEST_DIR+x}\"",
        &[],
    );
    assert_success(&out);
}

#[test]
fn quota_requests_are_refused_instead_of_silently_ignored() {
    let f = Fixture::new();
    let mut action = f.action(CapabilityProfile::Forge);
    action.memory_limit_bytes = Some(512 * 1024 * 1024);
    let out = f.run(
        action,
        "printf bad > \"$1\"",
        &[&f.0.join("write/sentinel")],
    );
    assert_eq!(out.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot enforce quotas"));
    assert_eq!(
        std::fs::read_to_string(f.0.join("write/sentinel")).unwrap(),
        "original"
    );
}

#[test]
fn proxy_requests_require_supervised_filter_integration() {
    let f = Fixture::new();
    let mut action = f.action(CapabilityProfile::Forge);
    action.network_mode = nono::NetworkMode::ProxyOnly {
        port: 8080,
        bind_ports: vec![],
    };
    let out = f.run(action, "exit 0", &[]);
    assert_eq!(out.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&out.stderr).contains("requires a supervised proxy filter"));
}
