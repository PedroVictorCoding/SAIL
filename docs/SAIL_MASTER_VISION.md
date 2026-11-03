# SAIL — Master Product Vision and System Implementation Contract

**AUTHORITATIVE DESIGN REFERENCE FOR CODING AGENTS** · Revision 2026-11-02 · Status: *target architecture, not a claim of delivered functionality*

> Before implementing a SAIL issue, read this document, [AGENTS.md](../AGENTS.md), [DESIGN.md](../DESIGN.md), the corresponding [subsystem specification](README.md), and the actual code. **The user's explicit product decisions from the SAIL design conversation establish the intended vision.** Existing repository code establishes the *current implementation*. The two must not be conflated. The roadmap defines ownership and prioritization. When any of them disagree, record a decision, do not quietly substitute a narrower interpretation.

## 0. A statement of purpose

SAIL is **one persistent, self-hosted, local-first, multimodal, proactive and increasingly autonomous personal AI system**. It is intended to operate like a Jarvis-style intelligence layer across the user's computer, browser, mobile device, files, projects, media and knowledge.

It is not merely a chatbot with tool calling, not a launcher dashboard with dozens of buttons, not a browser automation script, not a model downloader, and not a collection of unrelated subsystem demos. Those are possible components of a larger continuously operating assistant.

The experience should be: user expresses a goal naturally; SAIL knows what is known, uncertain, available and authorized; SAIL selects an appropriate model and tools, performs real bounded actions, reports actual progress, can resume work after disconnection, remembers the *meaning* of what was learned rather than raw conversation text, and updates the relevant workspace on screen. The user can intervene, inspect evidence, undo reversible actions, revoke permissions and stop the system.

**A capability that looks impressive in a screenshot but cannot safely complete a real end-to-end task is not delivered.**

### 0.1 Target environment and scope

- Primary host: Ubuntu Desktop 26.04, i5-10400 (6 cores/12 threads), 32 GB nominal RAM, AMD RX 5500; no assumption of ample VRAM. Hardware characteristics must be measured, not guessed.
- Storage layout: Ubuntu root on ~128 GB SSD; Kingston ~240 GB SSD mounted at `/srv/sail` (`SAIL_RUNTIME`), Seagate ~8 TB HDD at `/srv/sail-data` (`SAIL_DATA`). Runtime/model/build data should not saturate the root partition. Verify mounts before writing; service units should require mounts.
- Primary client delivery: **shared Rust/Dioxus Web and Android**. Desktop browser is a first-class way to operate; Android has appropriate responsive and lifecycle behavior. iOS, Windows and macOS packaging are later ambitions, not present blockers.
- Compute: local inference by default; llama.cpp/OpenAI-compatible loopback adapters, CPU and tested AMD Vulkan offloading. Models are selectable/routed by purpose and budget; do not make a cloud endpoint mandatory.
- Rust owns the long-running assistant and critical state/permission paths. The intended direction is no required Python runtime, but the current README documents supervised Python-based voice workers. Treat conversion as a migration, **not** as a shipped fact and not as grounds to break working audio.
- External models (the owner's ChatGPT, Claude and Gemini subscriptions through their own CLIs, and API keys) are used only by escalation after local attempts fail measurable checks, only with quota left, and always visibly (decision 12). Never silently route general SAIL inference to a paid cloud API.

### 0.2 Product invariants

1. **Coherent single assistant.** Product-facing names correspond to capabilities, not mandatory distinct processes, models or global UI tabs.
2. **Real world state.** Observed, planned, attempted, completed, tested and deployed are different states. Never substitute an LLM's claim for a verification receipt.
3. **Guarded autonomy.** Guardian authorization is enforced by the OS/process/tool boundary; prompts are not policy. An agent cannot self-authorize.
4. **Memory without transcript hoarding.** Durable memory is structured, evolving knowledge with evidence and temporal validity, not indefinitely stored conversations or chat embeddings.
5. **Continuity.** Background work is durable and user-visible, with correctly handled restarts, cancellations, checkpoints and idempotency.
6. **Local ownership.** Data, models, tasks and artifacts stay local unless a clearly authorized external integration requires disclosure. External models are such an integration (decision 12): used by escalation, never silently, and never given private memory (decision 15).
7. **Resource discipline.** Live voice and active interaction are favored over large background inference; model residency and job scheduling prevent swapping the host to death.
8. **Reversible by default.** Preview edits and tool actions. Require stronger approval for destructive, externally visible or irreversible effects.
9. **Proactive but not intrusive.** Curiosity, suggestions, notifications and presence obey attention limits, quiet hours and snooze.
10. **High-integrity UI.** Display states derived from authoritative backend events; no fake activity or fake functionality.
11. **Correctness before coverage.** Implement one complete useful vertical slice before an entire suite of unsupported cards/tools.
12. **Inspect before designing.** The developer must read current code, target issue, docs, dependencies and upstream license; never invent interfaces.

## 1. What the user should experience

### 1.1 Ambient presence and natural commands

SAIL is capable of always-available but user-controlled listening. The voice chain includes VAD, speech recognition, intent relevance, dialogue/reasoning, TTS and Orb animation. Passive sound should not necessarily become an LLM prompt; voice-trigger/turn policy filters first. Speaker recognition (after explicit enrollment) is a potential control, not infallible identity proof. The assistant can be interrupted; user can mute, stop, change modality and inspect recording state. Audio latency matters. Voice is not an excuse for hidden recording or broad permission.

The Orb expresses states such as idle, listening, thinking, speaking, tool work and error. Visuals should be ambient rather than a blinding animation. The assistant can render diagrams, source-grounded cards, media, editable workspaces and task views as appropriate. Context lives with the task, not in a wall of verbose status narration.

### 1.2 One interface, layered complexity

The aesthetic direction from design discussions is minimalist, spacious, translucent/glass-like, restrained and polished; native platform and accessibility rules still apply. Existing [DESIGN.md](../DESIGN.md) is the implementation UI contract. Central contextual cards, a voice/assistant surface and meaningful workspaces must avoid border/button fatigue.

**Basic layer:** speak or type, receive focused response or see an actual action; one prominent next step.

**Intermediate layer:** a contextual task, live workspace, progress, reasoning summary, evidence, editable plans.

**Expert layer:** explicit model selection, tool trace, schema, logs, graph, sandbox permits, resource measurements, diff, rollback and advanced workflows.

A new tool should normally extend an existing workspace or a contextual panel. Do not create a permanent page named after a library, agent, vendor or infrastructure mechanism.

### 1.3 Work does not depend on browser presence

A browser/Android client is a projection of authoritative server state. Closing the tab must not silently abort an accepted long-running job. Reconnecting must fetch the canonical task status and replay bounded event history without repeating irreversible actions. Distinguish successful completion from transport EOF, local optimistic UI and tool “accepted” status.

### 1.4 Examples of complete interactions

**Development:** “Continue SAIL development.” SAIL retrieves approved work priorities, assesses what is blocked, proposes or selects a scoped Forge issue, reserves compute, builds in isolated worktree, runs actual tests, requests user approval for external effects and shows diff/review. A model-unavailable fallback must be blocked, **not** a comment-only patch that claims to fix the issue.

**Learning:** “Teach me NAT traversal for my exam.” Campus inside Life finds course context and prerequisites, probes understanding, maps dependencies, teaches one step at a time, presents a dedicated answer/reasoning dock, supports deliberate retrieval practice and updates Arena's knowledge graph. It should not be merely a tutor prompt.

**Documents:** “Shorten this paragraph.” Studio identifies an actual open document and selection, requests scoped edit authority, applies a typed real edit, shows changed text, persists the native file and can undo. Canvas, by contrast, may show a transient visual explanation.

**Research:** “Investigate whether this dependency is suitable.” A research agent uses approved browser/search tools with citations and isolation; records results, unknowns and confidence; compares sources and returns a useful recommendation. It must distinguish sourced findings from speculation.

**Personal assistance:** “Remind me after class to continue this.” Life understands course context, Pulse owns the actual reminder timing, Relay presents it across devices as authorized; memory records a meaningful preference or commitment rather than permanently storing the whole exchange.

**Discovery:** When idle and permitted, SAIL notices a meaningful new research paper, job opportunity, interesting album or project friction, produces an evidence-based recommendation and offers “Not now” with appropriate delay. It is not allowed to start consequential action without the necessary grant.

## 2. System boundaries and ownership

The roadmap [#15](https://github.com/PedroVictorCoding/SAIL/issues/15) originally specified 20 product subsystems in six domains; Health [#104](https://github.com/PedroVictorCoding/SAIL/issues/104) extends product intent to 21 subsystems/seven domains. **Core is shared foundation outside the product subsystem count.** Studio, Campus, Skillsmith and the browser worker are experiences/cross-cutting capabilities unless formally promoted through a roadmap decision. Domain groupings are *not* process boundaries.

### 2.1 Intelligence and Knowledge

- **Mind #3:** local model inventory, model/provider adapters, Auto/Fast/Balanced/Deep/Custom routing, context window allocation, prefill/generation priorities, model warm/cold residence, measured CPU/Vulkan use, fallback and inference cancellation. Mind does not own tool privilege or project memory.
- **Arena #5:** structured, evidence-attributed knowledge; semantic, temporal, procedural, project and preference memory; entity graph; retrieval, consolidation, contradiction handling, user correction and forgetting. Avoid retaining raw conversations by default. Core supplies durable persistence infrastructure; Arena owns representation and semantics.
- **Discovery #10:** proactive interest exploration, research ideas, useful change detection, Echoes, novelty/expected value, attention budget. Discovery proposes; it does not autonomously grant authority.
- **Lab #14:** research, experiments, source/evidence auditing, evaluation and adversarial validation. Distinguish experiments and simulated results from established facts.

### 2.2 Interaction and Presence

- **Dialogue #1:** voice capture, VAD, STT, turn management, text response, speech output, barge-in and Orb; proper buffering/interrupt semantics.
- **Vision #2:** camera/image perception, OCR/document intelligence (OXR candidate), observed visual context with explicit acquisition permission.
- **Sense #12:** opt-in ambient signals, context and telemetry; never passive indiscriminate surveillance.
- **Relay #37:** continuity of sessions, frontend projection, audio focus and device-to-device handoffs; **not** distributed inference.

### 2.3 Agency and Creation

- **Agents #6:** goal decomposition, choice of specialist workers, bounded tool calls, delegation, step validation and result synthesis. It does not directly bypass Guardian or own inference servers.
- **Flow #8:** explicit workflows, triggers, conditional DAG execution, schedules and deterministic templates. **Skillsmith #62** within Flow helps propose/test/publish reusable typed workflows. Instinct #66 maps known utterances to authorized deterministic actions with minimal model involvement.
- **Canvas #9:** transient, interactive visual cards, diagrams, explanations, charts, forms and temporary simulations. It does not become permanent document storage.
- **Forge #27:** software-development agent workflows, repository context, isolated worktrees, real patches/tests/diffs, PR assistance, resource admission and coding benchmark. It must not claim to solve issues without substantive implementation and verification.

### 2.4 Personal and Social

- **Life #16:** user goals, calendar and time constraints, plans, academic Campus, project/group context, possessions and choices.
- **Pulse #11:** reminders, routines, recurring schedules, notification timing, snooze and quiet hours; distinct from Discovery's suggestion selection.
- **Friends #20:** consented context about people and relationships, privacy-specific permissioning and social continuity; not a separate generic memory database.
- **Entertainment #17:** music, video, reading and media continuation; discovery, Audio DNA, release tracking, approved playback adapters and optional mood-based “Moment Discovery” inspired by Taranify *as a product reference, not as open-source code*. Avoid pretending a color quiz is a validated psychological test.

### 2.5 Systems and Security

- **Guardian #4:** single authority for capability grants, restrictions, approval, resource/credential isolation and audit receipts; enforcement at OS/IPC/adapter boundary.
- **Bridge #7:** actual OS/browser/app/service/device adapters, permissioned connectors and command/browser workers. Bridge does not decide which permissions an agent deserves.
- **Sentinel #36:** interprets security anomalies and monitors authorized activity; provides warnings/recommendations/response proposals but cannot bypass Guardian.

### 2.6 Infrastructure and Health

- **Mesh #13:** optional distributed model computation and peer workers; distinct from Relay's device UI continuity.
- **Health #104:** opt-in physiology/wellness records and research support, strong sensitive-data isolation and non-fabricated source attribution. Do not mix health records into the general coding-agent memory context.

### 2.7 Core shared platform

Core owns daemon lifecycle, versioned typed protocols, event bus, structured persistence, durable scheduler/Continuity, resource QoS, crash and reconnect recovery, tracing, identity and service health integration points. Core does **not** become an alternative Guardian policy authority or duplicate Mind inference logic.

### 2.8 Cross-cutting experiences

- **Studio:** real editable projects/documents using verified native/browser editor adapters, typed commands, save/undo, CAS revision checks. ArtCraft ecosystem candidates require verified SDK/browser/format/licensing feasibility.
- **Campus:** adaptive learning UI within Life, using Arena graph and Mind/Agents; diagnostic probe → dependency plan → teach/check → separate transfer practice. LearnHarness is an architectural/pedagogical reference, not copied code.
- **Skills and Skillsmith:** Skills registry is callable permissions/contracts; Skillsmith is bounded Flow-based skill proposal and approval. Skills should be testable, versioned, reproducible and revocable.
- **Continuity:** server-owned task execution that survives UI and daemon lifecycle events. It is distinct from Relay and Flow's user-defined automation graph.
- **Headless browser runtime:** Bridge/Guardian worker available to Forge, research, Discovery and user-directed actions; not its own unrestricted system root.

## 3. Critical distinction: a product capability versus a subsystem

When implementing “web research,” do not create a new independent Web Research service with its own database, policy engine and tab. Instead: Agents requests research; Guardian approves target/network scope; Bridge owns browser/search adapter; Mind interprets results; Arena optionally retains evidence; Canvas displays sourced cards. Similarly, “continue coding” is not a button that invokes an LLM and prints success: Forge handles the real workflow using Core Continuity, Mind, Guardian and the existing UI.

A new feature is **complete only when it is correctly integrated into all ownership boundaries it touches**.

## 4. Memory: the most important architectural correction

The user explicitly rejected the common pattern of persisting whole conversations as “memory.” The desired system is an **evolving, compact, correctable world/user/project model**.

### 4.1 Memory types and representation

**Semantic:** entities, claims, relations and conceptual dependencies.

**Procedural:** reusable workflows with preconditions, postconditions, permissions, versions and real outcome evidence.

**Project:** objectives, architectural decisions, constraints, task states and issue references, distinguishing proposed, approved, underway, tested, shipped and superseded.

**Temporal:** observation timestamps, valid intervals, freshness and transitions; current state must not erase meaningful history.

**Preferences:** explicitly reported versus inferred choices, scope, salience and revisability.

**Epistemic:** verified/user-reported/inferred/disputed/invalidated, independent evidence and provenance.

Use typed IDs, entity resolution, claim records and explicit evidence. Vector similarity is for **retrieval assistance**, not truth or unique identity.

### 4.2 Consolidation, not hoarding

Default pipeline:

`ephemeral conversation / tool observation → candidate extraction → entity resolution → evidence classification → contradiction check → change proposal → deterministic validation → transactional consolidation → permitted source cleanup`.

Never leave the entire transcript as the authoritative permanent store. If recovery requires temporary event history, specify an expiry, local encryption and deletion controls. Evidence may be kept as bounded receipts with hashes and provenance without retaining raw chat. Watch all accidental raw-content persistence: model traces, logs, swap, caches, screenshots and backup copies.

**Example:**
- “Disk is 100% full” observed at time T1.
- “Cargo target is 42 GB” observed at T2.
- “Kingston runtime SSD is mounted at /srv/sail” observed at T3.
- User was advised to relocate build artifacts.
- Follow-up df showed root at 58% used and 47 GB available.
- Memory should record current verified mounts and storage constraint; historical full-disk event; **migration not confirmed unless actual post-change observation proves it**. Do not preserve eight chat turns for this.

### 4.3 LLM proposals are not database writes

The model creates a proposed `MemoryChangeSet` containing create/revise/supersede/link/conflict resolution operations. A deterministic Rust validator checks: schema, evidence support, current revision, permissions, privacy, entity links, duplicated claims, deletion generation and idempotency. Only then may a transaction commit.

Do not treat “model confidence: 0.95” as evidence. Statements from the same original message, however many times summarized, are not independent observations. An assistant's own optimistic action reports are not external verification.

### 4.4 Storage and retrieval

Start with SQLite WAL, FTS5, typed relationship edges, local embeddings + a bounded vector index, and versioned procedural manifests. No separate graph server or hosted memory database required at first. Storage adapters live in Core; Arena controls claim semantics. Query using a combination of exact IDs, full-text, semantic retrieval, graph neighborhood, time validity, task relevance, permission and provenance.

A **Context Composer** supplies the smallest relevant knowledge set within explicit token budgets to the task. A coding agent gets repository facts, not unrelated personal health or private social records. Entire knowledge graph injection is neither necessary nor safe.

### 4.5 Correction, forgetting and UI

Memory UI should show **Projects / Knowledge / Preferences / Skills / Questions**, with “why does SAIL believe this?”, inspect, correct, pin, merge/split, mark private, forget and export. It is not a permanent chat history interface. Deletion must invalidate downstream indexes and queued memory extraction jobs to prevent resurrection.

### 4.6 Memory acceptance test

A controlled sequence of conversation and tool events leads to compact evidence-attributed knowledge. Raw turns expire. A later relevant task retrieves the correct current facts; incorrect and sensitive facts do not leak; contradictory new evidence updates validity; explicit deletion survives rebuild and restart.

## 5. Models and inference are resources, not personalities

Mind should choose among small fast models, larger local reasoning/coding models, speech processors and vision models by task, measured quality, available memory, latency and inference cost. Model names are moving targets; examples discussed include Qwen3.6 35B-A3B, Qwen3-Coder variants, Gemma, LFM3-Omni and Kokoro. **A model's presence in a roadmap is not proof the corresponding weights or backend are installed.**

The Linux host actually demonstrated a ~20 GiB resident CPU-only llama-server process for a 35B Q4 model, exhausting an 8 GiB swap file while SAIL and voice workers ran. Stopping it recovered RAM. Therefore:
- Do not maintain multiple huge “always loaded” model processes.
- Prefer single heavy resident model with lightweight speech/vision services if measured budgets permit.
- Preempt low-priority research/Forge work for live Dialogue.
- Use SSD to back model weights and page cache; an unloaded model need not literally be rewritten from RAM to disk.
- Only enable Vulkan GPU offload after verifying driver and benchmarking layer counts on the specific AMD card.
- Report real prefill, TTFT, tok/s, working set and power/thermal behavior rather than guessed performance.
- Remote API-like interfaces may be **local loopback endpoints**, not a need for hosted inference.

A user-facing mode selector Auto/Fast/Balanced/Deep/Custom controls policy and model budget; it must be backed by actual routing logic and explain unavailable modes.

## 6. Voice and multimodal reality

Dialogue is an always-available option, not a mandate to stream every sound to the LLM. Silero VAD at 16 kHz mono and chunked transcription have been considered; the shipped voice path currently includes Python workers. Treat Qwen3-ASR, Candle Whisper and TTS migration as candidates that require reproducible Rust/ONNX integration and speech-quality testing. Preferred speech should handle European Portuguese and English with measured intelligibility. Conversational speech generation is not automatically reliable verbatim TTS. Do not promise speaker identity as an authentication factor without evaluation and fallback.

Vision handles validated images, camera frames and page-aware document parsing; the text model should receive bounded, permissioned observations. A camera or microphone is a user-controlled privacy-sensitive capability.

## 7. Tools, browser sessions and privileged work

### 7.1 Strict separation of planning and effect

`user/task → Agents/Flow planner → typed tool request → Guardian policy broker → Bridge sandboxed executor → result receipt → agent verification → UI projection`

- Tool registration has schema, ownership, scope, authorization requirement, version, timeout, cancellation and failure contract.
- No LLM-authored unrestricted shell is implicit in generic “tool use.”
- No browser worker gets host files, credentials, Git repository, model memory or LAN reachability by default.
- Network actions block metadata/private IP/localhost SSRF unless specially authorized; DNS/IP policy must apply across redirects and rebinding.
- Credential use is provided by an explicit scoped broker, with redacted tool results. No credentials dumped to the model prompt.
- OS tools may provide status, allowlisted process lists and workspace-scoped file queries; write/exec needs a separate narrow capability.
- Prompt injections in webpages, repositories and media metadata are *data*.

### 7.2 Headless browser

The desired browser is a **spawnable, temporary, isolated Chromium worker**, owned by Bridge and authorized by Guardian. It supports create, navigate, observe, action, screenshot, extraction, download control and close. Browser may be visible in Canvas when useful; session can be user-intervened. Notte is a product/architecture reference for observe-act-extract but its Python-first implementation and SSPL licensing conflict with a direct default dependency. Prefer Rust-owned CDP orchestration with a pinned browser and security controls; evaluate adapters if useful.

Research browser and authenticated user browser are **different permission profiles**. The first has ephemeral storage, no secrets and constrained egress; the second requires explicit site/account grant, profile isolation and user intervention. Do not circumvent DRM, site authentication or account restrictions.

### 7.3 Craft Anything and tool development

Craft Anything is a Rust-native reference/engine for **Study → Specify → Build → Check → Refine**, with CLI/MCP inspection. [Forge integration spec](SAIL_FORGE_CRAFT_ANYTHING.md) tracks the clean-room evidence-driven tool workflow; upstream is early alpha. Use it as a bounded inspection adapter, not as a replacement for Forge's coding pipeline or as authority to run arbitrary deep inspection. Initial integration should prefer its pinned CLI under a Guardian sandbox, then evaluate MCP and possibly crate integration. No automatic trusted tool installation.

### 7.4 Security monitoring

Guardian is authoritative for preventing unauthorized action and issuing audit receipts. Sentinel is a *separate observer* that detects suspicious tool/agent behavior and proposes intervention, with Guardian still enforcing effects. Existing Linux sandbox references include Landlock/nono and bubblewrap; do not apply irreversible host-wide restrictions to the SAIL parent daemon. Fail closed if a requested sandbox is unavailable, rather than silently falling back to unsandboxed host execution.

## 8. Forge: genuinely autonomous coding

Forge's user-visible mission is to accept a scoped coding request/issue, research its context, produce working code in a branch/worktree, run real tests and validation, show a verified diff, request review and produce a PR with honest provenance. It is not a completion generator.

### 8.1 Coding pipeline

`issue intake → clarification/contract audit → worktree/permission reservation → context retrieval → plan → local model patch → compile/test/lint → failure diagnosis/retry → independent review → human approval → PR`

For each step persist: task ID, issue/repo identity, state, input revision, model/backend, tool receipts, files changed, test commands, exit codes, logs, blockers and next safe action. Stop or request intervention on missing model, unavailable resources, ambiguous issue or failing tests. Do not equate “nonempty diff” with implementation, or a comment-only patch with a solution.

### 8.2 Codex reviewer exception

Prior design requires **at most one subscription-authenticated Codex CLI reviewer invocation per canonical repository+issue**, using a **durable atomic reservation before process launch** and a verifiable result. A process-local set is insufficient after crashes. Do not replace the user's subscription mechanism with paid OpenAI API calls or quietly invoke Codex multiple times through wrappers. User approves exceptional cost/role changes explicitly. Track reviewer availability and budget, degrade to no external review rather than pretend it happened.

### 8.3 Verification

The task is finished only if there is a real implementation, meaningful tests of requested behavior, reviewed diff and actual task output. Compile success alone is insufficient; unit tests of mocked dependencies are not an end-to-end proof. Protect source checkout and credentials from arbitrary sandboxed build actions. Do not auto-merge a generated PR. Follow [AGENTS.md](../AGENTS.md): task branch, checks, PR, reviewer merges.

### 8.4 Skillsmith versus Forge

Forge builds or modifies **software**. Skillsmith constructs **versioned, typed reusable Flow workflows and capabilities**, validated and user-approved. They can cooperate: Forge may build an adapter used by a Flow skill, but a reusable prompt or generated script does not gain privileged tool registration automatically.

## 9. Studio, Canvas and persistent artifacts

- **Canvas** is a transient, adaptive visual response plane: Mermaid diagrams, structured graphs, explorable cards, quick controls, temporary code/visual previews. Canvas can embed visualizations but does not pretend they are a saved document.
- **Studio** is the persistent workbench: native document state, versioning, editor adapters, actual open/edit/save/undo. Project and selection scope must be explicit. ArtCraft-style editors are candidates to verify, not assumed working APIs.
- **Forge** owns code repositories, worktrees and tests; it does not become the word processor.
- **Arena** may remember project decisions and provenance but is not the canonical document byte store.

This difference determines state ownership and prevents coding agents from building a fake editor out of chat-generated HTML without file round-trips or undo.

## 10. Campus and adaptive learning

Campus is **inside Life**, not an extra backend subsystem. LearnHarness supplies a pedagogical/UI reference: probe prerequisites → visible plan DAG → incremental explanation/check/repair → independent recall and transfer practice → knowledge graph. A compact lesson map and dedicated reasoning/question dock should remain available without long scrollback. Learner reasoning and self-reported confidence are signals, not proof of mastery. Arena stores source-linked propositions and demonstrated evidence, not psychological or academic ability ratings. User can import course material with permission. Reference: [Campus spec](SAIL_CAMPUS_LEARNING.md).

## 11. Entertainment and other personal experiences

Entertainment unifies approved playback, media discovery, reading state, release tracking and optional audio similarity. “Moment Discovery” may offer a short, playful color-preference/mood-direction exercise using **Taranify as a public product reference**. The color method is not a validated mental-health assessment and source code is not confirmed reusable. Cross-service media catalog/playback requires actual Bridge adapters and user permissions.

Life manages commitments, campus, routine constraints, practical scheduling and personal workspace; Pulse times reminders; Friends manages opt-in people context; Health has stricter private scopes. These must never be bundled into Forge's coding context as a default.

## 12. Discovery, proactive research and continuity

SAIL's idle behavior should be useful rather than “thinking aloud” randomly. Discovery can detect new useful papers, coding opportunities, missed tasks, relevant media or opportunities using opted-in interests and evidence. It prioritizes expected usefulness, novelty, actionability, cost and interruption budget. It can prepare background research through Continuity, but consequential actions are still gated by Guardian.

The user should see a compact Discovery card and, where enabled, an audible notification with **Not now**, snooze, mute topic and quiet-hours controls. A “not now” dismissal must actually suppress near-term recurrence (design preference: at least an hour) and persist across restarts. Multiple perspective/Echo workflows are bounded and scheduled so they do not monopolize local inference.

Background work has durable identity and state, not a detached `tokio::spawn` with no recovery path. Checkpoints should resume safely; untrusted external actions cannot be blindly replayed.

## 13. Hardware, filesystems, deployment and QoS

### 13.1 Known host failure lessons

The host once reached 100% root disk usage; a large Rust `target/` directory (~42 GB) occupied the root filesystem, while the extra `/srv/sail` SSD had capacity. Qwen CPU inference consumed ~20 GiB RSS and swap reached 8/8 GiB. Stopping the model recovered memory. This is **design evidence** for stronger resource scheduling and storage placement, not an instruction to run those cleanup commands automatically.

Baseline storage preference:
- root SSD: Ubuntu, source and lightweight config;
- `/srv/sail` runtime SSD: active GGUF, Cargo target, Forge worktrees and latency-sensitive runtime;
- `/srv/sail-data` HDD: **all persistent SAIL data** (Continuity task state, Arena, Forge state, review ledger, Guardian learned rules, logs, indexes), plus cold models, archives, datasets, histories and backups. It is owned by the SAIL service user and a `sail-data` group (mode `2770`), not root-only; the sandbox runner user is never in that group (decision 10 below).

Actual paths must be configurable, probed for mounts and free space, and subject to safe shutdown. `/tmp` may be a tmpfs; avoid spilling large browser artifacts and model caches into it. Do not blindly expand swap or reboot production tasks.

### 13.2 Resource admission

Every heavy task declares CPU, RAM/VRAM, disk, expected runtime and priority. The scheduler protects audio responsiveness, interactive actions and safety controls. Multiple model or Chromium processes need concurrency limits. Expose observed CPU/RAM/swap and active model residency to user and operators, not fabricated utilization.

### 13.3 Logging and secrets

Keep task logs structured and bounded, with redaction. Never include tokens, account cookies, private file content or unredacted conversations in debug logs. Observability records action/result metadata and permission receipts, not an unlimited sensitive-data archive.

## 14. The frontend contract

Use one shared Rust/Dioxus state model for Web and Android. Render authoritative task state from Core; show real tool execution, typed approval cards, errors, retry, cancellation and offline state. The existing design system controls colors, spacing, layout, Orb placement and accessibility.

Avoid:
- adding a global navigation item for every package/model;
- recreating a separate “assistant chat” inside each subsystem;
- static progress bars disconnected from backend;
- inert buttons;
- tiny complicated cards where the actual working surface should be central;
- responsive desktop-only UI that fails when the Android keyboard opens;
- blindly copying external product screenshots/branding.

Prefer:
- one large work area with contextual assistant and action surfaces;
- contextual views (Forge issue/diff, Studio editor, Campus reasoning dock, Canvas transient chart, browser live view);
- staged complexity with collapsible expert tools;
- durable per-task state restoration and cross-device handoff;
- accessibility through keyboard, screen reader, meaningful statuses and touch.

## 15. Exact implementation workflow coding agents must follow

### Step 1: Product comprehension

Read this document fully. Identify which product experience the issue enables, its owning subsystem, the user-visible end-to-end outcome and every affected cross-subsystem boundary. Restate those in the PR plan; do not reinterpret scope as “just make a component render.”

### Step 2: Repository audit

Read [AGENTS.md](../AGENTS.md), [DESIGN.md](../DESIGN.md), relevant subsystem docs, `Cargo.toml`, actual crates, UI components, tests and issue history. Distinguish shipped from stubbed. Check dependent upstream libraries against pinned versions and licenses. Document conflicts between product intent and current code.

### Step 3: Write acceptance contract

Before coding, specify: user action, backend state transitions, real side effects, permissions, persistence/recovery, UI feedback, negative cases and verification command. Include what is *explicitly out of scope*. Decide whether the scope is one complete vertical slice; reduce unbounded breadth without cutting the critical end-to-end path.

### Step 4: Task branch and isolated work

Follow [AGENTS.md](../AGENTS.md) exactly: create `agent/<purpose>` branch from fresh base, preserve dirty user work, isolate worktrees when needed, never commit directly to main, produce a PR and leave merge to human review.

### Step 5: Build with typed contracts

Reuse existing Core events, Guardian grants, Mind routing, Arena memory, Flow job lifecycle and UI design components. Where contracts are missing, add narrow versioned definitions with explicit migration. Do not hide privileged effects behind model prompt text. Build for genuine offline/local operation.

### Step 6: Test actual behavior

Unit tests validate pure algorithms; integration tests verify boundaries; a real e2e test validates at least one successful user workflow. Additionally test unavailable model/tool, denial, cancellation, restart, duplicate delivery, stale revision, resource exhaustion and reconnection where relevant. Label untested platform claims. Record observed latency and memory only when measured.

### Step 7: Report evidence

PR must show scope, path to demo, tests run/results, contracts changed, known limitations, resource implications, security decisions, UI screenshots where applicable and the next dependency. Do not claim support because documentation or UI text says so.

## 16. Agent preflight checklist

Before making any code change, answer:

1. Which **user-visible SAIL experience** will improve, and how can the user prove it?
2. Which subsystem **owns** the feature? Which other subsystems must be **reused**?
3. Does this need a **new process, table, API, tool, service or tab**, and what existing component is insufficient?
4. Is a stated requirement **current reality**, **target design** or **unverified third-party capability**?
5. What data is being persisted and **why**? Are privacy/retention/deletion requirements satisfied?
6. Which operations require Guardian grants or approvals? Where is enforcement implemented?
7. How does the feature behave when the model/browser/network/device is unavailable?
8. How does it resume safely after restart/disconnect without duplicated effects?
9. Does the UI show **actual** execution and receipt, including errors and undo?
10. Has the vertical slice been tested on real SAIL contracts and at least one target platform?
11. Is there a **reviewable branch and PR**, not a direct-main patch?
12. Would this still behave correctly if the user never saw an internal model, agent name or tool log?

If any answer is missing, the implementation plan is incomplete.

## 17. Worked example: the wrong and right scope

**Issue: “Add memory to SAIL.”**

Wrong: append chat messages to JSON, embed each line and retrieve nearest messages. This confuses context, history and knowledge; it cannot reliably detect changed facts or user deletion and needlessly collects private text.

Right: use temporary turn context, extract claims and procedures, validate provenance, reconcile with existing Arena entities, propose a MemoryChangeSet, commit through Core storage under Guardian permissions and retrieve a small relevant context. Demonstrate conversation deletion with retained accurate knowledge, corrected claim revisions and no deleted-memory resurrection.

**Issue: “Add a browser.”**

Wrong: install Playwright as a dependency and give the LLM arbitrary navigation with a shared logged-in profile.

Right: authorize an ephemeral worker, enforce network and path isolation, provide typed `create/navigate/observe/act/extract/close` methods, return evidence, handle timeouts/cancellation and optionally show a preview. If user credentials are needed, request separate scoped profile authorization.

**Issue: “Implement Forge.”**

Wrong: ask a model for code; write a comment-only patch if it fails; run existing tests and call it solved.

Right: reserve a worktree and runtime budget, get an actual model patch, run issue-specific tests, diagnose real failures, compare diffs, request independent scoped review and deliver an unmerged PR with logs. Missing model means blocked, not passed.

**Issue: “Implement Studio.”**

Wrong: display a PDF/HTML screenshot and show a chatbot that says “I edited it.”

Right: open a real editable file, use typed selection-based operation, enforce revision conflict checks, save the actual file, display real change, and support undo and reopen.

## 18. References and known design tensions

- [Subsystem specification index](README.md): full product ownership and per-feature contracts.
- [Roadmap #15](https://github.com/PedroVictorCoding/SAIL/issues/15): taxonomy and cross-cutting issues; note later Health addition.
- [Core](SAIL_CORE.md), [Mind](SAIL_MIND.md), [Arena](SAIL_ARENA.md), [Agents](SAIL_AGENTS.md), [Guardian](SAIL_GUARDIAN.md), [Bridge](SAIL_BRIDGE.md), [Forge](SAIL_FORGE.md), [Discovery](SAIL_DISCOVERY.md), [Dialogue](SAIL_DIALOGUE.md).
- [Studio](SAIL_STUDIO.md), [Campus](SAIL_CAMPUS_LEARNING.md), [Craft Anything for Forge](SAIL_FORGE_CRAFT_ANYTHING.md), [Entertainment](SAIL_ENTERTAINMENT.md).
- [Repository README](../README.md): describes portions of the current executable system, including supervised Python speech workers and earlier ten-subsystem runtime; not a complete description of the 21-subsystem target.
- [DESIGN.md](../DESIGN.md): concrete UI design requirements.
- [AGENTS.md](../AGENTS.md): mandatory branch/PR workflow; must be followed even for docs.

### Decisions requiring explicit migration rather than assumptions

1. **Raw conversation index (#52) versus knowledge-first memory:** historical roadmap mentions conversation snapshots/vector index; the user later specified that raw conversations should not be the lasting memory. Design bounded ephemeral context and semantic consolidation; migrate prior indexing assumptions explicitly.
2. **Rust-only aspiration versus Python speech workers:** keep current voice working while moving to a Rust-supervised/no-required-Python architecture when verified feasible.
3. **Local-only inference versus subscription reviewer:** local model for ordinary tasks; Codex CLI allowed only in the narrow Forge role with durable quota reservation.
4. **Existing implementation's limited subsystem count versus 21-capability product taxonomy:** do not fabricate implementations or launch 21 processes. Deliver integrated features incrementally.
5. **Static documentation versus evolving product:** do not change original product goals just because current crates are incomplete. When stronger user decisions conflict with stale code/docs, propose a documented migration.
6. **External inspiration versus reusable code:** LearnHarness (AGPL), Notte (SSPL), Craft Anything (MIT/Apache) and Taranify (public product reference) have different licenses, maturity and APIs. Verify before integrating.
7. **Tool capability versus authority:** adapters may technically be able to read, execute or upload; permission still belongs to Guardian, not the adapter or LLM.
8. **Browser as runtime versus research context:** a browser can observe a site; it cannot automatically access system files or host credentials.
9. **Dioxus client versus the current web UI (decided 2026-11-02):** the shared Rust/Dioxus Web/Android client remains the target, but the migration starts once SAIL is stable and hardened. Until then, new UI goes into the existing `sail-dialogue/web` interface under [DESIGN.md](../DESIGN.md).
10. **Persistent data placement (decided 2026-11-03):** all persistent data goes under `/srv/sail-data` (`SAIL_DATA`), resolved by `sail_common::storage` and used only when it is a mounted filesystem. Hot paths (Forge worktrees, Cargo `target/`, active GGUF) stay on the `/srv/sail` SSD. The mount is group-owned (`sail-data`, `2770`) so the owner can manage it without root; `scripts/setup-sail-data.sh` sets this up. This replaces the earlier placement of task state on the runtime SSD.
11. **One resident model (decided 2026-11-03):** SAIL keeps a single large general model (Qwen3.6-35B-A3B) loaded for every subsystem, including Forge; Mind orders requests with live voice first, and voice pauses background work instead of failing it. No separate coding model is loaded beside or swapped in for it. Speech (VAD, STT, TTS) moves toward the client device where it can run there, with the host workers as fallback, rather than adding a weaker on-device conversational model. See [Mind](SAIL_MIND.md#one-resident-model).
12. **External models by escalation (decided 2026-11-03):** the owner's ChatGPT (Codex CLI), Claude (Claude Code) and Gemini (Gemini CLI) subscriptions and API keys are model providers beside the local model. Work starts locally, voice and dialogue included; a task escalates only after its local attempts fail its measurable checks a set number of times, only to a provider with quota, and the answering model is always shown. Subscriptions are used only through the vendor's own CLI, as text models in Guardian's sandbox. See [External models, gateway, skills and scheduling](SAIL_EXTERNAL_MODELS_GATEWAY_SKILLS.md).
13. **Messaging gateway (decided 2026-11-03):** the owner can reach SAIL from chat apps, Telegram first, through Relay with Guardian pairing (default deny) and approvals.
14. **Agent Skills format (decided 2026-11-03):** skills are `SKILL.md` folders per the open [Agent Skills](https://agentskills.io/specification) specification, portable to and from compatible agents such as Hermes. A skill never grants permission; self-created skills are staged for owner approval.
15. **Memory sent to external models (decided 2026-11-03):** only owner-marked shareable memory at `Normal` sensitivity, with names and identifiers redacted and an egress receipt per request. Health, people and private items never leave the host.

## 19. Final north-star acceptance test

A first-time user launches SAIL on Web/Android, grants only narrow permissions, talks naturally, sees an actual useful result, asks SAIL to carry forward a multi-step goal, closes the UI, comes back later to an accurate progress view, corrects a remembered fact, watches the change affect future reasoning, opens a scoped real document or code task, sees an auditable edit, and can safely revoke access or stop the agent. The system remains useful on modest hardware, respects offline modes, and never confuses an intention with a completed action.

**Every scoped implementation should advance this coherent experience without fragmenting the architecture.**

---

## Appendix A — Canonical task/agent vocabulary

- **Observation:** result actually obtained from an authorized tool or user input.
- **Evidence:** permissible, provenance-tagged support for a claim; may be a receipt or an artifact, not necessarily transcript text.
- **Claim:** proposition about an entity, source and epistemic status.
- **Decision:** selected intended approach with owner/reason, not proof of deployment.
- **Task:** durable unit of work with identity, scope, state, permissions and recovery semantics.
- **Tool:** typed capability; invocations are scoped and permissioned.
- **Action receipt:** objective outcome from executor, not self-report from model.
- **Artifact:** persisted file or work product with revisions and ownership.
- **Space:** user-facing contextual workspace spanning multiple subsystem owners.
- **Subsystem:** backend/product ownership boundary; not necessarily a separate service or tab.
- **Memory:** maintained structured knowledge and relevant history with retention and correction controls.
- **Context:** temporarily assembled information for a model/agent run; may derive from memory and live observations.
- **Skill:** approved reusable typed procedure/workflow; not automatically privileged.
- **Sandbox:** enforced process/resource boundary, not an LLM instruction.
- **Autonomy:** scheduling/choice within granted authority; never unrestricted authority.

## Appendix B — Minimum real end-to-end receipts

| Workflow | Evidence of actual success |
|---|---|
| Conversation + voice | Captured permissioned audio → recognized text → generated response → actual played audio, interruption tested |
| Memory update | Supported claim changes in durable store → new retrieval reflects it → deleted claim absent from indexes and pending jobs |
| Coding | Actual functional diff + issue-specific passing tests + PR or reviewed artifact; original source safe |
| Browser | Navigated real allowed page; returned observation screenshot/structured state; closed isolated session; policy enforced |
| Studio | Native document changed, saved, reloaded, and undo verified |
| Campus | Probe answer persisted, relevant plan graph projected, lesson check resolved, independent practice scored |
| Discovery | Sourced useful suggestion delivered once, snooze retained, no quiet-hours notification |
| Flow | Authorized DAG nodes execute with receipts; restart resumes without replaying committed side effects |
| Model routing | Correct local model selected with real memory/latency metrics and available fallback |
| Media | Actual permitted player action or honest link; resume state verified |

## Appendix C — External references and adaptable candidates

These outside projects may inform SAIL designs or, after review, be adapted into it. Listing a project here is **not** approval to add it as a dependency, copy its code or treat its claims as verified. Before reuse, follow §15 Step 2: check the pinned version, the license and how well it fits the owning subsystem's contracts, then record the decision in that subsystem's spec.

| Resource | Possible use in SAIL | Owner | Status | License / reuse notes |
|---|---|---|---|---|
| [nottelabs/notte](https://github.com/nottelabs/notte) | Observe-act-extract browser automation ideas for the headless browser worker (§7.2) | Bridge, authorized by Guardian | Architecture reference only | SSPL-1.0; Python-first SDK with a Node SDK. Do not add as a dependency or copy code. Build browser control as Rust-owned CDP orchestration. |
| [LearnHarness](https://learnharness.ai) ([source](https://github.com/amosblomqvist/learnharness)) | Teaching workflow (probe → plan → teach/check → practice) and UI reference for Campus (§10) | Life/Campus | Pedagogical/UI reference | AGPL-3.0 source repo; TypeScript/Next.js/Electron. Do not port it as a whole. Audit assets and AGPL obligations before any reuse. See [Campus spec](SAIL_CAMPUS_LEARNING.md). |
| [muftiarfan/noop](https://github.com/muftiarfan/noop) | Offline WHOOP strap pairing and on-device storage, related to Health ingestion (#105) | Health (Android ingest via Bridge) | Candidate to evaluate | No license file found as of 2026-11-02 (README: "as-is" for personal/educational use). Treat as all rights reserved: study the behavior only, no code reuse unless a license is granted. Swift/macOS app plus a Kotlin Android app; unofficial and not affiliated with WHOOP. |
| [NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent) | Reference for the messaging gateway and pairing, skills, cron-style scheduling and memory hygiene ([design](SAIL_EXTERNAL_MODELS_GATEWAY_SKILLS.md)) | Relay, Flow/Skillsmith, Pulse, Arena | Architecture reference; skill format adopted via the open spec | MIT. Python. Compatibility is through the Agent Skills format, not a code dependency. |
| [mahseema/awesome-ai-tools](https://github.com/mahseema/awesome-ai-tools) | List for finding candidate tools; not a component | No single owner (whichever subsystem picks a tool) | Discovery list | MIT applies to the list itself. Each listed tool has its own license and maturity and must be evaluated separately. |

---

*This is a living design contract. Changes to its normative product definitions should be explicit and reviewed, not inferred from whichever model happens to be editing a subsystem.*
