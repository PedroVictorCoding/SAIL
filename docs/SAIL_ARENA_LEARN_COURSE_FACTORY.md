# Arena Learn Course Factory — ADR and source policy

**Status:** accepted (M0), 2026-11-07. **Issues:** #386 (courses), #215 (Arena Learn), #249,
#250, #313, #333, #379. **Contracts:** `crates/sail-arena-learn/src/factory/contracts.rs`.

The Course Factory turns books, papers and notes that the owner provides into source-grounded,
original interactive courses. It works ahead of demand, in small background jobs that can be
resumed. It is a feature of Arena Learn, not a new application: published output is an ordinary
`CoursePackage`, served and graded by the existing `CourseEngine` without any model call.

## 1. What already exists (audit, 2026-11-07)

| Need | Existing piece | Decision |
|---|---|---|
| Course content, graders, evidence, review gate | `sail-arena-learn::courses` (#386): `CoursePackage`, deterministic `grade`, `logic`, append-only `CourseStore`, `EvidenceState`, `/reviews/due` | **Reuse.** The factory's output type is `CoursePackage`, and evidence stays in `courses::evidence`. |
| Package source | `content::catalog()`: packages built into the binary | **Extend.** `CourseEngine` also serves published `PackageRevision`s from the factory registry. |
| Durable background work | `sail-continuity`: tasks, leases, checkpoints (64 KiB), dedupe keys, boot recovery, effect journal | **Reuse.** Each job is a Continuity task (owner `sail-arena-learn`, kind `learn.factory.<kind>`). No new scheduler. |
| Idle / admission | `sail_common::resources::ResourceScheduler` (RAM/PSI/swap/CPU, `max_heavy_models = 1`, voice preemption), Flow's idle tick (`MemoryMaintenance`), and Core's `idle` predicate (Mind has no work and Forge is idle) | **Reuse.** The factory runs on Flow's tick behind the same idle predicate and `try_admit(ResourceClaim::inference(Background))`. |
| Inference | `sail-mind` `infer_with_priority(.., WorkPriority::Background)`, cancellable, preempted by voice | **Reuse** through the `AuthorModel` port. |
| Sandboxed Rust | `sail-runner` + `sail-learn-rust-check` via `RunnerRustEvaluator` | **Reuse** to compile reference solutions and run hidden tests. |
| Documents | Reader: EPUB + TXT parser (`sail-bridge::reader`). Learn sources: `.md`/`.txt` notes ≤ 5 MB. No PDF parser. | **Add** PDF extraction and the factory source registry, and reuse the EPUB parsing logic. |
| Knowledge graph | `sail-arena` propositions, Learn `GraphSnapshot` | **Keep separate.** `ConceptGraph` is course structure. It is not memory and holds no learner competence. |

## 2. Decisions

1. **Location.** The code lives in `crates/sail-arena-learn/src/factory/`. HTTP goes in
   `crates/sail-dialogue/src/learn_factory_http.rs`, nested under `/api/learn`. The UI lives in
   `web/courses.js` (a generation inspector sheet; there is no new tab).
2. **Storage.** The factory uses the same Arena Learn SQLite file, in its own tables
   (`learn_factory_<area>_*`). Imported source bytes are stored by content hash under the Arena
   Learn state directory. There is no second learner database.
3. **Routes.** `/api/learn/sources` already lists tutor notes, so the factory uses
   `/api/learn/factory/*` (`sources`, `sources/:id`, `plan`, `jobs`, `jobs/:id`, `pause`,
   `resume`, `prioritize`, `settings`) and `/api/learn/courses/:id/{generation,drafts,publish,
   regenerate}`. Mutations need the owner token (`ApprovalAuthority`) and are idempotent by
   `request_id`.
4. **Jobs.** Jobs are small and each one checkpoints (see `AuthoringJob`). Only **one
   model-using job runs at a time**. A dispatcher, ticked by Flow, does the following:
   - Selects the lowest `(tier, tie-break)` from a queue rebuilt deterministically from
     persisted plans, drafts, evidence and due reviews (`PriorityReason`).
   - Dispatches only while the owner has enabled generation and the idle predicate holds, and
     only after `ResourceScheduler::try_admit` succeeds and the daily budget has room.
   - Keeps every stage result durable before the next stage starts. A cancelled or preempted
     model call loses only that call: the job resumes from its last completed stage, and
     partial tokens are never kept.
   - Maps `AuthorModelError::Unavailable` to *waiting for model* (backoff, no retry loop). It
     maps `Preempted` and `NotAdmitted` to *requeued*, and `Failed` to a retry with backoff, bounded at 5 (`jobs::MAX_FAILURES`).
5. **Model output is data.** Replies are parsed into `deny_unknown_fields` structs. Ids, revisions,
   anchors, receipts and publish decisions are assigned by the server. A fragment id that the
   prompt didn't include is a grounding failure.
6. **Validation is independent of generation.** `DraftValidator`s produce
   `ValidationReceipt`s, which are bound to the draft's `output_hash`. The checks are:
   - Schema
   - Grounding
   - Copyright
   - Subject (exact key / logic / math / physics / rubric)
   - ExerciseTests (sandbox: the reference compiles, the hidden tests pass, and `known_bad`
     fails)
   - Quality

   A reviewer-model pass is optional extra evidence and never replaces a deterministic check.
   `Unavailable` is never a pass.
7. **Publishing.** A revision publishes only when every required receipt is `Pass` for the
   current hash. `Assessment` stakes also forbid `NeedsReview`. Published `PackageRevision`s are
   immutable and are deduplicated by `package_hash`. The learner sees a unit only once all of
   its lessons are published (Unit 3 "Generating" shows as planned, never as partial).
8. **Source changes.** A changed source produces an `Invalidation`:
   - Drafts built from the old revision become `SourceInvalidated`.
   - Published revisions are re-validated. They stay served unless an anchor no longer
     resolves.
   - Attempt history and evidence are never deleted.
9. **Learner awareness.** Evidence influences the *priority* of generation and nothing else.
   The spec's states map onto `EvidenceState`: UNTESTED→`untested`, EXPOSED→`exposure`,
   ASSISTED→`assisted`, PROVISIONALLY_INDEPENDENT→`provisional`, RETENTION_DUE→`due`,
   RETAINED→`retained`, WEAK→`weak`. `uncertain` (conflicting evidence) and `stale` (retained
   but long unreviewed, or its source changed) are added in M4. They are added to the same
   enum, not built as a second mastery system.
10. **Rust exercises.** A new answer key, `AnswerKey::RustTests { tests }`, grades learner code
    against hidden tests in the sandbox. The reference solution and the tests never leave the
    server (`StepView` stays the only client shape).

## 3. Source rights policy

The owner sets the rights of each source at import (`SourceRights`). They are never inferred from
the file's text. The factory treats them as follows:

| Rights | Processing | Verbatim quoting in lessons | Figures / question banks / worked solutions | Export of derived packages |
|---|---|---|---|---|
| `owner_private` | yes | ≤ 200 words per run | allowed | allowed |
| `open { license }` | yes | ≤ 200 words, attribution kept | allowed under the licence | allowed, licence inherited |
| `restricted` | yes, for grounding | ≤ 25 words per run | **never reproduced**; write original ones | **no** |
| `unknown` | as `restricted` | as `restricted` | as `restricted` | **no** |

- The Copyright check measures the longest shared word run between published text and each cited
  source, and fails above the limit.
- A package inherits the most restrictive rights of its sources.
- DRM-protected files are refused, never decrypted.
- Source text is untrusted data. An instruction inside a book ("ignore your rules", "publish
  this") is just text: it can't change policy, job state or publication, and validators never
  read it as instructions.

Current owner sources (`course-content/`): the online text of *The Rust Programming Language* is
MIT OR Apache-2.0 (rust-lang/book), but the committed PDF looks like the 2022 No Starch print
edition. It is therefore `restricted` until the owner confirms otherwise. Using the rust-lang/book
Markdown would make it `open`. Every other book is `restricted` unless the owner says otherwise. Chinese language
learning has no `Domain` yet and is out of scope for this round.

## 4. Milestones

M0 is this ADR plus the contracts. M1 builds sources → fragments → concepts → syllabus, with owner
review. M2 adds lesson authoring, Rust fixtures and validation, and publishes one unit. M3 brings
idle orchestration (Continuity, admission, preemption, restart). M4 adds evidence-driven
priorities and unseen review variants. M5 covers logic, physics, maths, simulations and
assessments. M6 is the UI (web and Android), benchmarks and adversarial tests. The first
acceptance scenario is §18 of the brief: the Rust Book → an ownership lesson with two
compiler-backed problems and a review variant, published, played, and reviewed.

## 5. Status and use (2026-11-08)

Built: M0–M3, the M2 validators, the M6 web UI, and the evidence-free part of M4 (lessons
are prioritised by plan order, owner requests and pipeline stage, not yet by learner evidence).

**Using it.** Open **Arena → Course Factory**:

1. **Import** a book or notes (EPUB, Markdown, text, PDF). Rights default to `restricted`.
   EPUB, Markdown, text and older PDFs are indexed at once. A modern (compressed) PDF is
   read 20 pages per job by `pdftotext` inside the `sail-runner` sandbox. Without the runner
   it waits as *needs review* (see `docs/SANDBOX_RUNTIME.md` for the poppler setup).
2. **Create a course** over one or more indexed sources. The factory maps concepts in
   bounded batches, then proposes a plan, in the background, while the machine is idle.
3. **Review the plan.** Edit the concept map if you like (rename, merge, remove, reorder,
   confirm or reject prerequisites), then **Approve**. Lessons start generating, the first
   three first.
4. Each lesson runs through the six checks (schema, grounding, copyright, subject, Rust
   tests, quality). A lesson that passes them all is published to **Arena Courses**
   automatically. One the checks can't fully verify waits in the inspector for you to
   publish or reject. One that fails is rewritten, up to three drafts.

Settings: on/off, idle-only, a daily model-time limit (default 120 min) and a global pause.
Generation runs at background model priority, so a voice turn preempts it.

**Not verified yet:** a real `sail-runner` round trip for PDFs and generated Rust tests (no
runner is configured on the build host), and lesson quality with the local model. The
acceptance test (`tests/factory_pipeline_test.rs`) uses a scripted model.
