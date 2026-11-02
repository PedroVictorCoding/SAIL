# Architecture Decision Record: Production Integration of nono into SAIL Guardian

## Status
Accepted

## Context
SAIL's Guardian subsystem enforces capability-based security, process isolation, and owner approvals. However, the existing system relied on either coarse approval gates (prompting the owner for routine actions) or ad-hoc Bubblewrap scripts in `sail-forge` without OS-level LSM (Linux Security Module) enforcement across all tool execution paths.

`nono` (version `0.81.0`) provides capability-based sandboxing using Linux Landlock (enforcing up to ABI V6 on Linux 5.13+ kernels) and Seatbelt on macOS, alongside path sanitization, seccomp TCP filtering, credential scrubbing, and diagnostic reports.

On our target Linux platform (Linux kernel 7.0+), Landlock ABI V8 is supported, and nono enforces V6 features:
- Basic filesystem access control (read, write, execute)
- Directory renaming (`Refer`)
- File truncation
- TCP network filtering
- Signal and abstract UNIX socket scoping
- Process resource limits (pids, memory) via cgroups v2

## Decision

### 1. Library vs. CLI Integration
- **`nono` Rust library (`0.81.0`)**: Used directly inside `sail-guardian` for programmatic Landlock ABI detection, capability set generation (`CapabilitySet`, `AccessMode`, `NetworkMode`, `IpcMode`, `SignalMode`), seccomp network enforcement, credential path scrubbing, and diagnostic reasoning.
- **Bubblewrap (`bwrap`) defense-in-depth**: Retained and tightly integrated into Forge workers alongside Landlock. Landlock enforces kernel-level filesystem and network access restrictions that survive across `fork` and `execve`, while Bubblewrap provides mount namespace isolation (empty `/home`, `/tmp` tmpfs, read-only system binds) and unshared PID/network namespaces.
- **Resource Ceilings**: Layered via nono `ResourceLimits` and Linux `cgroups v2` / POSIX `setrlimit` (memory caps, PID limits, CPU timeouts).

### 2. Layering Architecture
```
+----------------------------------------------------------------+
|                   SAIL Application & UI Layer                  |
|    (Settings -> Security, Autonomy Presets, Approval Cards)    |
+----------------------------------------------------------------+
                               |
+----------------------------------------------------------------+
|              Guardian Authoritative Broker (GuardianSubsystem) |
|     PolicyEngine (Autonomy Presets: Careful, Balanced, Auto)    |
|     ApprovalBroker (Interactive Owner Approvals)               |
|     AuditLedger (Tamper-evident HMAC Audit Trail)              |
+----------------------------------------------------------------+
                               |
+----------------------------------------------------------------+
|                   SecurityRuntime Trait                        |
|  - inspect_capabilities() -> SecurityCapabilities             |
|  - validate(request) -> Result<AuthorizationDecision>          |
|  - execute(authorized) -> Result<ExecutionHandle>              |
|  - audit(event)                                                |
+----------------------------------------------------------------+
                               |
+----------------------------------------------------------------+
|                   NonoSecurityRuntime                          |
|  - Capability Profiles (ReadOnly, Workspace, Forge, etc.)      |
|  - nono::CapabilitySet + Landlock ABI application              |
|  - Bubblewrap namespace containment (where applicable)         |
|  - Credential scrubber & Unix socket protection                |
+----------------------------------------------------------------+
```

### 3. Capability Profiles
- **ReadOnly**: Code inspection, AST search, documentation. Reads allowlisted workspace files only, no network, no writes.
- **Workspace**: Project productivity, documentation editing. Read/write strictly within approved workspace directories.
- **Forge**: Autonomous coding. Disposable worktree writable; cargo dependencies and compiler read-only; network blocked; no credentials or host sockets.
- **Research**: Controlled web fetch through broker proxy; no credential inheritance.
- **Privileged**: System operations; requires explicit interactive owner approval token; short-lived.

### 4. Autonomy Presets
- **Careful**: Prompts owner for most workspace mutations and network operations.
- **Balanced (Default)**: Routine workspace file operations within granted worktrees proceed autonomously; elevated operations (external network, system telemetry, privileged tools) prompt.
- **Autonomous**: Deep autonomous coding workflows inside isolated worktrees run without interruptions; hard boundaries (credential stores, SSH keys, host files) remain strictly enforced and non-bypassable.

## Consequences
- No LLM prompt or tool can weaken or bypass the security policy.
- Landlock LSM ensures that even if a child process spawns sub-processes or shell scripts, restrictions are irreversibly inherited.
- Missing platform capabilities fail closed with clear diagnostics rather than silent unconfined fallback.

