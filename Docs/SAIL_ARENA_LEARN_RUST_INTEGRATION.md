# Arena Learn — LearnHarness integration, full Rust, and the Campus page

**Status:** design for implementation · **Owner:** Arena Learn (`crates/sail-arena-learn`) ·
**Supersedes nothing:** extends [SAIL Arena Learn](SAIL_ARENA_LEARN.md) (§2 layout, §4 events,
§7 milestones L2–L4) · **UI target:** `crates/sail-dialogue/web` now, Dioxus later (#38).

**Upstream reference:** <https://github.com/amosblomqvist/learnharness> — AGPL-3.0.
This is a **behaviour-level clean-room reimplementation**. No upstream source, prompts,
CSS, SVG or image assets are copied. Where this document names an upstream concept
(*probe*, *plan*, *practice*, *proposition*, *chain*), only the observable behaviour is
adopted, re-expressed in SAIL's own types and pedagogy. Legal review is required before
anything beyond behavioural parity is taken.

**Read with:** [Master Vision](SAIL_MASTER_VISION.md) §10, [Arena Learn](SAIL_ARENA_LEARN.md),
[Design rules](../DESIGN.md), [UI tokens](../crates/sail-dialogue/web/tokens.css).

---

## 0. What exists today (verified against `main`, 2026-11-04)

| Piece | Path | State |
| --- | --- | --- |
| Domain core (events, plan, question, view) | `crates/sail-arena-learn/src/core/` | Implemented |
| Roles + tool registry (8 tools) | `src/agent/tools.rs` | Implemented, exact allowlists |
| Turn runner + judge | `src/agent/turn.rs`, `judge.rs` | Implemented, `free_recall` rubric + follow-up |
| SQLite event store, idempotent keys | `src/store/sqlite.rs` | Implemented, `SCHEMA_VERSION = 2` |
| HTTP + SSE | `crates/sail-dialogue/src/learn_http.rs` | 8 routes |
| Page | `web/learn.js`, `index.html` `#learnPanelMain` | L1 slice |

The engine is **L1-complete**: goal → probes → plan → explain/quiz → judge → pause/resume,
with the browser rendering a projection and never owning transcript truth.

### Gap against LearnHarness

| LearnHarness capability | Today in SAIL | Consequence |
| --- | --- | --- |
| Knowledge that carries forward (propositions + chains, session provenance) | **Missing** — plan is session scaffolding only | Second session starts from zero; Arena never learns |
| Research briefs, PDF reading, visual explanations, interactive demos | **Missing** — `explain` is text only | Weakest explanation format wins |
| Learner images with arrows/labels | **Missing** | No diagram-based teaching |
| Practice: pre-committed model solution, multiple attempts, delayed hints | **Partial** — `problem`/`conclude` are empty-argument stubs | Practice cannot give honest feedback |
| Notes import + coverage fingerprints | **Missing** | No imported material, no re-extraction policy |
| Graph view | **Missing** | The accumulated knowledge is invisible |

Everything below closes those six rows. Nothing else is added.

---

## 1. Architecture

```
sail-dialogue (HTTP/SSE, web/*)
      │  /api/learn/*
      ▼
sail-arena-learn ── core/  (pure: no IO, no clock, no model)
      │            ├── events.rs      + Artifact, Graph, Practice variants   (v3)
      │            ├── artifact.rs    NEW  Artifact + validation
      │            ├── graph.rs       NEW  Proposition / Chain assertions
      │            ├── practice.rs    NEW  ProblemSpec, PracticeAttempt
      │            └── view.rs        + artifacts, practice, graph_summary
      │  agent/  (ports only)
      │            ├── tools.rs       8 → 13 tools, per-role allowlists
      │            ├── artifact_jobs.rs NEW orchestration of background artifacts
      │            └── extract.rs     NEW  post-turn graph extraction
      ▼
ports: Mind (inference), Arena (graph write, `MemoryPurpose::Learn`), Canvas (artifact host),
       Continuity (durable jobs), Guardian (path/search grants)
```

**Dependency rules, unchanged:** `core/` stays pure (there is a `core_purity_test.rs` that
enforces it). Only `agent/` talks to ports. `sail-arena` gains no model dependency.
`core/` may gain `artifact.rs`, `graph.rs`, `practice.rs`; tests must keep proving purity.

### 1.1 New port methods

```rust
// core/ports.rs — additions only
pub trait GraphWriter {
    /// Idempotent by (session, assertion_id). Returns how many nodes/chains were new.
    fn assert(&self, session: &SessionId, batch: &GraphBatch) -> Result<GraphReceipt, GraphError>;
}

pub trait ArtifactHost {
    /// Persist a visual/demo into Canvas-local storage; never execute it.
    fn store(&self, session: &SessionId, artifact: &Artifact) -> Result<ArtifactRef, ArtifactError>;
}

pub trait SourceReader {
    /// Read an approved path through Bridge, bounded by pages/bytes.
    fn read(&self, path: &ApprovedPath, budget: ReadBudget) -> Result<SourceDoc, SourceError>;
}
```

`Research` is **not** a port in `core/`: it is an `agent/` job that calls Bridge's approved
search tool, exactly like Forge calls its own tools. Keeping it out of `core/` is what lets
the domain stay testable with fakes.

---

## 2. Data model

### 2.1 Events (`SCHEMA_VERSION = 3`)

```rust
pub enum LearningEvent {
    // … v2 variants unchanged …

    /// A background artifact finished. `status` distinguishes a real result from a
    /// failure the learner must see; nothing is ever synthesised in its place.
    ArtifactDelivered {
        call_id: String,
        artifact: Artifact,
        status: ArtifactStatus,      // Ready | Failed { reason } | Refused { reason }
    },

    /// One extraction batch, applied to Arena. Replay of the same `batch_id` is a no-op.
    GraphAsserted {
        batch_id: String,
        nodes: Vec<PropositionAssertion>,
        chains: Vec<ChainAssertion>,
        checkpoint: String,          // fingerprint of the teacher text it came from
    },

    /// A practice attempt *before* grading: intermediate work is learner evidence.
    PracticeAttempted {
        problem_call: String,
        text: String,
        confidence: Option<u8>,
        thinking: Option<String>,
    },

    /// The practitioner's verdict on one attempt.
    PracticeGraded {
        problem_call: String,
        attempt_seq: u64,
        verdict: PracticeVerdict,    // Correct | Partial | Incorrect | Unclear
        feedback: String,
        missing_relation: Option<String>,
    },

    /// Learner attached an image (path granted through Bridge).
    ImageAttached { call_id: String, image: ImageRef },
}
```

`is_input()` gains `ArtifactDelivered`, `PracticeGraded`. `PracticeAttempted` is input only
when the practitioner owes a verdict — keep that explicit in the turn runner, not implicit.

### 2.2 Core types

```rust
// core/artifact.rs
pub enum Artifact {
    Research { question: String, brief: String, sources: Vec<SourceRef> },
    Pdf      { path: String, question: String, brief: String, pages_read: Vec<u16>,
               pages_consulted: Vec<u16> },
    Visual   { notes: String, svg: String },          // sanitised, no <script>, no <foreignObject>
    Demo     { title: String, notes: String, html: String }, // sandboxed iframe only
    LearnerImage { source: ImageRef, annotations: Vec<Annotation> }, // arrows/labels
}

pub struct Annotation { pub kind: AnnotationKind, pub x: f32, pub y: f32,
                        pub to_x: Option<f32>, pub to_y: Option<f32>, pub label: String }
```

**Validation is a hard gate, not a warning.** `Artifact::validate()` rejects: SVG containing
`script`, `foreignObject`, `on*=` attributes, external `href`; demo HTML over 256 KiB or
containing `<script src>`, `fetch(`, `XMLHttpRequest`, `import(`; research without sources;
PDF paths not passed through `ApprovedPath`. Rejected artifacts are stored with
`Status::Refused` so the learner sees "the tutor's diagram was blocked", never a blank card.

```rust
// core/graph.rs
pub struct PropositionAssertion {
    pub id: String,          // stable, content-derived: prop_<blake3-prefix>
    pub name: String,
    pub statement: String,
    pub aliases: Vec<String>,
    pub session: String,
    pub message: String,     // empty = chat-level attribution
}
pub struct ChainAssertion {
    pub id: String,
    pub premises: Vec<String>,   // ALL used jointly
    pub conclusion: String,
}
```

Merging follows the upstream semantics SAIL actually needs: first assertion for an id defines
name/statement; later statements and aliases union with exact-string dedup; provenance unions
in encounter order; **different chains with the same conclusion stay separate arguments.**
This logic lives in `core/graph.rs` and is unit-tested without a database.

### 2.3 SQLite (v3 migration, additive)

```sql
ALTER TABLE sessions ADD COLUMN mode TEXT NOT NULL DEFAULT 'assess';  -- assess|learn|practice|review

CREATE TABLE artifacts (
  id TEXT PRIMARY KEY, session TEXT NOT NULL, call_id TEXT NOT NULL,
  kind TEXT NOT NULL, status TEXT NOT NULL, payload TEXT NOT NULL,
  created_ms INTEGER NOT NULL
);
CREATE INDEX artifacts_session ON artifacts(session, created_ms);

CREATE TABLE practice_attempts (
  id INTEGER PRIMARY KEY AUTOINCREMENT, session TEXT NOT NULL,
  problem_call TEXT NOT NULL, attempt_seq INTEGER NOT NULL,
  text TEXT NOT NULL, confidence INTEGER, thinking TEXT,
  verdict TEXT, feedback TEXT, missing_relation TEXT, created_ms INTEGER NOT NULL,
  UNIQUE(session, problem_call, attempt_seq)
);

-- Outbox: extraction is applied to Arena exactly once, even across a crash.
CREATE TABLE graph_outbox (
  batch_id TEXT PRIMARY KEY, session TEXT NOT NULL, payload TEXT NOT NULL,
  created_ms INTEGER NOT NULL, applied_ms INTEGER
);

CREATE TABLE sources (
  id TEXT PRIMARY KEY, session TEXT NOT NULL, path TEXT NOT NULL,
  sha256 TEXT NOT NULL, pages INTEGER, imported_ms INTEGER NOT NULL
);
```

`graph_outbox` is the durability point: the event is committed locally first, then applied to
Arena, then marked. A daemon crash between the two replays on next open. This mirrors the
`begin_effect`/`commit_effect` discipline in [Continuity](SAIL_CONTINUITY.md) rather than
inventing a second one.

### 2.4 Arena mapping

Graph writes go to Arena under `learning:*` scopes with `MemoryPurpose::Learn`, one batch per
extraction, carrying `session` + `message` provenance. SAIL does **not** add a second graph
store: Arena's propositions, evidence ledger and retrieval are the substrate.
A node's *birth session* is its first provenance point, not its earliest timestamp.

---

## 3. Tool surface (8 → 13)

| Tool | Role | Arguments | Ends turn | Notes |
| --- | --- | --- | --- | --- |
| `probe` | Prober | question | yes | unchanged |
| `ask_user_question` | Prober, Teacher | question | yes | unchanged |
| `probe_done` | Prober | summary, known[], gaps[] | no | unchanged |
| `plan_init` | Teacher | goal, nodes[], edges[] | no | unchanged |
| `explain` | Teacher | node_id, text | no | unchanged |
| `quiz` | Teacher | question + node_id | yes | unchanged |
| `problem` | Practitioner | statement, target_nodes[], model_solution, hint_ladder[] | no | **changed: was `{}`** |
| `conclude` | Practitioner | verdict, feedback, missing_relation | no | **changed** |
| `research` | Teacher | question | no | spawns durable job; card appears when done |
| `read_pdf` | Teacher | path, question, pages? | no | Bridge-granted path only |
| `make_visual` | Teacher | notes, svg | no | sanitised; Canvas-local |
| `make_demo` | Teacher | title, notes, html | no | sandboxed iframe, no network |
| `show_image` | Teacher | source, annotations[] | no | learner-owned images |

**The Prober stays read-only and artifact-free.** Assessment must never depend on a diagram
the assessor generated; it would let the prober teach while measuring.

`problem` carrying `model_solution` is the important change: the solution is committed
*before* any attempt, so `conclude` cannot rationalise the learner's answer after the fact.
`hint_ladder` is ordered and revealed one rung at a time on request, never automatically.

---

## 4. Turn pipeline

```
answer | message | resume
   ↓
[1] append input event
[2] if free_recall and unanswered → judge (isolated call: question + rubric + answer)
   ↓ verdict, follow-up at most once
[3] role turn: Mind stream → tool calls
   ├─ question tool      → QuestionAsked, await learner, stop
   ├─ artifact tool      → Continuity job id, ToolResult{accepted}, continue
   └─ other              → validate + apply, continue
[4] after turn settles: extraction job (if enabled and new teacher prose exists)
   → GraphAsserted → Arena, via outbox
[5] projection recomputed, SSE delta
```

Budgets: one artifact job in flight per session, three total; research/PDF capped by
`ReadBudget`; extraction sees at most the last N teacher messages and is skipped when the
text is unchanged (coverage fingerprint in `sources`/checkpoint).

Failure rules, unchanged in spirit: model offline → draft preserved, `last_failure` set,
nothing fabricated; judge unusable → one retry, then `could_not_judge`; artifact job fails →
`ArtifactDelivered{status: Failed}` and the tutor is told, not silently retried forever.

---

## 5. HTTP API (additive)

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/learn/sessions/:id/artifacts` | Artifact list (metadata + payload for ready) |
| `GET` | `/api/learn/artifacts/:id/raw` | Sanitised SVG / demo HTML for the sandboxed frame |
| `POST` | `/api/learn/sessions/:id/notes` | Import a Studio/PDF note → `sources` + extraction job |
| `GET` | `/api/learn/sessions/:id/graph` | Propositions/chains born in this session |
| `POST` | `/api/learn/sessions/:id/practice` | Start practice over selected nodes → new practice session |
| `POST` | `/api/learn/sessions/:id/attempt` | `{problem_call, text, confidence?, thinking?}` |

SSE delta gains event names: `artifact`, `graph_updated`, `practice_feedback`.
`GET /sessions/:id/stream` already exists; only the vocabulary grows, so `learn.js`'s
`learnParseSse` keeps working unchanged.

---

## 6. The page

Placement: **Arena → Learn tab** (existing `#learnPanelMain`). No new navigation entry.

### 6.1 Desktop layout (≥ 1080 px)

```
┌ Arena · Learn ───────────────────────────────────────────────────────────────┐
│  Goal: "How TCP congestion control works"      [Assess][Learn][Practice][Review] │
│  ● Your turn · question 3 of ~7        [Pause] [Stop]                          │
├───────────────┬──────────────────────────────────────┬────────────────────────┤
│ LEARNING MAP  │ LESSON                               │ CONTEXT                │
│ (inset rail)  │  transcript (prose, equations, SVG)  │  Ask / dictate         │
│ ✓ Covered     │  ▸ artifact card (visual / demo)     │  Linked sources        │
│ ◉ Now         │                                      │  Graph preview         │
│ ○ Next        │  ┌ QUESTION DOCK ─────────────────┐  │  Learning activity     │
│               │  │ prompt                          │  │                        │
│ [diagram ▾]   │  │ ○ A   ○ B   ○ C                 │  │                        │
│               │  │ thinking: ______________________ │  │                        │
│               │  │ confidence 1—5                  │  │                        │
│               │  │ [Submit]  [I don't know]        │  │                        │
│               │  └─────────────────────────────────┘  │                        │
└───────────────┴──────────────────────────────────────┴────────────────────────┘
```

Grid: `grid-template-columns: 248px minmax(0,1fr) 312px;` collapsing to one column < 1080 px.
Left rail is **inset** (`--depth-inset`), the lesson column is the calm reading surface (flat),
and only the primary action is raised (`--depth-control`). Per `DESIGN.md`: depth earns
hierarchy, it does not decorate every card.

### 6.2 Component tree (existing ids preserved)

```
#learnPanelMain
├─ .learn-start            #learnStartForm  (goal, start)          ← exists
├─ .learn-sessions         #learnSessions   (resume list)          ← exists
└─ .learn-session          #learnSession
   ├─ .learn-goal-line     #learnGoalTitle #learnMode .learn-controls
   ├─ .learn-modes         NEW  role=tablist: Assess|Learn|Practice|Review
   ├─ .learn-status-line   #learnStatus role=status aria-live=polite
   ├─ .learn-layout
   │  ├─ nav.learn-map     #learnSteps #learnDiagram               ← exists
   │  ├─ .learn-main       #learnTranscript #learnDock             ← exists
   │  │  ├─ .learn-artifact  NEW  figure > svg|iframe[ sandbox ] > figcaption
   │  │  ├─ .learn-attempts  NEW  ol of practice attempts + verdict chips
   │  │  └─ .learn-dock      existing form, extended fields
   │  └─ aside.learn-context NEW #learnSources #learnGraphPeek
   └─ .learn-message       #learnMessageForm
```

### 6.3 Question dock states

| State | Rendering | Semantics |
| --- | --- | --- |
| `idle` | dock hidden | — |
| `asking` | prompt + options + thinking + confidence + Submit / I don't know | `aria-live=polite` announces prompt once |
| `judging` | Submit disabled, "Checking your reasoning…" | never fake a verdict |
| `answered` | chosen option marked, correct/partial shown, thinking retained editable? **no** | answers are immutable once accepted |
| `unclear` | follow-up question, same node, one per question | label "One more try" |
| `error` | retry button, draft preserved | no silent loss |

Options are **shuffled server-side**; the display index is never applied to authored order.
Confidence is 1–5 (SAIL's existing `Answer::validate` range), not 0–100; the UI shows
"not sure ————— certain". `thinking` is a real textarea, not an input: reasoning is the point.

### 6.4 Artifact cards

```
┌─────────────────────────────────────────────┐
│ ▸ Visual · the sliding window               │
│   [ sanitised SVG, max-height 320px ]       │
│   Source: session, step 4        [Expand]   │
└─────────────────────────────────────────────┘
```

- Visual/demo render inside `sandbox="allow-scripts"` **without** `allow-same-origin`;
  demos get `CSP: default-src 'none'; style-src 'unsafe-inline'` via the raw endpoint.
- Research: brief + numbered sources; links open in a new tab with `rel="noopener"`.
- PDF: question, brief, pages read/consulted as chips — citations, not the document.
- Failed/refused: a muted card saying what was blocked and why, with `Retry` when safe.

### 6.5 Graph view (Review mode)

Two representations of the same data: a **linear outline** (default, keyboard accessible:
`ul` of propositions with premise links) and a **diagram** toggle rendered from chains —
not a second graph engine; reuse Arena's renderer. Edges drawn only to express chains;
a chain with joint premises shows a junction, because pooling premises would misrepresent it.
Node detail: statement, aliases, birth session, first-seen date, user correction, conflict flag.

### 6.6 Visual tokens (uses existing `tokens.css`, no new palette)

| Element | Token |
| --- | --- |
| Page | `--bg` |
| Reading column | transparent on `--bg` |
| Map rail / dock inputs | `--recessed`, `--depth-inset` |
| Primary button | `--accent` text on `--control`, `--depth-control` |
| Focus | `2px solid var(--focus)` + 2px offset |
| Status: your turn | `--speaking` dot |
| Status: thinking | `--thinking` dot |
| Status: correct / partial / wrong | `--good` / `--warning` / `--accent` |
| Motion | `--transition-fast`, disabled under `prefers-reduced-motion` |

Correctness is never colour-only: chips carry text ("Correct", "Partly — the missing link is…").

### 6.7 Mobile (< 1080 px)

Single column. The map becomes a `<details>` sheet above the lesson; the dock sticks above the
keyboard with `env(safe-area-inset-bottom)`; context moves to a bottom sheet. Draft answers and
scroll position survive reconnect (server owns the question, client owns the draft).

### 6.8 Accessibility checklist (acceptance, not aspiration)

- Every question is reachable and submittable by keyboard; focus moves to the prompt, then to
  the first option; `Esc` does not discard a draft.
- `aria-live="polite"` only for status and newly arriving questions — not for streaming prose.
- Contrast AA on all text including muted hints on `--bg`.
- `prefers-reduced-motion` removes card entrance and dot pulsing.
- Diagram has an accessible text alternative; the outline is the source of truth.
- Touch targets ≥ 44 px.

---

## 7. Implementation plan (six PRs)

| # | Deliverable | Acceptance |
| --- | --- | --- |
| 1 | `core/artifact.rs`, `graph.rs`, `practice.rs` + event v3 + migration | purity test green; v2 stores open and replay identically; invalid SVG/demo rejected by tests |
| 2 | Tools 8→13, role allowlists, `problem`/`conclude` real schemas | `allowlists_are_exact` updated; prober still cannot reach artifacts; args validated |
| 3 | Artifact jobs over Continuity + Canvas host + `/raw` sandbox endpoint | crash mid-job recovers once; blocked payload shows Refused; no network from demo |
| 4 | Extraction job + `graph_outbox` + Arena write under `learning:*` | duplicate batch is a no-op; provenance lands on the right message; Arena untouched when extraction disabled |
| 5 | Page: modes, dock states, artifact cards, attempts, context rail | keyboard-only run; reconnect mid-question; mobile sheet; reduced-motion |
| 6 | Graph view + notes import + practice scheduler (Pulse opt-in) | outline/diagram agree; imported note shows provenance; no mastery score anywhere |

Tests to add per PR: pure unit (core), projection replay (`tests/projection_test.rs`), store
migration (`store_test.rs`), engine end-to-end (`engine_test.rs`), HTTP acceptance
(`learn_acceptance_test.rs`), one real browser pass and one Android pass at PR 6.

---

## 8. Explicit non-goals

- No mastery score, ability rating, or predicted grade — the record holds evidence, not verdicts.
- No second knowledge store beside Arena.
- No arbitrary HTML/JS execution outside the sandboxed frame, ever.
- No copying of upstream prompts, CSS, SVG or assets (AGPL-3.0).
- No new top-level navigation entry; Learn stays inside Arena.

---

## 9. Risks

| Risk | Mitigation |
| --- | --- |
| Extraction hallucinates propositions | Teacher-authored text only; assertions carry provenance; user can correct/delete in Arena |
| Artifact payloads become an XSS vector | Sanitise on write **and** on read; sandbox without same-origin; CSP on `/raw` |
| Graph write fails after event commit | `graph_outbox` replay, idempotent `batch_id` |
| Model spend during extraction | Bounded window, coverage fingerprint, disabled by default in settings |
| Long lessons exceed context | Existing compaction path; the event log remains complete |
| Duplicate UI with `memoryPanelMain` | Learn shows *session* knowledge; Memory shows *Arena* entities; the graph view links to Memory, never duplicates it |
