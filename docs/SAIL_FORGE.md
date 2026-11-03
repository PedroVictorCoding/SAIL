# SAIL FORGE — Product and Technical Specification

**Status:** Proposed / evolving — verify implementation against code · **Domain:** Agency & Creation · **Roadmap issue:** [#27](https://github.com/PedroVictorCoding/SAIL/issues/27) · **Priority:** P2 · **Platforms:** Ubuntu server, Dioxus Web + Android

## 1. Mission
Resolve authorized coding issues through local-model patches in isolated worktrees with real evaluation.

## 2. Functional scope
- Issue intake.
- repo context retrieval.
- local coding loop #90/#93.
- worktree isolation #94.
- real tests.
- durable queue #95.
- single Codex subscription review per issue #96.
- draft PR..

## 3. Ownership and non-goals
Cannot silently change production checkout or merge; Guardian owns sandbox authority, Mind owns model backend and Agents owns generic orchestration.

Avoid duplicating persistent state, permissions, agent orchestration, model hosting or navigation owned by another subsystem. This is a feature specification, not evidence of completed implementation.

## 4. Interfaces and dependency contracts
Guardian, Mind, Core, GitHub broker, Continuity #61, model benchmark #60.

- Reuse SAIL Core typed events, versioned Rust request/response contracts, cancellation and lifecycle APIs. Do not invent an unrestricted script or shell protocol.
- Every tool-mediated effect carries authenticated session/task identity, bounded input/output, Guardian capability scope, audit correlation ID and explicit success/failure semantics.
- Handle partial failures, reconnects, cancellation and restarts without synthesizing successful outputs. Prefer local-first operation and persist only consented, minimized state.

## 5. UI and interaction mapping
Existing Forge task card and enqueue, progress, diff, test logs, review and blocked reasons.

Use the existing SAIL design: compact dark UI, contextual cards, ambient Orb, progressive disclosure, accessible keyboard/touch controls, and no permanent new tab unless justified by repeated user use. Show honest loading, empty, unavailable, permission-denied and reconnection states. Web and Android share one typed state model.

## 6. Security, privacy and resource limits
- All privileged reads, writes, network calls, credential use, microphone/camera acquisition or subprocess execution must pass Guardian's authoritative policy broker; enforce at an OS/adapter boundary rather than trusting the model's prompt.
- Do not make hidden cloud inference calls. External service access, when explicitly required, uses a scoped Bridge connection and is visible to the user.
- Target the primary host (i5-10400, 32 GB RAM, RX 5500) with measured CPU/RAM/VRAM/disk budgets; protect real-time voice responsiveness and bound expensive work.
- Support explicit cancellation, resource exhaustion errors, versioned schema evolution, event provenance and least-privilege access. Do not hardcode secrets or bypass approval on retry.

## 7. MVP vertical slice
Solve issue #125 using real model output, prove non-empty functional change and test/inspect diff without fabricated success.

### Acceptance checklist
- [ ] Confirm existing source files, interfaces and dependency versions before changing architecture.
- [ ] Implement the minimal real end-to-end flow above, without mocks presented as production behavior.
- [ ] Test success, offline/unavailable, permission denial, cancellation and restart/reconnection as relevant.
- [ ] Record measured latency and resource use where relevant; avoid fabricated benchmarks or status.
- [ ] Show user-visible success/error through the mapped existing UI and preserve user control.
- [ ] Submit focused tests and a reviewable PR without automatic merge.

## 8. Autonomous overnight operation

Implemented in `crates/sail-forge/src/{admission,gates,self_review,github,publish,nightly}.rs`.

**Pipeline per issue:**
1. Admission: the issue is labelled `forge-ready`, has an `## Acceptance` checklist, has no blocking label, and no open PR works on it.
2. Local solve in an isolated worktree, then `cargo test`.
3. Deterministic gates: diff size, protected paths, `## Target files` scope, test weakening, new `.unwrap()`.
4. Local self-review of a review packet capped at about 8k tokens.
5. Optional external review, only when a provider is installed.
6. Draft PR on `agent/forge-issue-N`.

If a gate or the self-review rejects the patch, the reason is appended to the issue body as `## Previous attempt feedback` and the issue is retried, up to `SAIL_FORGE_MAX_ATTEMPTS`. Forge never merges, and it never modifies the user's checkout or local branches. Publishing uses a throwaway detached worktree based on `origin/main`.

**Enable in the daemon** (`sail-core`), with defaults shown:

| Variable | Default | Meaning |
|---|---|---|
| `SAIL_FORGE_NIGHTLY` | unset | `1` starts the scheduler (wakes every 10 min, runs once per night) |
| `SAIL_FORGE_REPO` | `PedroVictorCoding/SAIL` | GitHub slug; must be in the orchestrator allowlist |
| `SAIL_FORGE_WINDOW` | `1-7` | Local hours `start-end`, wraps past midnight |
| `SAIL_FORGE_MAX_ISSUES` | `5` | Issues per run |
| `SAIL_FORGE_MAX_ATTEMPTS` | `2` | Attempts per issue, across runs |
| `SAIL_FORGE_MAX_PRS_PER_DAY` | `3` | Verified patches beyond this wait for the next run |
| `SAIL_FORGE_PUBLISH` | `1` | `0` keeps verified patches local (digest only) |
| `SAIL_FORGE_STATE_DIR` | `~/.local/state/sail/forge` | State, digests, publish scratch |

**Operate:**
- Kill-switch: `touch $SAIL_FORGE_STATE_DIR/PAUSE` stops new work at the next issue boundary. Remove the file to resume.
- Morning digest: `$SAIL_FORGE_STATE_DIR/digests/YYYY-MM-DD.md` (worked, blocked and not-admitted issues, with reasons).
- Manual pass: `cargo run -p sail-forge --bin sail_forge_nightly -- --once [--dry-run]`. A dry run only reads GitHub and writes nothing.
- Retry an exhausted issue: delete its entry from `nightly_state.json`.
- Labels Forge expects to exist on GitHub: `forge-ready`, `forge-generated`, and optionally `blocked`, `needs-decision`, `needs-decomposition`, `forge-failed`.

**Known limits:**
- Gate heuristics work on the diff text.
- The self-review uses the same local model as the solver.
- A job interrupted by a daemon restart is resumed by checkpoint recovery, but it is only published if it completes during a later nightly run.

### Failure memory (#121)

Every finished job appends one record to `$SAIL_FORGE_STATE_DIR/failure_memory.jsonl` (`crates/sail-forge/src/failure_memory.rs`). A record holds:
- the canonical issue, base commit, job, model and files changed;
- the outcome: verified, or the first failing stage (compile, tests, gate, mutation, review, no patch);
- normalized signatures (`rustc:E0308`, `test:<name>`, `gate:new_unwrap`, `mutation:missed`, `review:rejected`);
- one short failure line.

It holds no source code, issue text or model transcript. Cancelled jobs and infrastructure failures (model down, timeouts) are not recorded.

Before the solver starts, it gets a `## Forge failure memory` section, at most 1500 characters:
- earlier failed attempts on the same issue;
- signatures that recur (2 or more attempts) in the issue's `## Target files`;
- verified fixes that changed those files.

The section goes to the model only; it is not saved into the issue body. Records that are invalidated are never shown.

Manage records with `cargo run -p sail-forge --bin sail_forge_memory -- list [--issue owner/repo#N] | export | invalidate <id|issue> <reason> | delete <id|issue>`. Use `invalidate` when a "verified" fix was later reverted. Deleting records never touches Git. Set `SAIL_FORGE_FAILURE_MEMORY=0` to turn it off in the daemon and the nightly runner.

### Solver: edits, context and measurement

**Edits.** The solver prefers Aider-style SEARCH/REPLACE blocks (`crates/sail-forge/src/edit.rs`): a path line, then `<<<<<<< SEARCH` / `=======` / `>>>>>>> REPLACE`.
- Matching tries an exact unique match, then ignores trailing whitespace, then ignores indentation (and re-indents the replacement).
- All blocks in one reply apply together or not at all.
- When an edit fails, the model is told how many places matched, or is shown the closest lines.
- `repo:edit` (JSON) and unified diffs are still accepted.

**Context.** The first prompt contains:
1. the workspace crate list
2. the numbered contents of the issue's `## Target files` (8k chars)
3. a ranked repo map of item signatures (`repo_map.rs`, 4k chars), with targets pinned first and the rest ranked by how well their identifiers match the issue text

`repo:map {"query": ...}` re-ranks on demand. The history budget is 36k chars, sized for the 16k-token llama-server context.

**Measurement.** `cargo run -p sail-forge --bin sail_forge_eval -- --label <name> [--only 201,202] [--compare <prev.json>]` runs the real solver on the validated cases in `eval_cases.rs` and grades each with hidden tests the solver never sees.
- Results go to `$SAIL_FORGE_STATE_DIR/eval/<label>.{json,md}`.
- Recorded per case: solve rate, false success (the solver claimed success but the hidden tests fail), edit failures, unparseable replies, turns and time.
- A test enforces that every case fails unmodified and passes with its reference solution. The older `BenchmarkHarness` only grades reference patches; 6 of its 8 fixtures pass unmodified, so it does not measure the model.
- If the model server is unreachable, the run stops and reports the case as `n/a`. It never reports a score in that case.

## 9. Next steps
1. Audit the relevant implementation and the linked roadmap issue; update this spec with confirmed module/file paths and contracts.
2. Build the MVP vertical slice with real tool/model/provider integrations.
3. Add security and regression tests before expanding capability breadth.
4. Record known limitations, operational instructions and migration compatibility.

## 10. Related references
- [Roadmap and subsystem taxonomy](https://github.com/PedroVictorCoding/SAIL/issues/15)
- [Documentation index](README.md)
- [SAIL Studio](SAIL_STUDIO.md) for persistent editable artifacts (distinct from Canvas)

## Workspace allocation safety

Forge uses `SAIL_FORGE_WORKTREE_ROOT`, defaulting to `/tmp/sail_forge_worktrees`.
The base must be an absolute path without symlink components, owned by the Forge process
user, with no group/world permissions (normally `0700`). Its parent must already
exist. Forge creates a missing base privately but refuses unsafe existing bases;
it never changes their permissions automatically. If an older installation has
an unsafe base, stop Forge and inspect its contents before explicitly repairing
ownership/permissions or configuring a fresh private directory.

The base cannot contain or be contained by the source repository. Job IDs accept
1–128 ASCII letters, digits, underscores or hyphens. Each destination is allocated
exclusively: an existing directory or symlink is an error, never cleared for reuse.
Cleanup uses pinned descriptors and verifies directory identity; substituted
entries are left untouched with a warning. Retained jobs require manual inspection
and removal before their ID can be reused. Linux `openat2` support is required;
unsupported confinement fails closed. This allocation fix addresses part of #94;
command-runner integration and remaining execution isolation are separate work.

### Durable external-review accounting (#96)

Review dispatch reloads the JSON ledger under an exclusive Linux `flock` on a
stable `.lock` sidecar. Every broker using the same ledger path shares lifetime
issue reservations and daily/weekly request caps. A private temporary file,
file sync, atomic rename and directory sync commit the reservation before the
reviewer can run. Completion merges into the latest ledger, preserving other
issues' reservations. Ambiguous crashes leave the issue consumed; they never
trigger an automatic second review.

Corrupt, unreadable or unwritable accounting storage, symlinked ledger files and
lock contention produce an inconclusive `review_ledger_unavailable` result with
no invocation. A failed completion write retains the durable consumed slot and
logs the persistence failure. Existing ledger JSON remains compatible;
`persist_ledger`/`persist_ledger_sync` now return I/O errors to their trusted
callers. These snapshot APIs are administrative operations, not review dispatch.

Use one stable absolute ledger path on a local filesystem supporting `flock`,
atomic rename and `fsync`, in a trusted service-owned directory. Do not remove the
ledger/sidecar or run older daemons concurrently: older versions do not honor the
lock protocol. Back up both accounting state and configuration. This change does
not complete #96's remaining canonical repository identity, CLI authentication,
read-only review isolation, capacity detection or verdict-validation requirements.
No UI additions are required; denied reviews use the existing review result pane.

### Local model request deadline

Forge allows up to 600 seconds per model request, including queueing, prompt
processing, and streamed generation. Set `SAIL_FORGE_MODEL_TIMEOUT_SECS` before
starting SAIL to override this deadline (1–3600 seconds; missing or invalid values
use 600). The connection establishment timeout is separately limited to 5 seconds.

A reachable single-slot CPU inference server can exceed the old 90-second limit
while another turn is generating. A healthy `/health` response does not mean a
model slot is immediately free. Forge now distinguishes timeout, connection, and
other transport failures. Operator cancellation is checked while waiting for
headers and response bodies. An interrupted or incomplete stream is discarded
instead of being applied as a complete patch; successful streams must include a
completion marker or finish reason. This does not add inference admission control
or guarantee that a long-running model request will complete within the deadline.
