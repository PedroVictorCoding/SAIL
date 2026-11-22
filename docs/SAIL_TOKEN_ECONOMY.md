# SAIL token economy: local does the heavy lifting, Claude and Gemini do the hard part

**Status:** Proposed — builds on the implemented escalation core · **Owners:** Mind (routing,
briefs, ledger), Forge (review provider), Pulse (windows, scheduling), Flow/Skillsmith (skill
capture), Guardian (egress, unchanged) · **Extends:** [External models](SAIL_EXTERNAL_MODELS_GATEWAY_SKILLS.md)
(decisions 12 and 15), [Proactive escalation](SAIL_PROACTIVE.md), [Night Shift](SAIL_NIGHT_SHIFT.md),
[Forge](SAIL_FORGE.md) · **Related repo:** `PedroVictorCoding/sail-framework` (local fine-tuning)

## 0. Problem

The owner is cutting down to two subscriptions: **Claude** and **Gemini**. ChatGPT (Codex) and
ByteDance go away. Subscription capacity is scarce and shared with the owner's own interactive
use, so SAIL must:

1. keep the local model as the workhorse for everything it can verifiably do;
2. send Claude and Gemini only the *hard kernel* of a task, as a small, dense brief;
3. never spend the capacity the owner needs for his own chats;
4. learn from every escalation so the same kind of task escalates less next month.

What already exists (decision 12 and `crates/sail-mind/src/escalation/`): Claude Code, Codex and
Gemini CLI providers as text-only models, an OpenAI-compatible key adapter, the quota ledger,
escalation only after local checks fail, redaction and egress receipts. This spec adds what is
missing on top of that, without a new service or tab.

## 1. Proposed owner decisions

| # | Decision |
| --- | --- |
| 16a | **Two providers.** Claude Code and Gemini CLI are the default external providers. Codex stays in code but is disabled by default; the ByteDance plan gets no adapter. |
| 16b | **Owner first.** SAIL may use at most a set share of each subscription window (default 30 %). It stops early, not at the vendor's hard limit, so the owner never finds his own Claude or Gemini blocked. |
| 16c | **Escalate the step, not the task.** Mind sends the stuck step with a brief of at most ~8k tokens by default. Whole repositories or transcripts go out only when the task kind allows it (Gemini long-context jobs). |
| 16d | **Every escalation must pay back.** Its outcome becomes a cached answer, a staged skill or a distillation example, so local can handle that pattern next time. |

## 2. Division of labour

| Work | Who | Why |
| --- | --- | --- |
| Search, fetch, parse, dedupe, format, lint, test runs, diffs, data cleaning, file moves | Deterministic Rust tools | No tokens at all |
| Summaries, classification, extraction, first drafts, retrieval, routine code, tool plans, voice | Local resident model | Free, private, always available |
| Retries with more thinking, best-of-N plus verifier, overnight long runs | Local, at `Background` priority (Night Shift) | Time is cheaper than tokens |
| Very long context: a whole repo, long PDFs, papers, video/YouTube transcripts; web-grounded research; broad first passes | **Gemini** | Long context window, search grounding |
| Hard coding and debugging, code review, tricky reasoning, final writing in the owner's voice | **Claude** | Strongest at these; worth the scarce capacity |
| Anything still failing after both | The owner | A clear question beats more spending |

The defaults are a starting point. The ledger measures which provider actually passes each task
kind's checks, and routing follows the measurements, not this table.

## 3. Escalation ladder

```text
 task ─► deterministic tools ─► local (N attempts, checks) ─► local+ (more thinking / best-of-N / overnight)
                                                                    │ still failing
                                                                    ▼
                                      Brief builder (local): hard kernel only, ≤ budget, redacted
                                                                    │
                                     ┌── cache hit? ─► reuse answer, re-verify locally
                                     ▼
                     Router: task kind → provider order (measured) → quota & owner share check
                                     │
                     ┌───────────────┴───────────────┐
                     ▼                               ▼
                  Gemini                          Claude
                     └───────────────┬───────────────┘
                                     ▼
                Local finisher: apply, run checks, format, summarize, record outcome
                                     │
                ┌────────────────────┼─────────────────────┐
                ▼                    ▼                     ▼
          answer cache        staged skill (Flow)    distillation example
                                     │
                       pass ─► done   fail ─► next provider or ask owner (no loops)
```

### 3.1 Brief builder (local)

Before any external call the local model produces a **brief**, and Rust validates it:

- the goal in two sentences and the task's measurable checks;
- what was tried locally and how it failed (the failing test output, trimmed);
- only the relevant context: the 1 to 6 files or sections retrieval selected, cut to the
  functions involved, or the paper excerpts with citations;
- the expected output shape (a unified diff, a JSON schema, a verdict line).

Limits: default 8k tokens in and an output cap per task kind. If the brief cannot fit, the step is
split or it goes to Gemini under a long-context task kind. The redaction and egress rules of
decision 15 apply unchanged, and the receipt records brief size.

### 3.2 Local finisher

External output is untrusted text, as today. The local side applies it, runs the same checks,
formats it, and writes the summary and PR text. External models are never asked to do the
boilerplate around their answer.

## 4. Capacity: windows, shares and scheduling (Mind + Pulse)

- **Shared capacity.** Treat each subscription as shared between the owner and SAIL. Vendor
  limits change and are not exposed uniformly, so read each vendor's current plan limits when
  configuring, and rely on what the CLI reports (usage fields, limit errors, reset times).
- **Owner share.** Per provider, an owner-set share of the window (default 30 %) and a daily
  cap of calls. The ledger estimates use from reported tokens; when it reaches the share, the
  provider is "reserved for owner" until reset. The owner can say "use it all tonight".
- **Timing.** External background work is scheduled preferentially when the owner is asleep or
  away (Presence) and early in a fresh window, so that his daytime capacity is intact.
- **Queue, don't fail.** Out of share means the job waits for the next window with a visible
  "waiting for capacity" state, never a fake answer and never a retry loop.
- **Optional overflow.** A pay-per-token API key (for example a small, fast Gemini model) with a
  hard monthly spending cap set by the owner. Off by default.

## 5. Migration off ChatGPT (Forge review)

Forge's per-issue external review is Codex-only today (`codex login status`, one slot per issue,
#96). Before the ChatGPT plan is cancelled:

1. Add a review provider choice: `claude_cli` (default), `gemini_cli` (fallback), `codex_cli`
   (legacy, off).
2. Same contract: exactly one `VERDICT:` and one `REQUIREMENTS_COVERED:` line, one slot per
   canonical issue, the slot is consumed only once the provider actually ran, read-only isolation.
3. Pre-flight per provider: binary present, signed in through its own CLI, version in
   `verified_versions`, tools disabled by the documented flags.
4. Existing ledgers stay compatible; the provider is recorded with each review.

## 6. Making escalations pay back (Arena, Flow, sail-framework)

1. **Answer cache.** Keyed by a hash of the normalized brief. A hit is re-verified locally
   before use. Entries expire when the files they touched change.
2. **Skill capture.** After a successful escalation, the existing self-improvement review
   proposes a skill or patch describing how the problem was solved. It stays staged until the
   owner approves, as today.
3. **Distillation set.** Successful (brief, answer, check result) triples are kept locally,
   already redacted. When enough accumulate for a task kind, `sail-framework` trains a LoRA
   adapter for the resident model, evaluated with [Model eval](MODEL_EVAL.md) before it is
   enabled. Check each vendor's terms on using outputs for training before enabling this
   for that vendor; until checked, it is off.
4. **The metric.** Monthly escalation rate per task kind, and tokens per solved task. Both
   should fall. Shown in Settings → External models.

## 7. UI

No new tab. Settings → External models gains: provider order per task kind, owner share and
daily cap per provider, "waiting for capacity" jobs, and the monthly metrics. Answers keep
showing the answering model, and the cost line reads "local" or "Claude · 3.1k tokens".

## 8. Phases

| Phase | Scope | Accepted when |
| --- | --- | --- |
| T1 | Forge review on Claude/Gemini; Codex off by default | A Forge issue gets one review from Claude Code with the same verdict contract; with Claude out of share it falls back to Gemini; the PR names the reviewer. |
| T2 | Owner share, daily caps, "waiting for capacity" queue, Presence-aware timing | At 30 % estimated use SAIL stops using a provider and queues work; it resumes after reset; nothing is retried in a loop. |
| T3 | Brief builder and local finisher | Every external request carries a validated brief within budget; receipts show brief size; median external tokens per escalation drop against the T2 baseline. |
| T4 | Measured routing per task kind | Provider order updates from ledger pass rates, with a minimum sample size and a manual override. |
| T5 | Answer cache and skill capture after escalation | A repeated escalation hits the cache and is re-verified; a solved escalation produces a staged skill. |
| T6 | Distillation with sail-framework | A LoRA trained on one task kind passes Model eval and lowers that kind's escalation rate. |

## 9. Risks

- Vendor CLI flags, output formats and plan limits change; pin versions and re-verify (as
  decision 12 already requires).
- Usage estimates can be wrong; the share has margin and vendor limit errors always win.
- A brief that drops too much context makes the external model fail; T3 is measured on pass
  rate as well as tokens.
- Distillation can teach the local model wrong patterns; nothing trained is enabled without
  passing Model eval.
