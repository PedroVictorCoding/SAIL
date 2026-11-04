# SAIL ARENA — Product and Technical Specification

**Status:** Proposed / evolving — verify implementation against code · Main product: [Arena Learn](SAIL_ARENA_LEARN.md) ([#215](https://github.com/PedroVictorCoding/SAIL/issues/215)) · Implemented memory architecture: [SAIL_COGNITIVE_MEMORY.md](SAIL_COGNITIVE_MEMORY.md) · **Domain:** Intelligence & Knowledge · **Roadmap issue:** [#5](https://github.com/PedroVictorCoding/SAIL/issues/5) · **Priority:** P1 · **Platforms:** Ubuntu server, Dioxus Web + Android

## 1. Mission
**Arena Learn is Arena's main product:** a local, clean-room Rust reimplementation of the LearnHarness learning loop (probe → dependency plan → teach/check → separate practice) that grows a persistent graph of propositions and reasoning chains. See [SAIL_ARENA_LEARN.md](SAIL_ARENA_LEARN.md).

Underneath it, Arena provides privacy-scoped, attributed long-term memory and a searchable knowledge graph. Arena Learn and the rest of SAIL build on that substrate.

## 2. Functional scope
- Arena Learn: learning sessions, lesson plans, questions, practice and the learning graph ([spec](SAIL_ARENA_LEARN.md)).
- SQLite WAL and typed blocks/relations.
- full-text plus local vector retrieval.
- provenance, deduplication and confidence provenance.
- Obsidian-style graph exploration.
- deletion/retention..

## 3. Ownership and non-goals
Not a social contact authority, truth oracle or substitute for document native storage; Friends owns relationship semantics and Studio owns canonical documents.

Avoid duplicating persistent state, permissions, agent orchestration, model hosting or navigation owned by another subsystem. This is a feature specification, not evidence of completed implementation.

## 4. Interfaces and dependency contracts
Core persistence, Mind embeddings, Dialogue, Friends, Lab, Cognitive Compiler #99, Evidence Ledger #100.

- Reuse SAIL Core typed events, versioned Rust request/response contracts, cancellation and lifecycle APIs. Do not invent an unrestricted script or shell protocol.
- Every tool-mediated effect carries authenticated session/task identity, bounded input/output, Guardian capability scope, audit correlation ID and explicit success/failure semantics.
- Handle partial failures, reconnects, cancellation and restarts without synthesizing successful outputs. Prefer local-first operation and persist only consented, minimized state.

## 5. UI and interaction mapping
The Arena page opens on Arena Learn; memory views (knowledge context, graph explorer) are a secondary section; explicit inspect, correct, forget and provenance UI.

Use the existing SAIL design: compact dark UI, contextual cards, ambient Orb, progressive disclosure, accessible keyboard/touch controls, and no permanent new tab unless justified by repeated user use. Show honest loading, empty, unavailable, permission-denied and reconnection states. Web and Android share one typed state model.

## 6. Security, privacy and resource limits
- All privileged reads, writes, network calls, credential use, microphone/camera acquisition or subprocess execution must pass Guardian's authoritative policy broker; enforce at an OS/adapter boundary rather than trusting the model's prompt.
- Do not make hidden cloud inference calls. External service access, when explicitly required, uses a scoped Bridge connection and is visible to the user.
- Target the primary host (i5-10400, 32 GB RAM, RX 5500) with measured CPU/RAM/VRAM/disk budgets; protect real-time voice responsiveness and bound expensive work.
- Support explicit cancellation, resource exhaustion errors, versioned schema evolution, event provenance and least-privilege access. Do not hardcode secrets or bypass approval on retry.

## 7. MVP vertical slice
Save a consenting conversation fact with origin, retrieve it by text query, display evidence and delete it completely from indexes.

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
