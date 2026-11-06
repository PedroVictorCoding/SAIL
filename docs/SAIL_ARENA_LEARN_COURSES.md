# Arena Learn · Interactive Courses

Issue #386. Interactive Courses are short, original lessons inside Arena Learn. They sit
beside the conversational tutor (#215) and practice sessions (#244). They reuse Arena Learn's
SQLite file and owner auth, but they keep their own append-only evidence table and **never
call a model**.

## What ships in this slice (P0)

- **Catalog:** Rust & Systems Programming (ready), Philosophy & Logic (planned), and Physics,
  Math & Engineering (planned). Planned tracks show no lessons rather than placeholders.
- **Rust ownership chapter:** a cold placement check (4 items), two lessons of about 5 and
  6 minutes, and 8 unseen review variants. The content is original SAIL writing in
  `crates/sail-arena-learn/courses/rust-systems.json`.
- **Step kinds:** `explain`, `choice`, `predict`, `order`, `fill` and `rust_edit`. Each is
  graded deterministically from the step's key, or from sandbox output for Rust.
- **Hints and reveal:** up to 4 progressive hints, plus an explicit solution reveal. Both are
  recorded, and either one makes that answer *assisted*.
- **Evidence and review:** per-objective evidence states, a 1/3/7/14/30-day review schedule,
  a bounded due queue (5 items) and a server-side certified-advance gate.
- **API:** `/api/learn/{courses,next,lessons,diagnostics,reviews,skills}`. See
  `crates/sail-dialogue/src/learn_courses_http.rs`.

## Code layout

| Path | Role |
|---|---|
| `sail-arena-learn/src/courses/content.rs` | Package schema, validation and learner-safe `StepView` |
| `sail-arena-learn/src/courses/grade.rs` | Deterministic graders, rustc JSON parsing |
| `sail-arena-learn/src/courses/evidence.rs` | Event types, projection, scheduler, due queue, gate |
| `sail-arena-learn/src/courses/store.rs` | `learn_course_events` table (append-only, idempotent by `attempt_id`) |
| `sail-arena-learn/src/courses/engine.rs` | Orchestration, with the clock and the Rust evaluator injected |
| `sail-dialogue/src/learn_courses_http.rs` | Owner routes and the `sail-runner` adapter |

## Evidence rules (policy `courses-evidence-1`)

These are planning heuristics, not claims about memory science or about the learner.

| State | Meaning |
|---|---|
| `untested` | No checked answer. SAIL doesn't assume either way. |
| `exposure` | Seen in a lesson, but not yet checked. |
| `assisted` | Passed only after a hint or a revealed solution. A fresh problem is due the next day. |
| `provisional` | Passed independently, but not yet after a delay. |
| `due` | The next delayed check is due now. |
| `retained` | Passed independently again at least 20 hours later, on a different problem family. |
| `weak` | The most recent answer was wrong. A new problem is scheduled for the next day. |

- `unavailable` results (for example, no sandbox) are never counted as right or wrong.
- A lesson is **completed** when every step is read, passed, or not assessed. It is
  **certified** only when every lesson objective has an independent pass and no review gate
  is open. Certification still means *provisional*. Only delayed reviews make an objective
  *retained*.
- **Gate:** while reviews are due, `POST /lessons/:id/advance` returns `blocked_by_review`.
  The gate opens again once the learner has done `min(due, 5)` review attempts within
  20 hours, so a backlog can't lock the course for good. Reading lessons is never blocked.
- Course evidence never writes to the knowledge graph. Competence isn't a claim about the
  world.

## Rust sandbox

Learner code runs **only** through `sail-runner`, the dedicated-UID service using
bubblewrap. Configure it with `SAIL_RUNNER_SOCKET` and `SAIL_RUNNER_UID`, and install
`crates/sail-arena-learn/sandbox/sail-learn-rust-check` plus a pinned `rustc` inside the
runner image (at `SAIL_LEARN_RUST_CHECKER`, default `/usr/local/bin/sail-learn-rust-check`).
Without them, Rust steps return `unavailable` and the lesson can still continue. There is no
fallback to the host toolchain.

## Not in this slice

Philosophy and Physics lessons (the logic checker is in progress), the tutor handoff, Pulse
reminders, confidence calibration reports, the Strategy Lab, the CodeMirror editor and
offline sync.
