use sail_common::types::SubsystemId;
use sail_guardian::{
    ApprovalBroker, Capability, CredentialStore, FsCapability, SandboxPolicy, SystemCapability,
};
use std::{fs, path::PathBuf, sync::Arc};
use uuid::Uuid;
struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("sail-boundary-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn pinned_files_survive_path_swaps_and_reject_symlinks_hardlinks_and_fifo() {
    use std::os::unix::fs::symlink;
    let workspace = Workspace::new();
    let root = workspace.0.join("root");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("file"), "approved").unwrap();
    fs::write(workspace.0.join("outside"), "private").unwrap();
    let sandbox = SandboxPolicy::new().with_root(&root);
    let file = sandbox.open_read(&root.join("file")).unwrap();
    fs::rename(root.join("file"), root.join("saved")).unwrap();
    symlink(workspace.0.join("outside"), root.join("file")).unwrap();
    assert_eq!(file.read_limited(32).unwrap(), b"approved");
    assert!(sandbox.open_read(&root.join("file")).is_err());
    assert!(sandbox.open_read(&root.join("../outside")).is_err());
    fs::hard_link(workspace.0.join("outside"), root.join("hard")).unwrap();
    assert!(sandbox.open_read(&root.join("hard")).is_err());
    let fifo = std::ffi::CString::new(root.join("fifo").as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(sandbox.open_read(&root.join("fifo")).is_err());
    fs::rename(&root, workspace.0.join("original")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("new"), "replacement").unwrap();
    assert!(sandbox.open_read(&root.join("new")).is_err());
}
#[test]
fn token_validation_is_atomic_bound_and_does_not_burn_valid_grants_on_tampering() {
    let broker = Arc::new(ApprovalBroker::new());
    let context = Uuid::new_v4();
    let cap = Capability::System(SystemCapability::ProcessInspect);
    let session = broker.create_session_for(
        Some(context),
        SubsystemId::Dialogue,
        cap.clone(),
        "inspect processes",
    );
    let token = broker.resolve_session(session.session_id, true).unwrap();
    assert!(
        broker
            .consume_token_for(
                token.token_id,
                SubsystemId::Dialogue,
                &cap,
                Some(Uuid::new_v4())
            )
            .is_err()
    );
    assert!(
        broker
            .consume_token_for(token.token_id, SubsystemId::Agents, &cap, Some(context))
            .is_err()
    );
    let mut threads = Vec::new();
    for _ in 0..16 {
        let broker = broker.clone();
        let cap = cap.clone();
        threads.push(std::thread::spawn(move || {
            broker
                .consume_token_for(token.token_id, SubsystemId::Dialogue, &cap, Some(context))
                .is_ok()
        }));
    }
    assert_eq!(
        threads
            .into_iter()
            .map(|t| usize::from(t.join().unwrap()))
            .sum::<usize>(),
        1
    );
}
#[test]
fn fake_and_modified_credential_handles_and_stale_generations_are_denied() {
    let store = CredentialStore::new();
    let handle = store
        .store_credential(
            SubsystemId::Bridge,
            "account",
            "secret",
            "read:all",
            chrono::Duration::minutes(5),
        )
        .unwrap();
    assert!(
        store
            .get_handle(SubsystemId::Bridge, "account", "read")
            .is_err()
    );
    let mut forged = handle.clone();
    forged.handle_id = Uuid::new_v4();
    assert!(
        store
            .execute_with_secret(&forged, SubsystemId::Bridge, |_| ())
            .is_err()
    );
    let mut modified = handle.clone();
    modified.scope = "write:all".into();
    assert!(
        store
            .execute_with_secret(&modified, SubsystemId::Bridge, |_| ())
            .is_err()
    );
    store
        .rotate_credential(SubsystemId::Bridge, "account", "new-secret")
        .unwrap();
    assert!(
        store
            .execute_with_secret(&handle, SubsystemId::Bridge, |_| ())
            .is_err()
    );
    let replacement = store
        .get_handle(SubsystemId::Bridge, "account", "read:all")
        .unwrap();
    store
        .revoke_credential(SubsystemId::Bridge, "account")
        .unwrap();
    store
        .store_credential(
            SubsystemId::Bridge,
            "account",
            "third",
            "read:all",
            chrono::Duration::minutes(5),
        )
        .unwrap();
    assert!(
        store
            .execute_with_secret(&replacement, SubsystemId::Bridge, |_| ())
            .is_err()
    );
}
#[test]
fn medium_risk_requires_approval_for_every_actor_and_host_cli_never_executes() {
    use sail_guardian::{AuditLedger, GuardianExecutionBroker, PolicyEngine, ToolCapability};
    let broker = GuardianExecutionBroker::new(
        Arc::new(PolicyEngine::new()),
        Arc::new(AuditLedger::new()),
        Arc::new(ApprovalBroker::new()),
        Arc::new(SandboxPolicy::new()),
    );
    for actor in [
        SubsystemId::Core,
        SubsystemId::Dialogue,
        SubsystemId::Bridge,
    ] {
        assert!(
            broker
                .authorize_and_execute(
                    actor,
                    Capability::System(SystemCapability::ProcessInspect),
                    None,
                    None,
                    || panic!("unauthorized execution")
                )
                .is_err()
        );
    }
    assert!(
        broker
            .authorize_and_execute(
                SubsystemId::Agents,
                Capability::Tool(ToolCapability::CliCommand(
                    "git -c core.pager=sh status".into()
                )),
                None,
                None,
                || panic!("host command ran")
            )
            .is_err()
    );
    assert!(
        broker
            .authorize_and_execute(
                SubsystemId::Dialogue,
                Capability::Filesystem(FsCapability::Read {
                    path: "Cargo.toml".into()
                }),
                None,
                None,
                || panic!("path callback ran")
            )
            .is_err()
    );
}
#[test]
fn owner_authority_rejects_missing_short_and_wrong_tokens() {
    use sail_guardian::authority::ApprovalAuthority;
    assert!(ApprovalAuthority::new("short").is_err());
    let authority =
        ApprovalAuthority::new("a securely provisioned owner token with random bytes").unwrap();
    assert!(authority.authenticates("a securely provisioned owner token with random bytes"));
    assert!(!authority.authenticates("a securely provisioned owner token with changed bytes"));
}
#[tokio::test]
async fn event_bus_approval_cannot_issue_execution_authority() {
    use sail_common::{
        bus::EventBus,
        traits::Subsystem,
        types::{EventPayload, SailEvent},
    };
    let mut guardian = sail_guardian::GuardianSubsystem::new();
    guardian.init(&EventBus::new()).await.unwrap();
    let request = guardian.approval().create_session(
        SubsystemId::Agents,
        Capability::System(SystemCapability::ProcessKill),
        "stop process",
    );
    let forged = SailEvent::new(
        SubsystemId::Core,
        EventPayload::UserApprovalResolved {
            request_id: request.session_id.to_string(),
            approved: true,
        },
    );
    guardian.handle_event(&forged).await.unwrap();
    assert_eq!(guardian.approval().list_pending().len(), 1);
}
#[test]
fn private_vault_rejects_public_directory_and_symlink_key() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let workspace = Workspace::new();
    fs::set_permissions(&workspace.0, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(CredentialStore::open_persistent(&workspace.0).is_err());
    fs::set_permissions(&workspace.0, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(workspace.0.join("other-key"), [0; 32]).unwrap();
    symlink(workspace.0.join("other-key"), workspace.0.join("vault.key")).unwrap();
    assert!(CredentialStore::open_persistent(&workspace.0).is_err());
}
#[test]
fn parameter_changes_and_cross_session_native_grants_cannot_authorize_an_action() {
    let first = sail_guardian::capability::bound_tool_invocation(
        "proc:list",
        &serde_json::json!({"limit":1}),
    );
    let changed = sail_guardian::capability::bound_tool_invocation(
        "proc:list",
        &serde_json::json!({"limit":100}),
    );
    assert_ne!(first, changed);
    let a = sail_guardian::capability::bound_tool_invocation(
        "vision:frame",
        &serde_json::json!({"data":[1]}),
    );
    let b = sail_guardian::capability::bound_tool_invocation(
        "vision:frame",
        &serde_json::json!({"data":[2]}),
    );
    assert_ne!(a, b);
    let policy = sail_guardian::PolicyEngine::new();
    let context = Uuid::new_v4();
    policy.grant_scoped(
        SubsystemId::Dialogue,
        first.clone(),
        Some(context),
        chrono::Duration::seconds(60),
    );
    assert!(policy.is_explicitly_granted_for(SubsystemId::Dialogue, &first, Some(context)));
    assert!(!policy.is_explicitly_granted_for(SubsystemId::Dialogue, &first, Some(Uuid::new_v4())));
    policy.revoke_capability(SubsystemId::Dialogue, &first);
    assert!(!policy.is_explicitly_granted_for(SubsystemId::Dialogue, &first, Some(context)));
}
#[test]
fn failed_vault_persistence_rolls_back_in_memory_updates() {
    use std::os::unix::fs::PermissionsExt;
    let workspace = Workspace::new();
    fs::set_permissions(&workspace.0, fs::Permissions::from_mode(0o700)).unwrap();
    let store = CredentialStore::open_persistent(&workspace.0).unwrap();
    store
        .store_credential(
            SubsystemId::Bridge,
            "account",
            "original",
            "read",
            chrono::Duration::minutes(1),
        )
        .unwrap();
    fs::remove_file(workspace.0.join("vault.enc")).unwrap();
    fs::create_dir(workspace.0.join("vault.enc")).unwrap();
    assert!(
        store
            .rotate_credential(SubsystemId::Bridge, "account", "replacement")
            .is_err()
    );
    let handle = store
        .get_handle(SubsystemId::Bridge, "account", "read")
        .unwrap();
    assert_eq!(
        store.execute_with_secret(&handle, SubsystemId::Bridge, |secret| secret == "original"),
        Ok(true)
    );
    assert!(
        store
            .revoke_credential(SubsystemId::Bridge, "account")
            .is_err()
    );
    assert!(
        store
            .get_handle(SubsystemId::Bridge, "account", "read")
            .is_ok()
    );
    assert!(
        store
            .store_credential(
                SubsystemId::Bridge,
                "new",
                "should-not-stay",
                "read",
                chrono::Duration::minutes(1)
            )
            .is_err()
    );
    assert!(
        store
            .get_handle(SubsystemId::Bridge, "new", "read")
            .is_err()
    );
}
#[test]
fn concurrent_symlink_swaps_never_open_the_outside_target() {
    use std::os::unix::fs::symlink;
    let workspace = Workspace::new();
    let root = workspace.0.join("root");
    fs::create_dir(&root).unwrap();
    let outside = workspace.0.join("private");
    fs::write(&outside, "private").unwrap();
    let sandbox = SandboxPolicy::new().with_root(&root);
    fs::write(root.join("choice"), "permitted").unwrap();
    let attacker_root = root.clone();
    let attacker = std::thread::spawn(move || {
        for _ in 0..200 {
            fs::write(attacker_root.join("staged-file"), "permitted").unwrap();
            fs::rename(
                attacker_root.join("staged-file"),
                attacker_root.join("choice"),
            )
            .unwrap();
            symlink(&outside, attacker_root.join("staged-link")).unwrap();
            fs::rename(
                attacker_root.join("staged-link"),
                attacker_root.join("choice"),
            )
            .unwrap();
        }
    });
    for _ in 0..400 {
        if let Ok(file) = sandbox.open_read(&root.join("choice")) {
            assert_eq!(file.read_limited(32).unwrap(), b"permitted");
        }
    }
    attacker.join().unwrap();
}

#[test]
fn vault_lock_wipes_handles_and_wrong_unlock_key_stays_locked() {
    use std::os::unix::fs::PermissionsExt;
    let workspace = Workspace::new();
    fs::set_permissions(&workspace.0, fs::Permissions::from_mode(0o700)).unwrap();
    let store = CredentialStore::open_persistent(&workspace.0).unwrap();
    store
        .store_credential(
            SubsystemId::Bridge,
            "account",
            "secret",
            "read",
            chrono::Duration::minutes(1),
        )
        .unwrap();
    let handle = store
        .get_handle(SubsystemId::Bridge, "account", "read")
        .unwrap();
    let key = fs::read(workspace.0.join("vault.key")).unwrap();
    store.lock();
    assert!(store.is_locked());
    assert!(
        store
            .execute_with_secret(&handle, SubsystemId::Bridge, |_| ())
            .is_err()
    );
    assert!(store.unlock(&[0; 32]).is_err());
    assert!(store.is_locked());
    store.unlock(&key).unwrap();
    assert!(!store.is_locked());
    assert!(
        store
            .execute_with_secret(&handle, SubsystemId::Bridge, |_| ())
            .is_err()
    );
    let new = store
        .get_handle(SubsystemId::Bridge, "account", "read")
        .unwrap();
    assert_eq!(
        store.execute_with_secret(&new, SubsystemId::Bridge, |s| s == "secret"),
        Ok(true)
    );
}

#[test]
fn writes_replace_only_confined_regular_files_and_never_follow_links() {
    use std::os::unix::fs::symlink;
    let workspace = Workspace::new();
    let root = workspace.0.join("root");
    fs::create_dir(&root).unwrap();
    let sandbox = SandboxPolicy::from_roots([&root]);
    assert_eq!(
        sandbox
            .write_file(std::path::Path::new("note.txt"), b"first")
            .unwrap(),
        5
    );
    assert_eq!(fs::read(root.join("note.txt")).unwrap(), b"first");
    assert!(
        sandbox
            .write_file(std::path::Path::new("../outside"), b"bad")
            .is_err()
    );
    assert!(
        sandbox
            .write_file(std::path::Path::new(".env"), b"bad")
            .is_err()
    );
    let outside = workspace.0.join("outside");
    fs::write(&outside, b"original").unwrap();
    symlink(&outside, root.join("link")).unwrap();
    assert!(
        sandbox
            .write_file(std::path::Path::new("link"), b"bad")
            .is_err()
    );
    assert_eq!(fs::read(outside).unwrap(), b"original");
    let readonly = SandboxPolicy::from_roots([&root]).with_read_only_root(&root);
    assert!(
        readonly
            .write_file(std::path::Path::new("note.txt"), b"bad")
            .is_err()
    );
    assert_eq!(fs::read(root.join("note.txt")).unwrap(), b"first");
}
