# SAIL Campus Learning — Adaptive Learning Experience and UI Specification

**Status:** Proposed; upstream-based architectural design, not implemented. **Owner:** Life/Campus with Arena, Agents, Mind, Canvas and Dialogue. **UI target:** Dioxus Web + Android. **Reference:** https://github.com/amosblomqvist/learnharness (AGPL-3.0; TypeScript/Next.js/Electron).
**Links:** [Life](SAIL_LIFE.md), [Arena](SAIL_ARENA.md), [Canvas](SAIL_CANVAS.md), [Dialogue](SAIL_DIALOGUE.md), [Mind](SAIL_MIND.md), [Guardian](SAIL_GUARDIAN.md).

## 1. Purpose and upstream verification

Build a **SAIL Campus learning experience** inspired by LearnHarness's UI/pedagogical workflow, not a new top-level SAIL subsystem or a wholesale TypeScript/Electron port.

Verified from upstream README and current architecture/teaching documentation (2026-11-02):
- LearnHarness assesses (probe), plans a dependency path, teaches in small reasoning steps with comprehension checks and repairs, and runs separate practice sessions with varied transfer problems.
- Learners can explain reasoning and uncertainty through free-form text or dictation. A persistent graph stores **propositions and chains of reasoning**, with links to originating sessions, rather than a reductive user ability score.
- Its interface contains persistent/contextual plan-tree navigation, a question dock, practice cards/dock, a conversation view, graph view, optional interactive visual demonstrations and research-job activity indicators.
- The released reference is Next.js + Electron, TypeScript and Pi agents; models can be configured through provider endpoints including local OpenAI-compatible servers. Optional dictation and web search may rely on separate services.
- The published code is AGPL-3.0. Do not copy protected implementation or UI assets into SAIL without legal/license review and compliance. Prefer clean-room behavior-level reimplementation in SAIL's own Rust/Dioxus architecture.

Source-derived learning theory is **not established scientific consensus**. In particular, LearnHarness's formulation that learning is complete when a proposition “feels obviously true” is its author's pedagogical theory, not an independently validated objective measurement. SAIL should use learner-expressed understanding as one signal alongside demonstrated recall, error analysis and later transfer.

## 2. Architecture and boundary ownership

Campus belongs within **Life** and is presented as an existing Campus Space; no new 22nd subsystem. The reusable mechanisms are:

- **Campus (Life):** courses, learning goals, study sessions, prerequisites, diagnostic coverage, learning plans, practice orchestration and assessment scheduling.
- **Arena:** evidence-attributed propositions, prerequisite and inference edges, retrieval of prior learning and user-controlled revision/deletion. Graph stores knowledge representations and their provenance; avoid unsupported “mastery scores”.
- **Mind:** local model role routing (prober, planner/teacher, evaluator/practitioner); use small model where adequate, Qwen for harder reasoning. Never require hosted inference.
- **Agents/Flow:** bounded source retrieval, task orchestration and background jobs; typed tools and durable Continuity lifecycle.
- **Canvas:** diagrams, interactive demonstrations and artifacts; learning visuals are contextual, not persistent document edits.
- **Dialogue:** PT-PT/English narration, dictation and optional voice-first checks with barge-in.
- **Guardian/Bridge:** authorize imported material and optional web lookup; never secretly upload source files or transcripts.
- **Pulse:** voluntary spaced practice scheduling and quiet-hour-aware reminders.
- **Studio:** user-editable notes/workbooks; Campus can reference Studio files but never silently change them.

## 3. UI strategy — one learning workspace, multiple modes

**Entry:** Spaces → Campus → Learning, or “Teach me X” from existing SAIL chat. Preserve existing SAIL shell, Orb, conversational right panel and dark minimal layout. No separate global navigation entry named LearnHarness.

### Desktop shell (conceptual)

```text
┌──────── SAIL · Campus / Topic ────────────────────────────────────┐
│ Goal: Understand TCP congestion control    Assess Learn Practice │
├─────────────────────────┬───────────────────────┬────────────────┤
│ LEARNING MAP            │ LESSON / EXPLANATION  │ SAIL CONTEXT   │
│ current prerequisite    │ focused question      │ ask / dictate  │
│ mastered? not assumed   │ thinking field        │ linked sources │
│ visited/current/next    │ reveal hint, submit   │ visual tools   │
├─────────────────────────┴───────────────────────┴────────────────┤
│ Current action: answer · reasoning · confidence · next / pause   │
└──────────────────────────────────────────────────────────────────┘
```

**Mobile/Android:** One primary column. Compact progress strip opens Learning Map as a sheet; question/answer dock stays accessible above keyboard and safe area; chat/tools and sources are bottom sheets. No permanent three-column layout. Resume session and draft answers after reconnect.

### Start screen
- Enter learning goal and optional target date, course or source PDFs.
- Choose **Learn** / **Practice** / **Review my knowledge**. Default Learn.
- Show existing relevant concepts from Arena with clear provenance, not invented familiarity.
- One prominent action to begin; advanced choices collapsed.

### Assess / diagnostic probing
- Small targeted questions that locate missing prerequisites; adapt each follow-up to answers and free-form reasoning.
- Do not grade all uncertainty as failure; allow “I don't know”, “I think…”, confidence and an optional thinking-aloud field.
- Explicit reveal of why a question was asked when requested.
- Session can stop early when starting point is sufficiently established; show diagnostic uncertainty rather than presenting certainty.
- Progress indicates diagnostic coverage, not pseudoscientific intelligence/mastery scores.

### Learning map
- Persistent **compact mini-map** or narrow rail near the lesson with visited/current/upcoming prerequisite clusters.
- Tap to expand a full DAG with pan/zoom, keyboard accessible linear outline, search, concept details and source/reasoning links.
- Distinguish *visited* from *demonstrated understanding*. A correct single answer does not automatically mean “mastered”.
- Highlight dependencies that a misunderstanding blocks; re-plan transparently when new evidence reveals missing prerequisites.
- Use existing Arena graph renderer or a focused Canvas visual, not another graph engine if avoidable.

### Teach
- Present each concise conceptual/reasoning step in its clearest format: prose, equation, diagram, analogy, worked example or simulation.
- Prompt a prediction/check at an appropriate cadence; allow user to reveal check manually when mid-explanation.
- **Dedicated question dock**, not a form buried inside long chat history. One active learner question at a time.
- Show multiple choice or free-response depending on what best tests the concept. Let learner explain *why*, not only choose.
- On error, diagnose the specific missing relation, present repair and recheck before proceeding. No ritualized “correct!” spam.
- Support “Explain differently”, “Give example”, “Show derivation”, “Let me try”, “Too easy/hard”, “Pause”.
- Use source-backed explanations and explicitly identify uncertain or contested claims.

### Practice
- Separate independent retrieval/transfer mode, not an endless quiz within the teaching transcript.
- Mix problems with different representations, hide prerequisite labels and delay hints so unaided retrieval is measurable.
- Provide post-attempt solutions, reasoned feedback, partial credit and honest uncertainty.
- Session review shows errors and concepts to revisit; optionally schedule future practice via Pulse.

### Graph / knowledge continuity
- Represent sourced propositions and directed justification/prerequisite edges, not just course titles.
- Source link to session/question/notes, origin timestamp, user correction and conflict flags.
- Reuse Arena's existing provenance/evidence ledger rather than creating a duplicate learner store.
- Store answers and reasoning with clear privacy controls. Support export and deletion.
- Do not unconditionally inject the entire knowledge graph into every prompt; retrieve a bounded, relevant and permissioned subset.

### Research, audio and visuals
- Async research/document parsing may run while teaching continues; visible activity dock shows the actual job state and source provenance.
- Dioxus Canvas can render structured LaTeX, Mermaid, charts and ephemeral demos through constrained components; arbitrary untrusted HTML/JS requires sandboxing and consent.
- Voice narration is optional; fixed text uses dedicated EU-PT/English TTS, not conversational model generation when exact words matter.
- Dictation is opt-in and uses the existing local speech pipeline when available. Explicit recording state.

## 4. Application state and typed event contracts

Model the session state as a durable event-driven projection rather than frontend-owned transcript truth:

```rust
enum LearningMode { Assess, Plan, Teach, Practice, Review }
enum LearningEvent {
    GoalSet { goal_id: String },
    ProbeAsked { question_id: String, concept_id: Option<String> },
    LearnerAnswered { question_id: String, answer_id: String },
    ReasoningProvided { question_id: String, text: String },
    PlanUpdated { revision: u64 },
    ConceptPresented { concept_id: String, step_id: String },
    CheckResolved { question_id: String, outcome: CheckOutcome },
    PracticeCompleted { session_id: String },
    SessionPaused,
}
```

Illustrative only: reuse existing Core IDs/bus and persist event origin, ordering, immutable revision and idempotency keys. Keep plan revision distinct from current viewport. Enforce one active unanswered question, replay-safe answers and no phantom completed checks. A client disconnect must detach the view, not silently cancel the learning turn. On reconnect, project persisted state and recover answer submission safely without double-grading.

## 5. Local model orchestration

- Define typed role-specific prompts/tools: diagnostic prober (read-only), planner/teacher (plan mutation, checks, visual request), practitioner (question evaluation/feedback).
- Roles may share a session context but should expose only necessary tools, preventing the assessor from mutating project files or granting capabilities.
- Use local OpenAI-compatible endpoints / llama.cpp adapters already owned by Mind. Respect memory residency and voice priority on i5-10400 + 32 GB RAM.
- Ensure model-generated graph links, answers and scores are validated. Answers are not automatically facts; cite imported sources.
- If model is offline, preserve draft and show unavailable/retry, never fabricate feedback.

## 6. Product integration use cases

1. **Computer engineering:** import course PDF with consent → discover prerequisites (BGP, NAT, networking) → interactive diagnostic → explain one mechanism → test edge cases → graph record with PDF page provenance.
2. **Philosophy:** connect existing structured lessons/podcasts to prerequisites and source passages; distinguish interpretation from established fact; generate independent recall checks.
3. **Research methods:** start from assumptions/evidence, reason through a paper, generate counterexamples and test interpretation without copying unsupported claims.
4. **Exam preparation:** use user's selected topics, progressively vary problems, display error themes, offer optional review schedule; do not invent predicted grades.

## 7. Release milestones and verification

### L1 — Minimal UI vertical slice (P1)
- Spaces → Campus launches a learning session from a topic.
- Real local model asks 2–4 meaningful diagnostic questions, stores answers and reasoning.
- Present a compact dependency map (at least 3 concepts) and one lesson step with a dedicated question dock.
- Persist and reconnect; accept answer exactly once; pause and resume.
- Tests: typed event replay, question validation, network disconnect, model-offline, keyboard/mobile; one real browser+Android end-to-end run.

### L2 — Adaptive teaching and practice
- Error-driven plan repair, distinct practice session, examples/derivations and concept provenance.
- Real retrieval/transfer tests with independent rubric and transparent feedback.

### L3 — Persistent learning graph and resources
- Arena source/edge integration, PDF/Studio references, bounded contextual retrieval, Canvas visualization, local voice optional.

### L4 — Personalization and scheduling
- Longitudinal learning paths, opt-in spaced practice and quiet hours, calibration with demonstrated progress; avoid inflated certainty.

### Acceptance principles
- Never claim learning happened merely because a lesson was displayed or an answer matched.
- Separate measured outcomes (answers, transfer, retention over time) from self-rated comprehension.
- Preserve local-first and Guardian controls; AGPL upstream not copied without license review.
- UI must focus learner effort on the material, not navigation or agent internals.
- Provide tested Rust/Dioxus delivery, real event/answer verification and a reviewable PR.

## 8. Provenance and licensing

Upstream:
- [README](https://github.com/amosblomqvist/learnharness/blob/main/README.md)
- [Current architecture](https://github.com/amosblomqvist/learnharness/blob/main/docs/architecture.md)
- [Teaching methodology](https://github.com/amosblomqvist/learnharness/blob/main/docs/teaching-methodology.md)
- [Historical probe/teach design](https://github.com/amosblomqvist/learnharness/blob/main/docs/probe-teach-split.md) — historical, not shipped contract.
- [License](https://github.com/amosblomqvist/learnharness/blob/main/LICENSE)

The UI strategy above is a SAIL proposal inspired by these public concepts; it is not a claim of faithful binary or source-level LearnHarness integration. Audit upstream assets and AGPL obligations before reuse.
