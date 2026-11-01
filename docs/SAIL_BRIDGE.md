# SAIL BRIDGE — Product and Technical Specification

**Status:** Proposed / evolving — verify implementation against code · **Domain:** Systems & Security · **Roadmap issue:** [#7](https://github.com/PedroVictorCoding/SAIL/issues/7) · **Priority:** P2 · **Platforms:** Ubuntu server, Dioxus Web + Android

## 1. Mission
Offer typed secure adapters between SAIL and OS, browser, local devices and external services.

## 2. Functional scope
- Capability registry #24.
- tool APIs.
- safe browser research #40.
- camera/mic integrations.
- Home Assistant #82.
- scoped connection/session broker..

## 3. Ownership and non-goals
Does not own security policies or agent plans; Guardian approves effects and Agents plans actions; Relay owns cross-device UX.

Avoid duplicating persistent state, permissions, agent orchestration, model hosting or navigation owned by another subsystem. This is a feature specification, not evidence of completed implementation.

## 4. Interfaces and dependency contracts
Guardian, Agents, Core, Dialogue, Vision, Flow, Relay.

- Reuse SAIL Core typed events, versioned Rust request/response contracts, cancellation and lifecycle APIs. Do not invent an unrestricted script or shell protocol.
- Every tool-mediated effect carries authenticated session/task identity, bounded input/output, Guardian capability scope, audit correlation ID and explicit success/failure semantics.
- Handle partial failures, reconnects, cancellation and restarts without synthesizing successful outputs. Prefer local-first operation and persist only consented, minimized state.

## 5. UI and interaction mapping
Existing Local tools and Connections settings with operation-level approval states.

Use the existing SAIL design: compact dark UI, contextual cards, ambient Orb, progressive disclosure, accessible keyboard/touch controls, and no permanent new tab unless justified by repeated user use. Show honest loading, empty, unavailable, permission-denied and reconnection states. Web and Android share one typed state model.

## 6. Security, privacy and resource limits
- All privileged reads, writes, network calls, credential use, microphone/camera acquisition or subprocess execution must pass Guardian's authoritative policy broker; enforce at an OS/adapter boundary rather than trusting the model's prompt.
- Do not make hidden cloud inference calls. External service access, when explicitly required, uses a scoped Bridge connection and is visible to the user.
- Target the primary host (i5-10400, 32 GB RAM, RX 5500) with measured CPU/RAM/VRAM/disk budgets; protect real-time voice responsiveness and bound expensive work.
- Support explicit cancellation, resource exhaustion errors, versioned schema evolution, event provenance and least-privilege access. Do not hardcode secrets or bypass approval on retry.

## 7. MVP vertical slice
Call one local authorized tool and one separately authorized network adapter; demonstrate correct denial and credential isolation.

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
