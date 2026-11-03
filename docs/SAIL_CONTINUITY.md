# SAIL Continuity — durable background tasks

**Status:** first vertical slice implemented (Forge is the first consumer) · **Owner:** Core shared foundation · **Issues:** [#61](https://github.com/PedroVictorCoding/SAIL/issues/61), [#95](https://github.com/PedroVictorCoding/SAIL/issues/95) · **Vision:** [§1.3, §2.7, §12, invariant 5](SAIL_MASTER_VISION.md)

Continuity is the server-owned record of accepted background work. A browser tab, a voice turn or a daemon process is a *projection* of that record, not its owner: closing the UI must not abort the task, and restarting the daemon must neither lose it nor replay an external effect that already happened.

Continuity is a library (`crates/sail-continuity`), not a new process, tab or service. Owner subsystems (Forge first; Flow, Discovery and Agents later) submit tasks to it, run them, and report progress through it. Continuity does not decide *what* a task does or whether it is authorized — Guardian still owns authority — it records *what state the work is really in*.

## 1. Acceptance contract for the first slice

**User action.** The user (by voice, the Forge panel, or the nightly runner) asks Forge to solve an issue, then closes the browser or the daemon restarts.

**Backend state transitions.**

```
            submit                claim                 finish
  (none) ─────────► queued ─────────────► running ─────────────► succeeded | failed | cancelled
                      ▲                      │
                      │ resolve(retry)       │ daemon restart (recover)
                      │                      ▼
             needs_intervention ◄──── interrupted / needs_intervention / failed
                                     (by resume policy and effect journal)
```

| State | Meaning |
|---|---|
| `queued` | Accepted and durable; no worker holds it yet (it may be waiting for the heavy-job slot). |
| `running` | A worker in **this** daemon boot holds a lease on it. |
| `interrupted` | A previous boot was running it; it is safe to restart and its owner will re-run it. |
| `needs_intervention` | It cannot continue without the user: its resume policy is manual, or an external effect's outcome is unknown. |
| `succeeded` / `failed` / `cancelled` | Terminal. |

**Resume policies** (declared by the owner at submit time):

- `restart` — work is local and idempotent (Forge's issue lifecycle in a private worktree). An interrupted task is re-run from its start.
- `abandon` — the caller has its own retry loop (Forge nightly jobs). An interrupted task fails with an explicit reason; the caller retries on its next pass.
- `manual` — never resumed automatically.

**Real side effects and the effect journal.** Before an external effect (for example pushing a branch and opening a draft PR), the owner calls `begin_effect(task, key)`. That durably records an *intent* tagged with the current boot. Afterwards it records `commit_effect` with a receipt (the PR URL) or `fail_effect`.

- Committed key → `begin_effect` returns the stored receipt; the effect is **not** repeated.
- Intent from an earlier boot with no receipt → the outcome is **in doubt**. Recovery moves the task to `needs_intervention`, naming the effect, and `begin_effect` refuses until the user resolves it. SAIL never guesses whether a push happened.

**Permissions.** No new authority. Continuity records are local SQLite rows; Forge's existing allowlist, sandbox and Guardian checks are unchanged.

**Persistence and recovery.** SQLite in WAL mode with `synchronous=FULL`. Each daemon start opens a new *boot*. `recover()` classifies every task left `running` by an older boot. Leases carry `(attempt, boot)`; a write from a stale lease is rejected, so a leftover worker cannot overwrite a newer attempt.

Default location, first match wins: `SAIL_CONTINUITY_DB`; `/srv/sail/state/continuity.db` when the runtime SSD is writable; `$XDG_DATA_HOME/sail/continuity.db` (or `~/.local/share/...`). Forge checkpoints moved from the system temp directory, which may be tmpfs, to a `forge-checkpoints` directory beside it; existing checkpoints are copied over once. If the store cannot be opened, SAIL runs with an in-memory store and logs an error; it does not pretend to be durable.

**Cancellation.** `request_cancel` cancels a queued, interrupted or `needs_intervention` task immediately. For a running task it sets a durable flag and signals the worker; if the daemon dies first, recovery finishes the task as `cancelled` rather than resuming it.

**Duplicate delivery.** Tasks may carry a dedupe key (Forge: `repo#issue`). While a task with that key is active, submitting again returns the existing task and dispatches no second worker. A key whose task needs intervention refuses new work until the user resolves it.

**Bounded history.** Each task keeps at most 200 events (detail truncated to 2,000 characters); checkpoints are limited to 64 KiB. Terminal tasks older than 30 days are pruned at boot. Continuity stores task metadata, not conversation text.

**UI feedback.** The existing Forge panel, which is refetched on every WebSocket reconnect, shows each job's task state in plain words alongside its reason. The labels are *queued*, *resuming after restart*, *needs your attention*, *failed* and *cancelled*. For example: **needs your attention** — *Needs you: 'push branch agent/forge-issue-12 and open a draft PR' was interrupted and may or may not have happened; check it before retrying*. There is no new tab.

**Workspaces on re-run.** A re-run after a restart uses a fresh private workspace (`<job>-a<attempt>`). The interrupted attempt's directory is left in place for inspection, not reused or deleted.

**Negative cases covered by tests.** Stale lease after restart, cancellation before and during a restart, duplicate enqueue, an effect in doubt after a crash, an already-committed effect not replayed, a missing checkpoint, and an oversized checkpoint.

**Verification commands.**

```
cargo test -p sail-continuity
cargo test -p sail-forge --test continuity_recovery_test
cargo test -p sail-forge --test nightly_runner_test
```

## 2. Explicitly out of scope for this slice

- Flow, Discovery, Agents and Dialogue tool calls adopting Continuity (each is a follow-up that reuses this API).
- A generic Tasks workspace in the UI; Forge's existing panel is the projection for now.
- Resuming Forge from the middle of its lifecycle (it restarts from the beginning in a fresh worktree). Fine-grained step checkpoints come later.
- Resource admission (#57, #80). Forge keeps its single heavy-job semaphore.
- The Codex reviewer ledger (it already has its own durable reservation, #96).
- Encryption at rest. Continuity holds no conversation content or credentials.

## 3. API summary (`sail_continuity::Continuity`)

| Call | Purpose |
|---|---|
| `open(path)` / `open_in_memory()` / `open_default()` | Open the store and start a new boot. |
| `recover()` | Classify tasks orphaned by earlier boots and return what to resume. |
| `submit(TaskSpec)` | Create a task, or return the active task with the same dedupe key. |
| `claim(id) -> Lease` | `queued`/`interrupted` → `running` for this boot. |
| `progress(&lease, stage, detail)` / `save_checkpoint(&lease, json)` | Record progress; rejected for stale leases. |
| `finish(&lease, Outcome)` | Move to a terminal state. |
| `begin_effect` / `commit_effect` / `fail_effect` | The effect journal described above. |
| `request_cancel(id, reason)` | Durable cancellation. |
| `resolve(id, Resolution)` | User decision on a `needs_intervention` task: retry (re-queue) or cancel. |
| `get`, `list`, `events` | Read the authoritative state for UI projection. |
