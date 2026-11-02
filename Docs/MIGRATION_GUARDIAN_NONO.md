# Migration Guide: nono Security Runtime in SAIL Guardian

## Overview
This document covers the migration of SAIL's Guardian subsystem from standalone ad-hoc checks to the integrated `nono` (0.81.0) capability runtime, Landlock LSM enforcement, and layered Bubblewrap workers.

## Key Architecture Enhancements
1. **Central `SecurityRuntime` Trait**:
   Replaces fragmented path checks with a unified capability builder and validator (`NonoSecurityRuntime`).
2. **Capability Profiles**:
   Standardized least-privilege profiles:
   - `ReadOnly`: Inspection, search, no writes, no network.
   - `Workspace`: Granted project directories only.
   - `Forge`: Isolated disposable worktree, read-only compiler/deps, network blocked.
   - `Research`: Broker proxy only, no host credentials.
   - `Privileged`: System actions, requires interactive owner token.
3. **Autonomy Presets**:
   Users can choose their preferred interaction frequency in **Settings → Guardian Security**:
   - `Careful`: High confirmation mode; prompts for all filesystem modifications.
   - `Balanced` (Default): Routine workspace/worktree edits proceed automatically; elevated/external tasks prompt.
   - `Autonomous`: Unattended workflows inside strict sandboxes run without approval interruptions. Hard security boundaries remain strictly enforced.
4. **Layered Worker Isolation**:
   Forge execution workers in `IsolatedWorktree::run_command` combine Bubblewrap namespace containerization (`--unshare-net`, `--unshare-pid`, `--unshare-ipc`, `--unshare-uts`, clean env) with nono capability boundaries and timeout termination.

## Compatibility & Rollback
- Existing API contracts for `GuardianExecutionBroker::new` and `ApprovalBroker` are fully backward-compatible.
- Existing tokens and HMAC audit chains remain valid.
- If Landlock is unavailable on a legacy kernel, the system reports `LimitedProtection` or `Unavailable`, failing closed safely on mandatory operations.

