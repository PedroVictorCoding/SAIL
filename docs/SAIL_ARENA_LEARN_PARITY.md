# Arena Learn — LearnHarness behavioral parity matrix

**Status:** implementation-tracking document for [#215](https://github.com/PedroVictorCoding/SAIL/issues/215) and [#293](https://github.com/PedroVictorCoding/SAIL/issues/293). Upstream reviewed 2026-11-02/04; re-review on upstream architecture changes. The SAIL implementation is independently authored (clean-room); no AGPL source, prompt text, UI assets or fixtures are copied.

Legend: **Tested** = a Rust or browser test exercises the behavior; **Deviation** = an explicit, scoped difference; **Gap** = not yet claimed.

| Upstream behavior | SAIL implementation | Status / test | Deviation or gap |
|---|---|---|---|
| Probe prerequisites, free reasoning and confidence | `probe`, `ask_user_question`, `probe_done`; `Answer.thinking`/`confidence` | Tested (`engine_test`, `judge_test`, acceptance) | — |
| Gated prober → teacher handoff only after successful `probe_done` | `Effect::Handoff`; crash resume test | Tested (`engine_test::crash_after_handoff_resumes_with_the_teacher_once`) | — |
| Validated dependency DAG plan; visited ≠ mastered | `LessonPlan::validate`, node-tagged `explain`/`quiz`, `Progress` with no score | Tested (`plan` unit tests, projection) | — |
| Predict → explain → check → repair with exactly-once answers | `explain`, `quiz`, hidden rubric judge, idempotent `answer` | Tested (`engine_test`, `judge_test`, acceptance) | Repair quality with a real local model is not claimed |
| Separate practice sessions; one active problem; hidden solution; learner hints; honest outcome | `Mode::Practice`, `problem`/`conclude`, hint and attempt endpoints | Tested (`practice_test`) | — |
| Practice outcome is not graph mastery | Practice events never touch plan or graph | Tested (`practice_test`) | — |
| Objective propositions + jointly entailing reasoning chains with provenance | `sail-arena` `learning:*` propositions, chains, provenance, tombstones | Tested (`learning_test`) | Extraction quality depends on the local model |
| Gated graph-read tools, no automatic injection | `search_knowledge` for prober/teacher/practitioner, bounded to 1500 bytes | Tested (`knowledge_test`) | Deliberate decision (#293) |
| Post-hoc extraction with grounded validation and deterministic merge | `agent::extraction`, verbatim quote check, retry, coverage ledger | Tested (`knowledge_test`) | Extraction runs as a background task after a turn, not through Flow's idle admission queue |
| Import Markdown/text with provenance; source deletion keeps shared claims | `/api/learn/sources`, content hash, provenance rows | Tested (`learning_test`) | PDF is out of scope (#48) |
| Session durability: append-only events, replay, idempotency, reconnect, compaction | SQLite event store, `Envelope` keys, SSE reconnect, `ContextCompacted` | Tested (`store_test`, `projection_test`, acceptance) | — |
| Dedicated answer dock, plan map/list, practice, graph explorer | `crates/sail-dialogue/web/learn.js` | Browser helpers tested (`learn_ui_test.cjs`); manual browser check remains | Graph explorer UI is a compact contract panel, not a full graph workbench |
| Owner-inspectable/overridable SAIL-authored role prompts | Prompt override directory and `GET /prompts` | Tested (`prompts` unit tests) | — |
| Visual demos / research jobs with honest availability | `show_diagram` with strict Mermaid; research jobs not implemented | Diagram tested | Research/PDF/visual jobs remain gaps; the UI does not claim they exist |
| Local-model robustness and real multi-session acceptance | Typed tools, argument healing, judge uncertainty, role slots | Scripted tests pass | **Gap:** one recorded real local multi-topic journey with observed misconceptions, plan repair and graph reuse |
