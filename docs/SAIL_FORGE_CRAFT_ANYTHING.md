# SAIL Forge — Craft Anything tool integration

**Status:** Integration proposal, not installed or implemented. **Owner:** Forge (#27); Guardian controls effects; Mind owns inference; Agents owns scheduling; Canvas visualizes comparisons. **Reference implementation:** [storytold/craft-anything](https://github.com/storytold/craft-anything) (MIT OR Apache-2.0, early alpha; review vendored dependencies and NOTICE). **Target:** Ubuntu 26.04, Rust backend, Dioxus Web/Android.

## Objective

Give Forge an authorized **Study → Specify → Build → Check → Refine → Review** capability for creating *new tools and software* from observed interfaces/behaviors, documents, and authorized existing software. The immediate value is **evidence-backed specification generation and differential testing** before letting Forge's local coding agent implement anything.

Do not replace Forge's existing coding loop or move its security boundaries into Craft Anything. No automatic installation of generated tools into SAIL's trusted tool registry.

## Verified upstream capabilities (2026-11-02)

The upstream README and [STATUS](https://github.com/storytold/craft-anything/blob/main/docs/STATUS.md) describe:
- Rust engine, desktop egui app, CLI (`craft-anything-cli`), MCP server (`craft-anything-cli mcp`), with engine commands also available from CLI/MCP.
- 298+ analysis engine tools across web, native binaries, Android, managed apps, game files, archives, and more; `craft-anything-cli commands` exposes the real installed command surface. Treat counts as upstream-version-specific, not a static SAIL contract.
- Clean-room mode: observations from visible UI/behavior, screenshots and user-provided evidence, without reading source/private program internals. Deep mode can inspect program contents, scripts and disassembly, for explicitly authorized targets only.
- Website capture across desktop/tablet/mobile, layout/style/interaction comparisons, provenance and unknowns.
- Upstream full rebuild has verified Claude Code path. Other coding agents, including Codex adapter, are not fully verified. Consequently **SAIL's own Forge coding loop remains the builder** at MVP; no Claude subscription requirement.
- Upstream includes `core_evidence_export`, `core_evidence_browse`, `core_unknowns_show`, `core_brief_write`, `core_target_triage`, `web_site_capture`, `rebuild_check` (confirm availability and request schema for the pinned build).
- Source is [MIT/Apache-2.0 dual licensed](https://github.com/storytold/craft-anything/blob/main/README.md); preserve attribution and validate the exact upstream commit and transitive licenses.

## Recommended integration strategy

**Phase 1: pinned CLI adapter** (best isolation, low coupling). Install a version-pinned `craft-anything-cli` binary under `/srv/sail/runtime/tools/craft-anything/`, record SHA-256 and upstream commit. Forge spawns it as a restricted subprocess via Guardian, with explicit typed requests. Call `craft-anything-cli doctor` and `craft-anything-cli commands` to detect supported features at runtime. No shell string interpolation or arbitrary agent-written CLI arguments. Keep all child state in a dedicated Forge job directory on `/srv/sail`, larger captures on `/srv/sail-data`.

**Phase 2: MCP adapter** once trusted schemas, tool session lifetime, and audit controls are proven. Use MCP stdio as a child process, not an open unauthenticated LAN server. Map only a reviewed allowlist of tools; require deterministic version negotiation. Per-tool Guardian policy and user approval remain authoritative. **Do not expose hundreds of tools unfiltered to the LLM**.

**Phase 3: narrow Rust crate integration** only if measurable start-up latency, schema reliability, or performance justifies vendoring upstream crates. Respect maintainability and licensing; never fork the entire egui UI as SAIL's frontend.

### Tool taxonomy and initial allowlist

| Forge capability | Upstream tools to evaluate | Output |
|---|---|---|
| Target identification | `core_target_triage`, `app_folder_explain` | Target kind, constraints |
| Clean-room web observation | `web_site_open`, `web_site_capture` | Responsive observations with evidence IDs |
| Evidence capture | `core_evidence_show`, `core_evidence_export`, `core_evidence_browse` | Structured provenance and limitations |
| Knowledge gaps | `core_unknowns_show`, `core_unknown_add` | Explicit unresolved questions |
| Specification handoff | `core_brief_write`, `core_report_write` | Authored functional spec, with evidence trace |
| Differential checks | `rebuild_check` / relevant website checks if compatible | Comparable measurements, not proof of correctness |

The listed identifiers are upstream names, **not verified SAIL endpoints**. Confirm exact schemas through the installed tool manifest and versioned contract tests. Exclude file unpacking, binary execution, command execution, network access and Deep tools from initial allowlist.

## User flow in existing Forge UI

**Forge → New Task → “Build a tool from reference”** (not a new global tab). Input: link, screenshot, file, authorized installation, or blank idea. Fields: goal, required behaviors, desired Rust/UI platform, observation mode (**Clean room default**, Deep explicitly gated), network/data permissions. User chooses an isolated output workspace.

```text
Forge / Build from Reference
[ Reference URL or file ]   [ Clean room ▾ ]       [ Inspect ]
┌───────────────────────────────┬──────────────────────────────┐
│ OBSERVATIONS & EVIDENCE       │ BUILD STATUS                 │
│ visual/layout/behavior        │ Study → Spec → Build → Check │
│ evidence IDs, coverage        │ model, tests, approvals      │
│ open questions/uncertainty    │ live sandboxed preview       │
├───────────────────────────────┴──────────────────────────────┤
│ Differences: screenshot / behavior / API contract / tests    │
│ [View specification] [Ask follow-up] [Refine] [Review diff]  │
└──────────────────────────────────────────────────────────────┘
```

Progress is *task state*, not simulated activity. Provide real progress events and error states. Show a source/capture overlay in the existing Canvas preview with desktop/tablet/mobile toggles and grounded visual comparisons. Preserve expandable proof/evidence cards; collapsed by default.

**Mobile:** single-column stage overview; evidence and comparison as sheets; approvals require explicit confirmation; no side-by-side comparison that overflows phone width. Keep Forge queue and task resume semantics.

**Outcome:** a named, versioned tool candidate with source project, contract, runnable examples, diff, test reports, provenance, and an explicit **Propose installation** action. The final install is a separate Guardian-authorized review event.

## Architecture and contracts

```text
Dioxus Forge Task UI
  → Forge task/continuity queue
  → CraftReferenceAdapter (versioned Rust typed commands)
  → Guardian-authorized isolated child process [Craft Anything CLI/MCP]
  → EvidenceStore + SpecificationArtifact (immutable evidence refs)
  → Forge local coding agent + worktree (existing pipeline)
  → Test runner + differential checks
  → Human review → optional signed/approved Skillsmith tool registry
```

Suggested DTOs: `ReferenceTarget { kind, locator, authorization, mode }`; `StudyRequest { task_id, target, budget, allowed_tools }`; `EvidenceRecord { id, source, capture_time, observation, confidence, limitations, hash }`; `ToolSpec { capabilities, input_schema, output_schema, constraints, evidence_ids, unresolved_questions }`; `CheckResult { test_id, expected, observed, verdict, artifact_path }`; `ToolProposal { code_revision, permissions, tests, provenance, installation_request }`.

Events: `reference.study.started`, `reference.evidence.recorded`, `reference.study.blocked`, `reference.spec.ready`, `reference.build.started`, `reference.check.completed`, `reference.refine.requested`, `reference.review.ready`. Persist with Forge task ID, step ID, monotonic revision and idempotency key. Recover after process death without replaying privileged effects.

## Safety, copyright and resource budget

- Clean-room default is architectural separation: untrusted reference evidence must not be presented as authorized internal code. Deep exploration requires explicit rights and a separate permission scope; never bypass auth, DRM or security controls.
- Authorization applies to **the target, the selected upstream tool, the exact filesystem roots, network destinations and child processes**, not merely to “Craft Anything” generally. Guardian enforces at the OS/adapter boundary. Deny if sandbox not available.
- Network capture may contain personal information, secrets or session cookies: minimize, redact, encrypt if required and provide deletion. Do not automatically forward reference content to hosted models.
- Inference via local Mind; subscribed Codex CLI review remains subject to its one-invocation-per-issue durable reservation rule. Craft Anything must not independently spawn an unchecked subscription-billed agent.
- Limit memory, CPU, time, subprocess count, disk and network. SAIL root partition is not a workspace. Protect active voice responsiveness on 32 GiB RAM.
- Never interpret a screenshot similarity score or successful compilation as behavioral equivalence. Require functional, accessibility and security tests. Store negative results and unknowns.
- Generated tools run quarantined with bounded permissions until explicit review/activation. No automatic modifications to SAIL Core or permanent privileged plugin installation.

## Implementation milestones

**F0 — discovery/audit**: Pin upstream commit or release, audit license/build/commands and sandbox support, inspect Forge code paths and map existing typed events. Run `doctor` on the Ubuntu host; record available tool schemas and nonfunctional capabilities. No UI shipping claim.

**F1 — real thin vertical slice**: A user supplies a publicly accessible reference website. Forge invokes only approved clean-room inspection through CLI, stores evidence and unknowns, outputs a *non-hallucinated* testable Rust/Dioxus implementation brief. Demonstrate restart/reconnect and blocked permission results.

**F2 — build/check**: Forge local model implements in isolated worktree; system builds, runs tests and compares accessible reference behavior/screens at 1440/768/390 px, with separate evidence-based differential report and explicit missing comparisons.

**F3 — tool packaging**: Export a manifest and artifact; demonstrate sandboxed standalone run and user-reviewed registry installation without auto-merge or credentials exposure.

**F4 — opt-in deeper study**: Per-target Deep permissions and additional Android/native/file analysis only after adversarial sandbox and legal review.

### Non-negotiable acceptance

- A real local inspection result with evidence ID and replayable summary; no mock success.
- No untrusted child process or shell bypass of Guardian; authorization refusals observable.
- Local inference, offline/CLI-missing/version-skew states handled honestly.
- No re-run of approved side effects on reconnect; durable job history.
- One visible, keyboard/touch accessible Forge user flow through Dioxus Web/Android.
- Generated tool fails closed until separate review.
- Tests demonstrate all claims; submit reviewable PR, do not auto-merge.

## References
- [Craft Anything main repository](https://github.com/storytold/craft-anything)
- [Upstream tools](https://github.com/storytold/craft-anything/blob/main/docs/tools.md)
- [Upstream implementation status](https://github.com/storytold/craft-anything/blob/main/docs/STATUS.md)
- [Upstream extensibility](https://github.com/storytold/craft-anything/blob/main/docs/extending.md)
- [Forge](SAIL_FORGE.md), [Guardian](SAIL_GUARDIAN.md), [Agents](SAIL_AGENTS.md), [Canvas](SAIL_CANVAS.md).
