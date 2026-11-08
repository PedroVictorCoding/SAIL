# SAIL FLOW — Product and Technical Specification

**Status:** Partial — native manual text-DAG editor and durable runner; broader automation remains proposed · **Domain:** Agency & Creation · **Roadmap issue:** [#8](https://github.com/PedroVictorCoding/SAIL/issues/8) · **Priority:** P2 · **Platforms:** Ubuntu server, Dioxus Web + Android

## 1. Mission
Run reproducible permissioned DAG workflows and reusable Skillsmith automation.

## 2. Functional scope
- Typed DAG nodes/edges.
- trigger/schedule.
- deterministic no-LLM steps.
- Skillsmith #62.
- Instinct #66.
- Flow Optimizer #101.
- retries/rollback..

## 3. Ownership and non-goals
Not a novel general planner or security authority; Agents handles novel decisions, Guardian enforces capabilities.

Avoid duplicating persistent state, permissions, agent orchestration, model hosting or navigation owned by another subsystem. This is a feature specification, not evidence of completed implementation.

## 4. Interfaces and dependency contracts
Agents, Bridge, Pulse, Canvas, Guardian, Continuity #61.

- Reuse SAIL Core typed events, versioned Rust request/response contracts, cancellation and lifecycle APIs. Do not invent an unrestricted script or shell protocol.
- Every tool-mediated effect carries authenticated session/task identity, bounded input/output, Guardian capability scope, audit correlation ID and explicit success/failure semantics.
- Handle partial failures, reconnects, cancellation and restarts without synthesizing successful outputs. Prefer local-first operation and persist only consented, minimized state.

## 5. UI and interaction mapping
Existing Flow editor with graph nodes and contextual runtime logs; small-screen simplified task list.

Use the existing SAIL design: compact dark UI, contextual cards, ambient Orb, progressive disclosure, accessible keyboard/touch controls, and no permanent new tab unless justified by repeated user use. Show honest loading, empty, unavailable, permission-denied and reconnection states. Web and Android share one typed state model.

## 6. Security, privacy and resource limits
- All privileged reads, writes, network calls, credential use, microphone/camera acquisition or subprocess execution must pass Guardian's authoritative policy broker; enforce at an OS/adapter boundary rather than trusting the model's prompt.
- Do not make hidden cloud inference calls. External service access, when explicitly required, uses a scoped Bridge connection and is visible to the user.
- Target the primary host (i5-10400, 32 GB RAM, RX 5500) with measured CPU/RAM/VRAM/disk budgets; protect real-time voice responsiveness and bound expensive work.
- Support explicit cancellation, resource exhaustion errors, versioned schema evolution, event provenance and least-privilege access. Do not hardcode secrets or bypass approval on retry.

## 7. MVP vertical slice
Build a three-step local DAG, execute it twice deterministically and resume safely after interruption.

### Acceptance checklist
- [ ] Confirm existing source files, interfaces and dependency versions before changing architecture.
- [ ] Implement the minimal real end-to-end flow above, without mocks presented as production behavior.
- [ ] Test success, offline/unavailable, permission denial, cancellation and restart/reconnection as relevant.
- [ ] Record measured latency and resource use where relevant; avoid fabricated benchmarks or status.
- [ ] Show user-visible success/error through the mapped existing UI and preserve user control.
- [ ] Submit focused tests and a reviewable PR without automatic merge.

## 8. Next steps
1. Audit the relevant implementation and the linked roadmap issue; update this spec with confirmed module/file paths and contracts.
2. Build the MVP vertical slice with real tool/model/provider integrations.
3. Add security and regression tests before expanding capability breadth.
4. Record known limitations, operational instructions and migration compatibility.

## 9. Related references
- [Roadmap and subsystem taxonomy](https://github.com/PedroVictorCoding/SAIL/issues/15)
- [Documentation index](README.md)
- [SAIL Studio](SAIL_STUDIO.md) for persistent editable artifacts (distinct from Canvas)


## 10. Native workflow studio — first executable slice

Issues [#8](https://github.com/PedroVictorCoding/SAIL/issues/8) and
[#62](https://github.com/PedroVictorCoding/SAIL/issues/62) establish an n8n-style **SAIL-native**
workflow layer. n8n itself is an optional adapter, not a required dependency. No n8n
code or packages are included in this slice. #66 (Instinct), #80 (compute scheduling),
#84 (Friction Detector) and #101 (Optimizer) remain follow-on integrations.

### Acceptance contract and current implementation

- User opens **Flow**, edits a graph, connects a node's input and true/false branch,
  saves an immutable version, supplies text, runs that saved version and inspects actual
  node receipts. Pulse's reminders stay under **Reminders**; Flow never redirects to Pulse.
- Flow owns validation, topological execution and checkpoint persistence in
  `crates/sail-flow/src/workflows.rs`. `sail-protocol::flow` owns version-1 request schemas.
  All ports carry UTF-8 text; a Contains node additionally emits a boolean routing decision.
  The browser uses the existing owner authority through `/api/flow`. No model is needed.
- The graph supports one manual trigger, Trim, Uppercase, Prefix, Contains and terminal
  Result nodes, fan-out and conditional branch skipping. It has no arbitrary code, tool,
  filesystem, network, credential or inference node. Those effects require future typed
  adapters and Guardian enforcement; an owner token does not grant arbitrary tool access.
- Each run snapshots its workflow version, input, timestamps, per-node outputs and skip
  decisions. A client-supplied UUID deduplicates retries; conflicting reuse returns 409.
  Saving checks the expected revision and rejects stale updates. Terminal results are
  `completed`, `failed` or `cancelled`; skipped branches never claim node execution.
- Accepted runs execute on the server independently of the browser. Each node commits
  an atomic fsynced checkpoint. On server restart, running executions become `paused`;
  the owner can resume without repeating committed pure nodes or cancel. A crash before
  a checkpoint can repeat a pure transform. This is not an external-effect exactly-once claim.
- State lives at `SAIL_DATA/flow/workflows-v1.json` through the shared data-root resolver,
  using private files and a bounded snapshot. There are at most 128 stored workflow
  revisions and 64 executions, 32 nodes per graph, 4096 input bytes, 1024 configuration
  bytes per text node and 8192 output bytes per node. Excess is rejected explicitly.
  The owner can delete terminal execution receipts and delete workflows (all versions)
  once active runs are cancelled. Execution snapshots remain until separately deleted.
- The interface has a desktop connected-node graph, keyboard-operable node settings,
  a stacked phone view, loading/unavailable/error feedback and server-derived results.
  Text/code surfaces remain opaque and static under DESIGN.md.

### API and errors

All routes below require `Authorization: Bearer <owner token>`, have a 64KB request-body
limit and return `Cache-Control: no-store`:

| Method and path | Contract |
| --- | --- |
| `GET /api/flow/workflows` | Latest saved workflows and bounded execution history |
| `POST /api/flow/workflows` | `SaveWorkflow {expected_revision, workflow}` |
| `POST /api/flow/runs` | `StartRun {id, workflow_id, revision, input}` |
| `POST /api/flow/runs/:id/cancel` | Cancel between pure node checkpoints |
| `POST /api/flow/runs/:id/resume` | Resume a restart-paused execution |
| `POST /api/flow/runs/:id/delete` | Delete a terminal execution and receipts |
| `POST /api/flow/workflows/:id/delete` | Delete all versions after cancelling active runs |

Errors return `{error: string}`: 400 invalid graph/version/limits on individual inputs,
401 unauthorized, 404 missing workflow/run, 409 stale revision or invalid lifecycle,
429 store capacity, 503 unavailable storage/authority. Malformed JSON and oversized
bodies use Axum's extractor errors. A storage failure stops execution; restart recovery
requires explicit resume. One server process owns this store; multi-process writers
and distributed execution are unsupported.

### Verification and local demo

```sh
cargo test -p sail-flow --test workflows_test
cargo test -p sail-dialogue flow_http::tests --lib
# Run with a temporary SAIL_DATA directory and a chosen 32+ character
# SAIL_FLOW_DEMO_TOKEN. The example listens only on loopback, port 18808.
cargo run -p sail-dialogue --example flow_studio
# Enter the same owner token in Guardian settings, then open /#flow.
# With the demo running, exercise real API + UI:
.venv-voice/bin/python scripts/ui/flow_browser_test.py /tmp/flow-shots
```

The example serves the real Flow API and compiled UI assets without loading models;
other SAIL services are intentionally unavailable. The browser test takes the same token
from `SAIL_FLOW_DEMO_TOKEN` and checks save/run/reload, branching, route separation,
mobile layout and errors. Do not use the example as a production server.

Not delivered by this slice: automatic schedules/webhooks, typed connector nodes,
approval gates, retries/compensation for external effects, Canvas publication,
Skillsmith synthesis, Instinct triggering, performance optimization or n8n
import/export. The parent epics remain open. The future executor should reuse
Continuity/Guardian contracts before admitting external effects; this bounded
checkpoint runner does not replace their authority or general task persistence.

Browser verification captures (isolated real Flow API; the other services are offline):
[desktop](design/flow-studio/desktop.png), [phone](design/flow-studio/mobile.png).
