# SAIL Studio — Product and Technical Specification

**Status:** Proposed · **Owner:** SAIL Studio (existing Spaces/Canvas/Mind/Guardian integration) · **Target:** Rust backend, Dioxus Web/Android, local-first

## 1. Vision

SAIL Studio is a persistent, AI-editable productivity workspace, **not a renamed Canvas and not merely an iframe directory**. Users and SAIL collaborate on real projects and editable files using third-party browser-capable creative/productivity editors, with the ArtCraft/Craft ecosystem as an initial candidate. An open editor and its native data model remain the source of truth; SAIL may inspect and apply **authorized, structured, reversible** edits. The user sees those changes in real time and can undo them.

**Core promise:** “Ask SAIL to change the document, watch the edit happen, keep control of the file.”

### Boundaries
- **Studio:** persistent files, real editors, project-based artifact collaboration.
- **Canvas:** ephemeral diagrams/previews and visual explanations; can send outputs to Studio.
- **Spaces:** groups projects, people and context; Studio projects may live inside Spaces.
- **Mind:** intent routing / structured edit planning; never directly manipulates DOM or permissions.
- **Guardian:** scope-based authorization and auditing.
- **Forge:** software repository edits, not a substitute for document editors.
- **Flow / Skillsmith:** reusable multi-artifact routines; all actions use the same Studio command interface.
- **Arena:** opt-in searchable project knowledge/provenance; not the canonical source of document state.

## 2. Product experience and navigation

One **Studio** entry in SAIL's existing left navigation. Preserve existing dark/minimal appearance, ambient Orb and contextual right-side conversation, without adding another chat view or full suite of navigation sections.

- Studio landing: recent projects, new project, open existing file, recently used editors.
- Project workspace: left file tree / asset list; center actual editor; right collapsible SAIL assistant. Editor gets maximum width; phone uses switchable editor/chat panels.
- Editor tabs and breadcrumbs: project → file → editor, unsaved indicator, save/error/conflict.
- “Ask SAIL about this” scoped to active selection, object, sheet/range/slide/page or whole document; explicitly show scope.
- SAIL edit states: **suggest** (preview before change), **assist** (live reversible edits), **autonomous** (bounded multi-step edits inside granted workspace).
- Every applied edit has a brief visible indication, undo affordance and linked action receipt. Avoid permission-modal spam for previously granted, low-risk workspace operations.
- Keyboard accessibility, responsive Android touch interactions, offline state, foreground/background lifecycle, loading and actionable failures.
- Never represent unsaved, unsupported, simulated or failed operations as completed.

## 3. Editor ecosystem

ArtCraft applications are **candidates** to audit and integrate, not assumed stable browser plugins. Investigate real repository names, licenses, maintenance, runtime dependencies, iframe/CSP constraints, native file formats and scripting/command APIs before selecting any.

Desired capabilities and rollout order:
1. **Documents / word processing**: initial editor and full vertical slice (WordCraft *if verified suitable*).
2. **Spreadsheets and presentations**: chart/formula/range/slide operations (GridCraft / DeckCraft if suitable).
3. **PDF, raster and vector graphics**: reading, annotation, object/layer edits where supported.
4. **Audio, video, CAD and other creative apps**: later, after per-editor architecture/feasibility assessments.

For every candidate, maintain a compatibility record: source URL, pinned version/commit, license (and redistribution terms), browser build status, supported import/export formats, command API maturity, autosave behavior, platform requirements, performance, and accessibility. **No editor is considered integrated merely because its webpage loads in an iframe.**

Prefer self-hosted pinned browser builds under a controlled SAIL origin and isolated storage; external hosted apps are opt-in integrations with explicit data-transfer disclosure, not a local-first default. Third-party GPL/AGPL/proprietary dependencies must be handled per their actual license requirements.

## 4. Architecture

```text
Voice / Chat / Flow / User UI
             |
          SAIL Mind (intent -> action plan)
             |
       Studio Orchestrator (Rust)
             |
  Guardian authorization + project ACL
             |
   Studio Command Bus (typed commands,
    revision checks, idempotency, receipts)
             |
       Per-editor adapter boundary
             |
    Browser-hosted editor instance
             |
   Native editor document state + save
             |
   Local versioned project storage
```

### Core interfaces (illustrative, adapt to actual codebase)

```rust
struct StudioCommand {
    request_id: Uuid,
    project_id: ProjectId,
    document_id: DocumentId,
    editor_id: String,
    expected_revision: u64,
    action: StudioAction,
}
enum StudioAction {
    ReadSelection,
    ReplaceText { selection: SelectionRef, text: String },
    InsertContent { at: AnchorRef, content: StructuredContent },
    // Editor-specific commands exposed through typed capability manifests.
}
struct StudioReceipt {
    request_id: Uuid,
    before_revision: u64,
    after_revision: u64,
    undo_token: Option<String>,
    changed_ranges: Vec<ObjectRef>,
}
```

- Typed, schema-validated, versioned commands; per-editor allowlisted capabilities and capability discovery. Validate arguments and permissions server-side.
- The agent issues **intent and actions**, not raw arbitrary JavaScript, unrestricted browser automation, or DOM selectors.
- Structured editor adapter using same-origin messaging or an authenticated, origin-validated `postMessage` bridge where embedding architecture requires it. Require message schema, handshake, per-instance nonce/session identity, origin/source checks, sequencing, bounded payloads and expiry.
- Editor adapter must return authoritative receipts and revision numbers after changes actually apply; no optimistic “success” without confirmation.
- Fail unsupported commands clearly. A visual embedding alone must never be advertised as live AI editable.
- Conflict handling: compare-and-swap expected revision, pause and show diff if human edited concurrently. Never silently overwrite active edits.
- Coalesce incremental edits for UI rendering, but preserve meaningful undo/operation boundaries; autosave debounce and explicit save-status/error reporting.
- Model can read selected text/context through an authorized bounded retrieval interface; large files fetched by sections. Redact secrets where appropriate.

### Suggested internal Rust components
- `studio_core`: project/document identities, typed command protocol, authorization metadata, receipts.
- `studio_host`: local file/session storage, revisioning, editor lifecycle.
- `studio_adapters`: per-editor capability contracts and browser bridge.
- Dioxus feature view under existing frontend organization; avoid separate top-level services for each editor.

Names and crate boundaries are provisional: reuse existing common bus, persistence, Guardian, Mind and frontend primitives if suitable.

## 5. Storage, files and versioning

Projects are local-first directories with manifest metadata and native editable files; avoid inventing a universal lossy document format. Support new/open/save/save-as, import, export, recovery after crashes, external-file changes, file locks and safe path normalization. Use atomic writes (temp + fsync + rename where applicable), bounded revision history, and backup-before-conversion. Track user edits and AI edits distinctly but keep one canonical native editor document.

A project can reference reports, spreadsheets, slide decks, PDFs, images and other assets in one place. Cross-document references are explicit and validated. User consent is required before SAIL opens or indexes unrelated files. Search indexing is optional and privacy-scoped.

## 6. Authorization and safety

Guardian grants task/workspace-limited capabilities. Routine reversible edits in an already approved project can be auto-authorized. Require explicit approval for importing from outside scope, destructive bulk operations, publishing/sharing externally, network upload or irreversible transformations.

- Agent must not directly access filesystem outside granted roots or run editor script/plugin commands with arbitrary host privileges.
- Browser/editor processes should be sandboxed. Keep credentials out of untrusted iframe workers and messages.
- Third-party editor updates pinned and reviewed; CSP, origin isolation, no hidden external telemetry or model APIs.
- User-selected autonomy mode affects friction **within** hard capability constraints, never bypasses them.
- Log action ID, file/object scope, grant, revision, result, error and undo pointer with privacy-aware retention.
- Offline operation must be supported for verified browser-capable editor builds and local inference.

## 7. First delivery: narrow end-to-end MVP

**Milestone S1 — One editor, one real AI edit.**

1. Identify and verify one suitable ArtCraft word-processing editor (or document a better fallback if its browser build or licensing is unsuitable).
2. Add Studio to navigation and implement project/file selection.
3. Self-host/embed the actual editor and open/save an editable native file.
4. Implement a typed `read_selection` + `replace_selection`/equivalent command bridge with revision guards.
5. Give Mind an explicit narrow tool to request edits; return real result and undo receipt.
6. Show live edit and undo from existing SAIL UI, including user-facing pending/success/failure.
7. Enforce workspace authorization and ensure offline operation.
8. Provide a real integration/e2e test of selecting text, requesting a rewrite, verifying exact document state, saving/reopening and undo. Include collision, timeout, denial and unsupported-command negative tests.

**Acceptance:** A user opens a document, selects a paragraph, asks “Make this shorter,” observes a real change within the embedded editor, sees the saved artifact after reopening, and can undo. No external inference/storage is required. Tests run against the actual editor adapter, not only mocks.

### Explicitly out of scope for S1
Spreadsheet and presentation adapters, entire Craft suite, joint coauthoring across devices, freeform browser click automation, arbitrary plugin execution, independent real-time collaboration server, complex cross-document workflows and model training.

## 8. Later milestones
- **S2:** spreadsheets, range/formula/chart operations and native save/import/export.
- **S3:** presentation editor, template/slide manipulation, speaker notes.
- **S4:** cross-artifact operations with transactional previews: data → chart → document → slides.
- **S5:** verified image/PDF/design adapters, additional media editors, Skillsmith workflows, offline Android performance optimization.

Each editor requires separate compatibility evidence, functionality matrix, version-pinned build, permission tests and honest import/export fidelity testing.

## 9. Quality gates and observability

Minimum measures: cold/warm editor load, selection-to-visible-edit latency, edit/save success rate, crash recovery, memory on desktop/Android, oversized document responsiveness and consistency after concurrent edits. Report hardware, model, version and methods rather than invented benchmarks.

Security tests: origin spoofing, unauthorized commands, stale revision replay, path traversal, untrusted document prompt injection, unsupported operation and expired capability. Product tests: unsaved-change warning, keyboard/phone accessibility, offline round trip and meaningful error messages.

## 10. Implementation rules

- Inspect actual SAIL APIs before introducing new modules.
- Verify ArtCraft upstream rather than inventing stable SDKs or assuming feature parity.
- Build incrementally and keep UI coherent with the screenshot/current layout.
- Produce functional code, documentation, tests and an unmerged PR for review.
- Never claim real live editing, save, offline capability or end-to-end completion without directly testing it.
