# SAIL Guardian Threat Model: nono & Landlock Integration

## 1. Scope & System Overview
This document evaluates the security posture and threat vectors for SAIL's integration of the `nono` (0.81.0) capability-based security runtime and Linux Landlock LSM into the Guardian execution broker.

The system executes autonomous coding tasks (Forge), voice-driven commands, and workspace productivity workflows on Linux (Ubuntu 22.04 / 24.04, kernel 5.13+ with Landlock V1–V8).

## 2. Trust Boundaries & Workload Personas
- **Untrusted / Confined Entities**:
  - LLM-generated code, patches, shell inputs, and test filters.
  - Downloaded repository code, build scripts (`build.rs`), and dependencies.
  - Browser/web scrape content (untrusted input, prompt injection vector).
- **Trusted Host Components**:
  - Main SAIL daemon (`sail-core`, `sail-dialogue`).
  - Guardian Execution Broker (`GuardianExecutionBroker`, `PolicyEngine`, `AuditLedger`, `ApprovalBroker`).
  - Cryptographic credential vault (`CredentialStore`).

---

## 3. Threat Vectors & Defenses

### T1: Sandboxed Agent Escape & Path Traversal
- **Threat**: A model-generated build script or command attempts to traverse directory paths (via `../`, symlinks, hardlinks, or unshare escapes) to access host files outside the worktree.
- **Defense**:
  1. **Confined Root & Landlock LSM**: nono generates a `CapabilitySet` granting write access strictly to the assigned disposable worktree (`self.root_dir`). Landlock restricts filesystem traversal at the kernel LSM level, surviving sub-process spawning (`fork`/`clone`/`execve`).
  2. **Bubblewrap Namespace Defense-in-Depth**: Forge runs tools inside Bubblewrap with empty tmpfs mounts at `/home` and `/home/main`, read-only bind mounts on system libraries (`/usr`, `/lib`, `/bin`), and `--unshare-pid`, `--unshare-net`, `--unshare-ipc`, `--unshare-uts`.
  3. **Symlink Swaps**: `is_sensitive_host_path` and `ConfinedRoot` use fd-relative operations (O_NOFOLLOW / openat2) preventing TOCTOU symlink races.

### T2: Prompt Injection & Self-Elevation
- **Threat**: The LLM receives indirect prompt injection from an issue body, git commit message, or web page asking it to disable the sandbox, grant permissions, or execute privileged tools.
- **Defense**:
  1. **Immutable Security Policies**: Models do not have access to alter `PolicyEngine`, `SandboxPolicy`, or autonomy presets.
  2. **Non-Bypassable Point-of-Use Verification**: `GuardianExecutionBroker::authorize_and_execute` checks permissions at the concrete point of tool execution, consuming single-use HMAC-verified tokens.
  3. **Hard Boundaries**: Autonomy presets (`Careful`, `Balanced`, `Autonomous`) only modulate routine workflow friction within granted worktrees. Critical actions (`command:execute`, `ProcessKill`, out-of-tree writes) *always* require interactive user token verification.

### T3: Credential & Secret Exfiltration
- **Threat**: An agent tries to read SSH keys (`~/.ssh`), GPG keys (`~/.gnupg`), GitHub tokens (`~/.config/gh`), cloud credentials (`~/.aws`), or SAIL's persistent approval token (`.local/sail_owner_token`).
- **Defense**:
  1. **Strict Path Blacklist & Landlock Isolation**: Sensitive directories are explicitly blocked by `is_sensitive_host_path` and never included in nono capability sets.
  2. **Environment Sanitization**: `bwrap` runs with `--clearenv`, stripping all secret environment variables (`GITHUB_TOKEN`, `AWS_SECRET_ACCESS_KEY`, `SAIL_APPROVAL_TOKEN`) before worker execution.
  3. **Audit Ledger Redaction**: Credential patterns and token values are redacted before being logged to disk.

### T4: Network Ingress/Egress & Exfiltration
- **Threat**: Untrusted code or malicious crates in `cargo test` attempt to beacon home, open reverse shells, or download secondary payloads.
- **Defense**:
  1. **Network Namespace Unsharing**: Bubblewrap worker runs with `--unshare-net`.
  2. **Landlock / Seccomp TCP Blocking**: nono's `NetworkMode::Blocked` applies kernel Landlock TCP rules (on ABI V4+) and seccomp filters preventing socket creation and TCP connect calls.
  3. **Offline Environment**: Workers receive `CARGO_NET_OFFLINE=true`.

### T5: Resource Exhaustion & Denial of Service (Fork Bombs, OOM)
- **Threat**: Model code runs infinite loops, fork bombs, or allocates gigabytes of RAM to destabilize the host.
- **Defense**:
  1. **Timeouts**: Strict timeout enforcement terminates the entire process group with `SIGKILL` on expiration.
  2. **Process Limits & Cgroups**: nono `ResourceLimits` and cgroup v2 controllers enforce maximum tasks/pids and resident memory caps (`memory.max`, `pids.max`).
  3. **Output Volume Caps**: Stdout and stderr are concurrently drained with a 512KB limit to prevent pipe buffer deadlocks.

---

## 4. Residual Risks & Operational Assumptions
- **Host Kernel Requirement**: Landlock filesystem enforcement requires Linux kernel 5.13+. On kernels < 5.13 without Bubblewrap, `SecurityStatus::Unavailable` is reported and protected operations fail closed.
- **Rootless Operation**: SAIL daemon and Forge workers run as an unprivileged local user (`main`), never as root.

