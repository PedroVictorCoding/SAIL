# Execution safety audit: Guardian, nono and Forge

Audit date: 2026-11-02. Reviewed default branch `main` at
`215e177a028caa957ca3d1178a0105c44c7793b2`.
Tracks [#43](https://github.com/PedroVictorCoding/SAIL/issues/43) and
[#94](https://github.com/PedroVictorCoding/SAIL/issues/94). Both remain open.
This is an assessment of the checked-in implementation and targeted local checks,
not certification of a deployed host or a completed penetration test.

## Enforcement inventory

| Execution path | Implemented protection | Evidence and remaining gap |
| --- | --- | --- |
| Guardian `nono` runtime | Support detection, policy validation and capability construction | [Runtime][runtime] builds capabilities, but repository-wide call-site inspection finds no production caller applying the constructed sandbox. The only invocation of `build_capability_set` outside its declarations is a unit test. Policy approval is not process confinement. |
| User command service | Dedicated non-root UID distinct from daemon, trusted root image checks, Bubblewrap, cgroups, rlimits, bounded protocol/output, timeout/cancellation and peer credentials | [Runner][runner], [protocol][protocol] and [service unit][service]. Implemented, not verified end to end on a correctly provisioned service. Refusal tests do not demonstrate successful confinement. |
| User command adapter | Typed argv and scheduler authorization before dispatch to the separate service | [Adapter][adapter] and [scheduler grant consumption][scheduler]. This is a separate path from Forge builds. Service deployment and the complete Web/Android authorization path still require verification. |
| Forge tests/builds | Guardian command preparation, Bubblewrap mounts, network/PID/IPC namespaces, cleared environment, process-group timeout logic and 512 KiB captured output per stream | [`run_command`][forge-command] does not dispatch through the dedicated service or apply nono capabilities. It lacks equivalent cgroup resource controls; its synchronous API takes no cancellation token. Whole Cargo/Rustup directories are mounted read-only, so credential confidentiality still needs scrutiny. |
| Forge file edits | Lexical/policy checks and checks for currently visible escaping symlinks | [`write_file`/`read_file`][forge-files] use ordinary path-based operations after validation. [`apply_patch`][forge-patch] launches host Git and has a path-based fallback. Descriptor-bound editing, hardlink/mount handling and races remain open. Guardian's confined filesystem tests do not establish that Forge uses that API. |
| Forge allocation and cloning | Validated job IDs, private non-overlapping base, exclusive allocation, pinned descriptors, identity checks before cleanup, independent local clone and pinned commit | [Allocation][allocation], [cleanup][cleanup] and [clone][clone]. These protections are exercised by allocation/clone tests. Their scope does not include every later edit, command or publication operation. |
| Git, reviewer and publishing helpers | Varying path validation, review accounting, some command timeouts and draft publication | [Worktree Git][forge-patch], [reviewer spawn][reviewer] and [publisher][publisher] launch host subprocesses. Publishing intentionally uses a separate Git worktree sharing source metadata; allocation protections for coding workspaces do not automatically cover it. Trusted-operation boundaries, credentials, hooks/configuration and export authorization need separate review. |
| Benchmark grading helper | Temporary files and real compiler/test grading | [`grade_source_test`][benchmark] compiles and executes on the host without the isolated runner. The candidate evaluation path currently grades static fixture reference patches. Confine this helper before accepting untrusted source; this audit does not establish a remotely reachable exploit. |

### Security status is support detection

[`inspect_capabilities`][status] selects `Protected` when Landlock support is
reported and a Bubblewrap binary is found. It infers cgroup availability from the
presence of `cgroup.controllers`. These checks do not establish working namespace
creation, delegated cgroups, configured runner service or applied worker rules.
The [status HTTP endpoint][status-http] exposes these values from a new runtime
instance rather than a successful worker probe.

The six [nono integration tests][nono-tests] check support reporting, policy
outcomes, presets and audit state. Despite their names, they do not launch a
confined child. Their passing results must not be described as demonstrated
nono process enforcement.

## Acceptance reconciliation

“Partial” means individual mechanisms exist; the original combined acceptance
criterion remains unchecked. “Open” means the requested guarantee is not yet
established. No issue is closed by this document.

### #94 — Forge isolation and worktree safety

| Original acceptance criterion | Status | Evidence / outstanding requirement |
| --- | --- | --- |
| Malicious build script cannot read/write production or fetch network | Open | Namespace/mount configuration and a malicious-build fixture exist, but successful adversarial execution was not established in this audit environment. Credential mounts and dedicated-runner integration remain unresolved. |
| Symlink escape blocked; timeout enforced and job child killed | Partial | Allocation rejects unsafe paths and substitutions; editing remains path-based. Timeout logic exists, but the current test does not prove timeout or descendant termination. |
| No host process fallback on sandbox errors | Complete, scoped to `IsolatedWorktree::run_command` | Missing Bubblewrap returns an error; a dedicated regression passes. This does not mean Git, reviewer or other helpers run inside a sandbox. |
| Production repository and `.git` unaffected by failed/compromised job | Partial | Normal independent cloning and fresh-allocation failure cleanup are verified. Compromised-job behavior, subsequent filesystem operations and publication are not established by those tests. |

Completed subrequirements, without closing the broader combined criteria:

- [x] Job IDs, base privacy/ownership and repository overlap validated before allocation.
- [x] Destinations created exclusively; existing directories/symlinks left untouched.
- [x] Descriptor-pinned cleanup refuses substituted entries in regression tests.
- [x] Normal repository clones use independent Git metadata, pin commits and retain on request.
- [x] Invalid commits/non-repositories fail closed; fresh allocations are cleaned without an empty-repository fallback.
- [x] Missing Bubblewrap refuses command execution without a native command fallback.

The original PR #92 finding is historical: native `run_command` execution, an
unused timeout, shared coding-workspace Git metadata and the empty-repository
fallback have been superseded. Host Git operations and path-based writes remain
relevant; static symlink checks do not settle filesystem race safety.

### #43 — Safe command service

| Original acceptance criterion | Status | Evidence / outstanding requirement |
| --- | --- | --- |
| Utilities, tests and failing arbitrary commands work without production effects | Partial | Service and typed adapter exist; correctly provisioned successful execution and the required workload matrix remain unverified. |
| Overwrite/remove attempts against production assets denied or isolated | Open | Need disposable sentinel files and before/after hashes through the actual service. |
| Isolated shell, network off, quotas and timeout/cancel termination | Partial | Bubblewrap/cgroup/rlimit/cancellation code exists; actual network, resource and descendant-termination tests remain necessary. Forge uses a separate, weaker path. |
| User-inspectable changes and exact, single-use export approval | Partial | Guardian has bound approval-token tests; those are not an implemented and verified artifact-export workflow. |
| Hardening tests run in CI and on target host | Partial | Rust CI invokes workspace tests, but dedicated-service provisioning and the complete threat matrix on a target host are not evidenced. |
| Reboot/crash cleans abandoned sandboxes and reconciles audit logs | Partial | Startup cgroup cleanup exists; crash/reboot behavior and audit reconciliation still require evidence. |
| No unrestricted remote shell endpoint | Partial | Registered `command:execute` uses scheduler authorization and separate-service dispatch, with no host fallback in that adapter. Full authenticated Web/Android route and bypass testing is still outstanding. |

The dedicated-UID service, bounded command plans, request-digest binding,
namespace/cgroup configuration and refusal behavior are implemented mechanisms.
Keep deployment-dependent guarantees explicitly **implemented, not verified end
to end**. The original requirements remain the target, not claims of completion.

## Targeted verification and limits

The following 31 checks passed in the audit workspace. Commands use the lockfile;
Forge integration checks use a fresh private `SAIL_FORGE_WORKTREE_ROOT`.

| Command | Passed | What it establishes |
| --- | ---: | --- |
| `cargo test -p sail-guardian --test execution_boundaries --test nono_integration_test --locked` | 18 | 12 descriptor/approval/vault boundary regressions and six support/policy tests; no nono-confined child execution. |
| `cargo test -p sail-runner --test refusal --locked` | 3 | Invalid plans rejected, request binding changes with arguments/budgets/correlation, same-UID runner refused before host effects. |
| `cargo test -p sail-forge --lib allocation_tests --locked` | 8 | Traversal/root checks, exclusive/concurrent allocation, cleanup identity, failed setup, normal clone/retention. |
| `cargo test -p sail-forge --test worktree_isolation_test test_missing_bwrap_fails_closed_with_error --locked` | 1 | Missing sandbox executable refuses execution. |
| `cargo test -p sail-forge --test worktree_isolation_test test_standalone_git_clone_does_not_link_to_production_git --locked` | 1 | Normal clone has independent Git metadata and no source agent branch. |

The [Forge timeout test][timeout-test] accepts a fast successful result or certain
errors. It does not require an actual timeout, launch a known long-lived process
with descendants, or check that descendants disappear. Do not mark termination
acceptance complete from that test.

An earlier full Forge run in this session had 64 passes and 13 failures. Those
same failures reproduced on unchanged main; Bubblewrap reported that
non-privileged namespace creation was unavailable, followed by shared-test-lock
poisoning. That run is historical context, not a full-suite rerun for this
documentation change. No dedicated-UID service, privileged host setup, destructive
production probe, real reviewer dispatch or deployed-host security test was run.

## Update 2026-11-03: host Git and sandbox outputs

The 2026-11-02 inventory treated the sandbox and the host-side helpers as separate
rows. Their combination was exploitable. Bubblewrap bound the whole worktree
writable, `.git` included, and Forge then ran host `git diff` there after every
sandboxed `cargo test` (and `git apply` for model edits). A build script could
append `core.fsmonitor` or `diff.external` to `.git/config`. Host Git then ran that
command as the SAIL user, outside every sandbox. Reproduced on this host through
`IsolatedWorktree::run_command` and `get_diff`; the test below failed on `main`
with both markers written.

Related paths found in the same review:

- `resolve_safe_path` did not refuse `.git`. The model's host-side edit tools
  (`write_file`, the `apply_patch` fallback) could write `.git/config` directly,
  or through an in-worktree symlink such as `alias -> .git`.
- A repository the build creates at a submodule path makes `git diff` run
  `git status` inside it. That status applies the nested repository's own clean
  filter on the host. `-c` overrides on the outer Git do not reach this.
- Forge read sandbox outputs (JUnit report, symbol index, mutation lists) and
  cleared old ones with symlink-following calls. A planted symlink could put a
  host file into model context or reports, or delete host data.

Fixes, each covered by `crates/sail-forge/tests/sandbox_adversarial_test.rs`:

| Defense | Where |
| --- | --- |
| `.git` is a read-only bind mount inside the sandbox. As a mount point it cannot be renamed or replaced, and hardlinks across it fail. | `IsolatedWorktree::run_command` |
| Host Git refuses to run if `.git/config` differs from setup (`GitConfigTampered`), and runs with fsmonitor, hooks, pager and submodule recursion off; `get_diff` also passes `--no-ext-diff --no-textconv --ignore-submodules=all`. | `IsolatedWorktree::host_git`, `get_diff`, `apply_patch` |
| Host-side edits refuse any `.git` component in any case, and any path that resolves into `.git` (`GitMetadataPath`). | `resolve_safe_path` |
| Sandbox outputs are read and removed only through `read_output`/`remove_output`, which refuse symlinks out of the worktree. | `solver.rs`, `mutation.rs`, `symbol_index.rs` |

Each layer was checked by removing it: without the read-only mount the config
check fails closed; without `--ignore-submodules` the nested filter runs on the
host and the test fails.

| Command | Passed | What it establishes |
| --- | ---: | --- |
| `cargo test -p sail-forge --test sandbox_adversarial_test --locked` | 7 | Through the real `run_command` path on this host (Ubuntu 26.04, Bubblewrap 0.11.1): a build script cannot read Cargo credentials or the source repository, write the source repository or its `.git`, reach host loopback or the internet, outlive a timeout or a finished command, exceed the file-size limit, plant Git commands that run on the host, steer host edits into `.git`, or redirect output reads/removals out of the worktree. |
| `cargo test -p sail-forge --locked` | 181 | No regressions in the rest of Forge (unit, integration and doc tests). |

Not covered by this update: the dedicated `sail-runner` service (#43 rows above
are unchanged), host Git in the publisher (it applies only patch text in a fresh
checkout of the production repository), the reviewer's Codex process, and the
benchmark grading helper.

## Remaining work in the existing issues

- [ ] **#43: Apply nono enforcement and report it truthfully.** Apply constructed rules in a disposable worker; distinguish detected support, configured mechanisms and successfully verified enforcement. Test permitted operations and denied filesystem/network/process access without restricting the daemon itself.
- [ ] **#94: Integrate Forge with the dedicated runner.** Define confined source/artifact exchange, vetted offline dependencies and mounts; enforce resource limits and active-job cancellation without host fallback or broad credential mounts.
- [ ] **#94: Use descriptor-bound editing.** Cover reads, writes, patch application and cleanup across symlink/hardlink/mount substitution and races; preserve existing files on denial.
- [ ] **#43/#94: Separate trusted host operations.** Host Git on coding worktrees and host reads of sandbox outputs are done (update above); the rest remains. Review Git configuration/hooks/environment, reviewer credentials and execution, publisher allocation/shared metadata, and exact artifact-export approval. Restrict benchmark grading before admitting untrusted source.
- [ ] **#43/#94: Verify actual workers.** On a disposable provisioned host, run allowed/failing commands plus production sentinel hashes, malicious build scripts, credential reads, network/DNS, output flood, resource exhaustion, cancellation/descendants, interrupted execution, restart cleanup and audit reconciliation. Require the relevant CI evidence; unavailable infrastructure is not a pass.

Suggested next step: apply the existing nono capability sets inside a disposable
worker and verify both allowed and denied operations before reporting active
protection. UI mapping: no new UI; track truthful protection status and actionable
denials through the existing security status and Forge Activity surfaces.

[runtime]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-guardian/src/runtime.rs#L498
[runner]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-runner/src/lib.rs#L30
[protocol]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-protocol/src/execution.rs#L1
[service]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/deploy/runner/sail-runner.service#L1
[adapter]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-dialogue/src/execution.rs#L48
[scheduler]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-agents/src/tools.rs#L642
[forge-command]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-forge/src/worktree.rs#L598
[forge-files]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-forge/src/worktree.rs#L302
[forge-patch]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-forge/src/worktree.rs#L383
[allocation]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-forge/src/worktree.rs#L102
[cleanup]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-forge/src/worktree.rs#L170
[clone]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-forge/src/worktree.rs#L854
[reviewer]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-forge/src/reviewer.rs#L603
[publisher]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-forge/src/publish.rs#L80
[benchmark]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-forge/src/model_bench.rs#L221
[status]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-guardian/src/runtime.rs#L241
[status-http]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-dialogue/src/approval_http.rs#L52
[nono-tests]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-guardian/tests/nono_integration_test.rs#L34
[timeout-test]: https://github.com/PedroVictorCoding/SAIL/blob/215e177a028caa957ca3d1178a0105c44c7793b2/crates/sail-forge/tests/worktree_isolation_test.rs#L113
