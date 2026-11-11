# SAIL VISION — Product and Technical Specification

**Status:** Proposed / evolving — verify implementation against code · **Domain:** Interaction & Presence · **Roadmap issue:** [#2](https://github.com/PedroVictorCoding/SAIL/issues/2) · **Priority:** P1 · **Platforms:** Ubuntu server, Dioxus Web + Android

## 1. Mission
Turn authorized camera, screenshot and document input into grounded multimodal observations.

## 2. Functional scope
- LFM3-Omni perception adapter.
- frame sampling and keyframe dedup.
- source timestamps.
- OCR/document parser #48.
- image location and uncertainty.
- annotated inspection..

## 3. Ownership and non-goals
Not a hidden surveillance camera or source of unquestionable facts; Bridge handles camera adapter and Guardian consent.

Avoid duplicating persistent state, permissions, agent orchestration, model hosting or navigation owned by another subsystem. This is a feature specification, not evidence of completed implementation.

## 4. Interfaces and dependency contracts
Mind, Dialogue, Bridge, Canvas, Arena, Guardian, OXR parser #48.

- Reuse SAIL Core typed events, versioned Rust request/response contracts, cancellation and lifecycle APIs. Do not invent an unrestricted script or shell protocol.
- Every tool-mediated effect carries authenticated session/task identity, bounded input/output, Guardian capability scope, audit correlation ID and explicit success/failure semantics.
- Handle partial failures, reconnects, cancellation and restarts without synthesizing successful outputs. Prefer local-first operation and persist only consented, minimized state.

## 5. UI and interaction mapping
Contextual image preview, analyze-image action and temporary observations; camera activity always apparent.

Use the existing SAIL design: compact dark UI, contextual cards, ambient Orb, progressive disclosure, accessible keyboard/touch controls, and no permanent new tab unless justified by repeated user use. Show honest loading, empty, unavailable, permission-denied and reconnection states. Web and Android share one typed state model.

## 6. Security, privacy and resource limits
- All privileged reads, writes, network calls, credential use, microphone/camera acquisition or subprocess execution must pass Guardian's authoritative policy broker; enforce at an OS/adapter boundary rather than trusting the model's prompt.
- Do not make hidden cloud inference calls. External service access, when explicitly required, uses a scoped Bridge connection and is visible to the user.
- Target the primary host (i5-10400, 32 GB RAM, RX 5500) with measured CPU/RAM/VRAM/disk budgets; protect real-time voice responsiveness and bound expensive work.
- Support explicit cancellation, resource exhaustion errors, versioned schema evolution, event provenance and least-privilege access. Do not hardcode secrets or bypass approval on retry.

## 6.1 Local image archive and recall (implemented slice)

Camera frames, screen captures and uploaded images are archived under the SAIL data root so
a later turn can still refer to them:

- `<data>/vision/archive.sqlite3` indexes the metadata.
- `<data>/vision/images/<id>.jpg|png` holds the bytes; `<id>.json` is a sidecar with the
  same metadata and labels.
- Each record stores `id`, `taken_at`, `conversation_id` (voice session), `source`
  (`camera`/`screen`/`upload`), `origin` (`snapshot`/`attached`), format, dimensions,
  caption (the user's text or the model's question), an optional local description and
  searchable labels. Labels are `source:`, `origin:`, `date:` plus lowercase words from the
  caption and description.
- The model calls `vision_photos` with `mode=recent` (default: this conversation, last 24
  hours), `mode=search` (labels/caption/description; `scope=all` for older conversations) or
  `mode=get` (one image by id). A vision-capable model receives the image itself; a
  text-only model receives the stored description, generated on demand by the local
  describer and persisted.
- Archiving is best effort: a storage failure is logged and a tool reports it, but it never
  blocks a turn. Files are written atomically with owner-only permissions and nothing in
  this path uses the network.

This slice lives in `crates/sail-dialogue/src/vision_archive.rs`; it should move to a
dedicated Vision crate when one exists. It has no automatic retention yet: the owner can
remove `<data>/vision` to clear it, and the directory is recreated on the next capture.

## 7. MVP vertical slice
Analyze a selected local image using real LFM3, expose model/time/source, recover from missing projector and never invent a result.

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
