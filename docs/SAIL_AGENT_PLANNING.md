# Agent task planning

**Status:** usable from the web UI and voice (slices 1–2, §8–§9) · **Owner:** Agents ([#6](https://github.com/PedroVictorCoding/SAIL/issues/6), [#26](https://github.com/PedroVictorCoding/SAIL/issues/26)) ·
**Builds on:** tool registry and scheduler (`crates/sail-agents/src/tools.rs`, [#44](https://github.com/PedroVictorCoding/SAIL/issues/44)), [Continuity](SAIL_CONTINUITY.md), Guardian approvals ·
**Roadmap:** [A1](SAIL_ENGINEERING_ROADMAP.md#wave-1-agency-the-critical-path) · **Vision:** §1.4, §19 ("carry forward a multi-step goal")

The owner gives SAIL a goal by voice, web or chat. SAIL turns it into a plan the owner can
see and edit, runs the steps with tools and approvals, checks each result, re-plans when a
step fails, and keeps going across restarts and closed tabs. The owner can pause, edit,
cancel or approve at any point.

## 1. What exists and what is missing

`ToolRegistry` holds typed tool descriptors (schema, security effect, timeout, concurrency)
and `ToolScheduler::execute_plan` runs a dependency graph of up to 32 tool steps through
Guardian. Missing: a planner that turns a goal into steps; a durable plan; per-step
verification; re-planning; plan-level review and editing; and Dialogue's own 16 tools,
which are a hard-coded list in `vlm.rs` instead of registry entries.

## 2. Model

```
Goal ── planned by Mind ──> Plan (revision n)
                              ├─ Step: intent, tool + input (or "ask owner"), depends_on,
                              │        check, approval need, state, receipt
                              └─ ...
Plan state: draft → awaiting_approval → running ⇄ paused → succeeded | failed | cancelled
Step state: pending → ready → policy_pending → running → verifying → done | failed | skipped
```

- **Goal:** the owner's words, the origin (voice, web, chat) and the memory grant for the
  task's purpose.
- **Step:** one tool call from the registry. Its `check` says how success is judged:
  - **deterministic:** for example, the tool reported success, a file exists, a value
    is in range, a test passed;
  - **model check:** Mind is asked a yes or no question about the observation; used
    only when no deterministic check fits, and labeled as such.
  
  A step can instead be "ask the owner", which pauses the plan for a typed answer.
- **Plan revision:** each re-plan or owner edit creates revision `n+1`. Earlier revisions
  and their receipts are kept, so the history shows what changed and why.

## 3. Planning

1. **Context.** The Context Composer gives Mind the goal, the registry's tool descriptors
   (only those the purpose may use) and a bounded set of relevant memory under the
   task's grant.
2. **Draft.** Mind returns a plan as JSON. It is **validated deterministically** before
   anyone sees it:
   - every tool exists in the registry, and its input matches the tool's schema;
   - dependencies refer to steps in the plan and contain no cycles;
   - at most 32 steps, with bounded inputs;
   - no step uses a tool outside the purpose's grant.
   
   An invalid draft goes back to Mind once with the validation errors. If it fails
   again, planning fails and the owner is told why.
3. **Review.** The plan is shown with each step's intent, tool and approval need. Read-only
   plans start immediately. A plan with any effectful step waits for the owner to approve
   it as a whole. Per-step Guardian approvals still apply when those steps run. The owner
   can edit, remove or reorder steps before approving; the edit is a new revision and is
   validated again.

## 4. Execution

- **Durable.** The plan is a Continuity task. Each step boundary is a checkpoint: before a
  step (intent recorded), after it (receipt recorded). Steps with external effects use
  Continuity's effect gate. After a crash, a step whose effect may or may not have happened
  becomes "needs your attention", never a blind retry. Read-only steps are re-run.
- **Through the scheduler.** Each step runs via `ToolScheduler::execute`, so Guardian
  grants, approvals, timeouts, cancellation and audit are unchanged. Independent steps run
  in parallel, within the registry's concurrency limits.
- **Background work.** Plans are admitted by the resource scheduler and paused, not
  failed, by live voice. Forge's pause-and-resume behavior is used here too.
- **Verification.** After each step, its check runs. Only a passing check marks the step
  done. Results are typed observations and are treated as data, never as instructions.

## 5. Re-planning

When a step fails its check:

1. Retry it once if the failure is retryable and the step is read-only.
2. Otherwise ask Mind for a revised plan from the current state: the goal, completed steps
   with their receipts, the failed step and its error. Completed steps are kept; the
   revision replaces only what is not done.
3. Bound it: at most 3 re-plans per plan. After that, or if a revision is invalid, the plan
   stops as **failed** with the history.
4. With external models enabled (decision 12), the escalation policy applies here:
   re-planning moves to an external provider only after the local re-plans failed and
   only with quota left. That is shown in the plan history.
5. A revision that adds an effectful step the owner has not approved pauses for approval.

## 6. Interfaces

- **Voice and web:** "Plan: …" or a goal recognized as multi-step starts planning. The
  plan appears as a task card in the existing Activity and Canvas surfaces (no new tab).
  The card shows steps, states, receipts, the model that planned, and pause, edit,
  approve and cancel controls. Spoken progress stays short.
- **Chat (P2):** `/plan <goal>`, `/plans`, `/approve`, `/cancel <plan>`. Progress
  messages arrive through the outbox.
- **Scheduler (P3):** a scheduled job can run a goal or a saved plan.
- **Skills (P4):** a successful plan can be saved as a skill (staged for approval).

## 7. Safety

- The planner can propose only registry tools within the purpose's grant. It cannot
  grant itself permissions, and its output is validated before anything runs.
- The owner approves a plan with effectful steps as a whole. Guardian still checks every
  step at run time, with the same single-use, exact-request approvals as today.
- Tool output and web content are data. A step's observation cannot add steps; only a
  re-plan from Mind can, and it is validated and approval-gated like any plan.
- Every plan is bounded: steps, re-plans, run time and admitted resources.

## 8. First slice (acceptance)

A two-step goal with one effectful step, end to end:

1. The goal "write today's time to notes/time.txt" produces a validated plan:
   `local_time` → `fs_write` (effectful).
2. The plan waits for approval. After approval, step 1 runs and is verified; the plan
   checkpoints.
3. **The daemon restarts.** The plan resumes from the checkpoint without re-running
   completed effects. Step 2 asks for Guardian's approval, runs, and its check (the file
   exists with the expected content) passes.
4. Negative cases:
   - an unknown tool is rejected at validation;
   - a cycle is rejected at validation;
   - a denied step fails the plan with the denial shown;
   - a failing check triggers a bounded re-plan;
   - cancellation stops the plan and any running step.
5. Tests drive the planner with a scripted model and real tools in a temporary
   workspace. A separate test runs the real local model, labeled as such.

Out of scope for the first slice: the chat interface (P2), scheduled plans (P3), saving
plans as skills (P4), escalation to external models (P1), and moving all Dialogue tools
onto the registry (done incrementally after the slice).

## 9. Second slice: in the running app

- **One registry.** Dialogue's tools are registry entries (`crates/sail-dialogue/src/execution.rs`)
  with their real descriptions and schemas. The voice model's tool list is generated from
  the registry, so the model, the planner and the scheduler see the same tools and the
  same enforced schemas. `vision_frame` and `memory_recall` stay Dialogue-local: they act
  on the current turn's image and archive.
- **Started at boot.** `sail-core` builds the runner over the shared Continuity store,
  Dialogue's scheduler and the resident model, and resumes running plans after Forge's
  recovery pass. Plan steps run as the Agents actor, so Dialogue's "always allow" rules do
  not apply to them.
- **Voice and text.** "Plan: …" (also "Plan, …" as speech recognition writes it) drafts a
  plan instead of a conversational answer. SAIL says briefly what happens next and shows
  the plan card. "Plan a trip" without the separator stays conversation.
- **Plan card** on Canvas: steps with state, tool, "changes something" and errors; the
  history; approve, edit (the remaining steps as JSON, validated again) and cancel. A
  running step waiting for Guardian shows its approval on the card. A plan whose step was
  interrupted mid-effect shows "Needs your attention", with "It did not happen: retry"
  and "Cancel plan". Open and attention-needing plans reappear after a reload or restart.
- **Owner controls.** `/api/plans` (list, create, show, approve, edit, cancel, resolve)
  requires the owner token, like Guardian approvals. The voice socket can start a plan;
  only the owner token lets its effects run.
- **Priorities.** A new goal is drafted at interactive priority, since the owner is
  waiting; a re-plan during a run is background work that waits while live voice has
  priority instead of failing the plan.

Not yet done:
- **Guardian approvals expire after 60 seconds.** A step that waits longer fails and is
  re-planned, so an effectful plan still needs the owner nearby when its steps run.
- An interrupted effectful step needs the owner even when the interruption happened while
  it waited for Guardian, before anything ran.
- Spoken progress, chat (P2), scheduled plans (P3), and pausing a plan.
- The planner has not been measured on the resident model; tests and the live check used
  a scripted model.
