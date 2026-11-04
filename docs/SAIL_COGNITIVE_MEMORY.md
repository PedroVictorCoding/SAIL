# SAIL Cognitive Memory — architecture audit and implementation plan

**Status:** Phases 0–5 are implemented. Phase 6 is partly implemented: Guardian memory grants
and the Dialogue integration are in place; Forge and Agents integration and model-proposed
change sets are next. Related issues: [#5](https://github.com/PedroVictorCoding/SAIL/issues/5)
Arena, [#21](https://github.com/PedroVictorCoding/SAIL/issues/21) Persona and retrospective
memory, [#52](https://github.com/PedroVictorCoding/SAIL/issues/52) conversation index,
[#100](https://github.com/PedroVictorCoding/SAIL/issues/100) Evidence Ledger,
[#68](https://github.com/PedroVictorCoding/SAIL/issues/68) contradiction audit.

The main consumer of this store is [Arena Learn](SAIL_ARENA_LEARN.md), Arena's learning product (#215).

Long-term memory in SAIL is structured knowledge: entities, claims backed by evidence,
procedures and project state. Conversations are temporary evidence. Once their knowledge
has been consolidated, they can be released, and a content hash and a short receipt
remain to record why the knowledge is believed.

## 1. Audit: what exists today (verified against code, November 2026)

| Area | State in code | Relevance |
|---|---|---|
| **Arena** (`crates/sail-arena`) | Working. A SQLite WAL store (`store.rs`) holds LZ4-compressed bodies and a contentless FTS5 index scoped by session. It archives every voice turn and every large tool result whole (`kind` = `turn`/`tool_result`). `compaction.rs` builds a bounded prompt context and the `memory_recall` tool reads the archive. | The only persistence layer suitable for memory. The knowledge schema is added here as migration v2, so there is no second database. |
| **Core** (`crates/sail-core`) | A binary crate (`main.rs`, `saild.rs`) that wires subsystems together. It has no library target. | It cannot host shared storage code without being restructured (see §3). |
| **Mind** (`crates/sail-mind`) | Manages a local `llama-server` and model selection. **There are no embeddings anywhere in the repository.** | Phase 3 vector retrieval needs an embedder provided through Mind. |
| **Agents** (`crates/sail-agents`) | Voice tool authorization. It publishes `MemoryRecordStored`, which Arena archives. | No durable job queue exists. |
| **Forge** | JSON task checkpoints with idempotency keys (`persistence.rs`) and a nightly runner. | The pattern can be reused for consolidation jobs; the code is Forge-specific. |
| **Flow** | A scheduled, interval-based safety review with catch-up after downtime. | A candidate trigger for idle-time consolidation. |
| **Guardian** | Typed `Capability` grants, a hash-chained audit ledger and an approval broker. | It has **no notion of memory scope**. `knowledge::Access` is the grant that Guardian will need to issue. |
| **Discovery** | An 87-line stub. | — |
| **Life / Friends** | Documentation only; no crate. | Preferences and person entities are stored in Arena using the `life` and `user` scopes. |
| **UI** | Plain HTML/JS served by `sail-dialogue/web/`. **The repository contains no Dioxus code**, although the docs and issues describe "Dioxus Web + Android". | Affects Phase 4 (see §3). |
| **Protocol** (`sail-protocol`) | Versioned inference contracts only. | Typed memory request/response contracts go here in Phase 3 and Phase 6 (required by #5). |

### Gaps against the specification

- Long-term memory **is** stored conversations today: the #164 archive keeps raw turns
  indefinitely. Its session ids are a fresh UUID per WebSocket connection, so archives from
  earlier connections are never recalled and are never cleaned up.
- There are no entities, claims, evidence model, epistemic status, temporal model,
  procedures or project state (Phase 1, delivered).
- There is no extraction or consolidation pipeline (Phase 2), no embeddings or vector
  index (Phase 3), no memory UI (Phase 4), no durable background jobs or retention
  enforcement (Phase 5), and no Guardian memory grants (Phase 6).

## 2. Module boundaries

```
sail-arena
├── store.rs        one SQLite file: connections, WAL tuning, versioned migrations, integrity check
├── compaction.rs   per-turn working context (hot window, breadcrumbs, tool offload)
└── knowledge/      long-term structured memory (this work)
    ├── model.rs    typed contracts: entities, claims, evidence, procedures, projects, Access
    ├── schema.rs   migration v2
    ├── write.rs    KnowledgeTx: every mutation, with invariants enforced
    └── read.rs     scoped reads: lookup, current state, history, evidence, support, graph
```

The conversation archive (`records`) and knowledge (`kn_*`) share one file and one writer
connection. This is what lets Phase 2 commit consolidated knowledge **and** release the
source transcript records in a single transaction. If that transaction fails, the source
evidence is still there.

## 3. Conflicts with the specification, and the proposed resolution

1. **"Core owns durable memory infrastructure."** `sail-core` is a binary, so other crates
   cannot depend on it. The storage engine stays in `sail-arena`, which already owns the
   only SQLite store. Core owns its *lifecycle*: `main.rs` opens it, and maintenance and
   integrity checks at startup will be scheduled from there. If a separate crate is
   wanted later, `store.rs` can be extracted to a `sail-storage` library without schema
   changes.
2. **Dioxus Web/Android UI.** No Dioxus code exists. **Decision (owner, 2026-11-02):** the
   Dioxus migration happens once SAIL is stable and hardened. Until then, memory views live in
   the existing web UI (`sail-dialogue/web`, following DESIGN.md).
3. **Relationships table.** A relationship is a claim whose object is an entity.
   `kn_relationships` is a SQL **view** over `kn_claims`, so relationships share evidence,
   epistemic status, temporal validity, scope and deletion with every other claim instead
   of being a second copy that could drift.
4. **The #164 transcript archive.** It is kept, as the *evidence buffer*. Phase 2
   consolidates from it; Phase 5 adds a retention policy so that records are released
   after consolidation or expire.

## 4. Phase 1: the knowledge model (implemented)

### Epistemics: separate axes, not one score

| Axis | Where it lives |
|---|---|
| Verification status | `kn_claims.status`: `verified`, `user_reported`, `inferred`, `unverified`, `disputed`, `superseded`, `invalidated` |
| Evidence reliability | `kn_evidence.source_kind`: `tool_observation`, `external_artifact`, `user_statement`, `model_inference`, `prior_memory` |
| Source independence | `kn_evidence.origin_hash`. Restatements (`prior_memory`) inherit the origin of the evidence they restate. `Support` counts *distinct origins*. |
| Inference confidence | `kn_claims.inference_confidence`, allowed only on `inferred` claims |
| Temporal validity | `observed_at` (when SAIL learned it) is kept separate from `valid_from` / `valid_until` (when it is true), plus `last_verified_at` and `superseded_by` |
| Preference basis | `explicit`, `observed`, `inferred` or `situational`. In current-state projections, explicit preferences rank above inferred ones. |

### Invariants enforced by the store

Each of these has a test in `crates/sail-arena/tests/knowledge_test.rs`.

- A claim has at least one piece of evidence, and its status must be supported by that
  evidence. `verified` requires a supporting tool observation or artifact; `disputed`
  requires refuting evidence.
- Project items cannot become `implemented`, `tested` or `deployed` without a tool
  observation or artifact as evidence. A model saying that it did the work is rejected.
- An older observation cannot supersede a newer one. `current()` orders by observation
  time, not insertion order, so readings that arrive out of order still project correctly.
- Asserting an equivalent active claim adds the new evidence to the existing claim instead
  of duplicating it.
- Entity creation never merges automatically. Name and alias lookup returns *every*
  match, so homonyms stay separate. Merging is an explicit, logged operation that keeps
  the merged entity as an alias stub.
- Procedures are versioned and start as `draft`. A `workflow_ref` must be a typed
  workflow id, never a shell command. Execution outcomes require direct evidence. The
  crate has **no execute function**: `required_capabilities` lists what Guardian must
  authorize.
- Deletion removes the row, its evidence links and any evidence receipts nothing else
  uses. It tombstones the claim's content fingerprint under every name its subject and
  object are known by, including aliases and pre-merge names, and increments a deletion
  generation:
  - a background job that started before the deletion fails with `StaleGeneration` when
    it calls `write_at_generation`;
  - a later replay of the same content fails with `Tombstoned`;
  - only an explicit user action (`Actor::User`) can teach the content again.

  Entity *names* are not tombstoned, so deleting one "Alex" does not block another.
- Reads take an `Access` value (allowed scopes and maximum sensitivity) and filter in SQL.
  A claim is visible only if the claim, its subject and its object are all visible, so
  graph traversal cannot cross a scope boundary.
- Every mutation writes a `kn_operations` row in the same transaction. The log stores ids,
  kinds and reasons; for a deletion it stores only a fingerprint, never the deleted
  content.
- Migrations are numbered steps, each running in one transaction. A database from a newer
  schema is refused rather than downgraded. `MemoryStore::check_integrity()` runs SQLite's
  integrity check and foreign-key check.

## 5. Remaining phases

| Phase | Deliverable | Depends on |
|---|---|---|
| 2 Consolidation | A deterministic extractor for explicit statements ("remember…", "I prefer…", decisions, corrections) and tool observations. Entity resolution: exact id → alias → typed attributes → (later) embeddings → LLM adjudication only when needed; uncertain matches get `possible_duplicate_of`. Reconciliation (duplicate, contradiction → `disputed`, supersession). One transaction commits the knowledge and releases the source `records`. Gate: a synthetic conversation becomes correct claims and the transcript rows are gone. | Phase 1 |
| 3 Retrieval | FTS5 over entity names and claims, embeddings through Mind (`llama-server --embedding`, version pinned), a brute-force vector baseline before any ANN index, graph expansion, permission-aware ranking, and a context composer with a token budget that is testable without a model. Typed contracts in `sail-protocol`. | Mind embedder |
| 4 UI | Overview, entity detail, graph, change history; correct, pin, merge, separate, mark private, forget, export, "why remembered". | §3.2 decision |
| 5 Background | A durable `kn_jobs` table (idempotency key, generation, attempts, backoff), admission control against active Qwen inference, idle triggers via Flow, retention and expiry for transient claims and archive records, startup integrity check. | Phase 2 |
| 6 Integration | Guardian issues `Access` grants per subsystem and task; Dialogue, Forge and Agents use the shared store; Discovery consumes approved insights. | Phases 2–3 |

Evaluation (spec §10) is added alongside each phase as deterministic datasets. No accuracy
or latency figures are claimed until they have been measured.

## Phase 2: deterministic consolidation

`KnowledgeStore::consolidate` consumes a bounded batch from one authorized scope.
Its whole-utterance grammar accepts `Remember: NAME is VALUE`, `Correction: NAME
is VALUE`, `Decision: NAME is VALUE`, `I prefer VALUE`, and `I now prefer VALUE`.
Only the user part of an archived turn is eligible. Unsupported or ambiguous
multiline text stays in the buffer; no general language extraction is claimed.
Trusted tool adapters can archive a typed `consolidation::Observation` as
`tool_observation`; ordinary tool-result text cannot promote claims to verified.

Resolution uses a supplied entity id first, then a unique same-kind name/alias in
the same scope. Multiple matches create a separate entity with
`possible_duplicate_of` links. There is no automatic embedding or model-based merge.
Claims, reconciliation, and source/index deletion commit together at the job's
captured deletion generation. Evidence keeps a bounded receipt and source hash.
An invalid candidate rolls back the entire batch, including all source deletions.
Tests in `consolidation_test.rs` cover restart, release, corrections, disputes,
permissions, injected role text, verified tool observations, and rollback/replay.

## Phase 3: retrieval baseline

Schema v3 indexes entity names, aliases and claim text with FTS5. Index changes
and vector invalidation follow knowledge mutations and deletion. Retrieval takes
an `Access`, filters visible/valid claims before ranking, combines keyword and
same-model/same-revision cosine scores, optionally expands one graph hop, and
returns typed v1 contracts from `sail-protocol::memory`. Disputed claims retain
their status; retrieval never upgrades epistemic certainty.

`retrieval::compose` accepts a token counter and emits complete JSON records only.
The default charges UTF-8 bytes conservatively for byte-level tokenizers; callers
can supply their actual tokenizer. The budget excludes surrounding instructions.
Retrieved values remain untrusted data, not executable instructions.

Mind provides an explicitly configured `embedding::EmbeddingClient` for a separate
worker using llama.cpp's [embedding API](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md).
Configure its immutable artifact revision, model id and dimensions. Run that
worker with `--embedding` and the embedding model's required pooling; do not
change the active chat worker. Model identity, dimensions, finite/nonzero values,
input/output sizes and timeouts are checked. Embedding inputs must still match
when their vectors are committed, and stale deletion generations are rejected.
No embedding model is downloaded, started, or benchmarked by this change.

## Phase 4: owner memory views

Arena's existing web page now includes entity overview/search, details, evidence
receipts ("Why remembered"), relationship graph, procedure/project records, change
history, correction, pin/unpin, privacy, merge/separate, forget and JSON export.
Controls reuse the dialogue `quiet-button` design. Retrieved text is inserted as
text nodes, never interpreted as HTML or instructions. No frontend migration is
required.

The `/api/memory` POST routes require the existing owner bearer authority, enforce
body limits, return `no-store`, and check deletion generation for mutations. Model
tool calls cannot choose `Actor::User`. Corrections add user evidence and supersede
the old claim; they never become tool-verified. Merge requires equal scope/privacy.
Separation restores the captured assignments/aliases only if no knowledge mutation
has happened since that merge; otherwise it refuses rather than overwriting later
edits. Deleting an entity removes its merge snapshots. Export contains knowledge
and receipts, not the transient transcript archive.

## Phase 5: background jobs and retention

Schema v5 adds durable, idempotent `kn_jobs`, exclusive five-minute leases, restart
recovery, a five-attempt limit and exponential storage-failure backoff. Sources are
identified by id, content hash and creation time, so row-id reuse cannot change a
queued job's inputs. Knowledge/source commits are atomic; completion bookkeeping
is replay-safe. Unsupported/failed sources remain until retention, but processed
source markers prevent them from starving newer batches. Stale-generation and
tombstoned jobs become obsolete and never recreate deleted knowledge.

Core checks database integrity before starting services. Flow schedules one bounded
memory tick per minute on a blocking worker, catches up after startup, and admits
work only while Mind has no active/queued inference and Forge is not planning,
coding, testing or reviewing. Admission is rechecked between stages; maintenance
never calls a model. `SAIL_MEMORY_BACKGROUND=0` disables this worker.

Raw evidence-buffer records expire after 30 days by default, configurable with
`SAIL_MEMORY_RETENTION_DAYS` (1–3650). Expiry deletes archive and FTS rows together.
Expired transient claims are removed using deletion/tombstone rules; pins protect
their subject's transient claims. Expiry, consolidation and queue batches are
bounded to 256 records per tick. The owner overview exposes job summaries without
source content. No scheduler latency or throughput benchmark is claimed.

## Phase 6: scoped integration (Dialogue slice)

`sail-protocol::memory` adds `MemoryPurpose`, `MemorySensitivity` and `MemoryGrant`.
`sail_guardian::memory::memory_grant` is the only policy that maps a purpose to visible
scopes; no prompt or model chooses them, and no grant reads `Sensitive` records.

| Purpose | Scopes | Max sensitivity |
|---|---|---|
| Dialogue | `voice`, `bus`, `user`, `life` | Private |
| Forge `{repo}` | `project:<owner/name>` (validated, lowercased) | Private |
| Maintenance | `voice`, `bus` | Private |

Arena enforces a grant with `Access::from_grant`; an empty scope list sees nothing.
`ArenaSubsystem::knowledge_context` runs the Phase 3 search under that access and returns
complete JSON records within a byte budget, or nothing. Retrieval errors are logged and
never block a turn.

On every turn, Dialogue appends the result to its system prompt as labelled untrusted data
(`SAIL_MEMORY_CONTEXT_BYTES`, default 1500, range 1–16000). Core's background
maintenance now takes its access from the Maintenance grant.

`crates/sail-dialogue/tests/memory_acceptance_test.rs` is the master vision §4.6 test,
run on an on-disk store through the production Dialogue helper:
- An explicit turn becomes one claim, and its raw turn is released.
- A later turn retrieves the fact, and unrelated turns are not memory.
- Forge sees only its repository's facts, and Dialogue does not see them.
- A correction supersedes the old value.
- Deletion holds after a restart, and re-running consolidation cannot resurrect it.

Not done yet:
- **Forge integration.** Forge does not consume its grant yet.
- **Agents integration.** Agents does not consume a grant yet.
- **Model-proposed change sets** (vision §4.3): implemented separately; see the next section.
- **Embeddings in live retrieval.** Embeddings are not wired into it.

## Model-proposed change sets (vision §4.3)

Phase 2's grammar only catches fixed phrasings, so ordinary conversation produced no
knowledge. Now a local model may **propose** a `MemoryChangeSet`
(`knowledge::changeset`). A deterministic validator decides whether any of it is committed.

**Operations.** There are two:
- `assert`: a fact, an Owner preference, or an event.
- `correct`: names an existing claim id from the known-claims list.

Each operation cites one offered source turn and quotes the user's exact words.

**Validation.** The validator checks schema and limits first:
- Version, at most 16 operations, size bounds, and snake_case predicates.
- No capability or question categories from a model, and preferences only for `Owner`.

Then it checks grounding:
- The quote must be a verbatim substring of that turn's **user** side. Assistant prose is
  never offered or accepted.
- The value must appear in the quote.
- The subject must appear in the quote, or be `Owner`.

**Commit.** One invalid operation refuses the whole set. Accepted sets commit in one
transaction at the deletion generation captured before the model call. A deletion during
the call returns `StaleGeneration`, and the turns are proposed again later.

Entity resolution, supersession and dispute rules are Phase 2's own
(`consolidation::apply_candidate`). Tombstones still prevent resurrecting deleted claims.

Model-extracted claims are `inferred`. The quote is the evidence receipt, and model
confidence is never recorded as evidence.

**Turn lifecycle.**
- A turn is offered only after the deterministic pass, with no consolidation job pending
  in its scope.
- A turn is offered at most once, marked in `kn_source_progress` as version 1001.
- Consumed turns are released. Unconsumed turns stay until retention expires them.
- If the model fails, turns stay queued. If the reply is unusable or rejected, turns are
  marked as offered.

**Scheduling.** Flow's maintenance makes at most one model call per tick, only while
idle. Core wires it to `MindService` with temperature 0, 768 output tokens and a 180 s
timeout.

**Opt-in.** It is off by default: set `SAIL_MEMORY_MODEL_PROPOSALS=1`.

**Not measured.** Extraction quality on the target host's local model has not been
measured; that is the condition for turning it on by default.

Tests:
- `crates/sail-arena/tests/changeset_test.rs` covers:
  - A grounded commit.
  - Rejecting hallucinated values, assistant-sourced quotes and partially invalid sets.
  - Corrections.
  - No resurrection of deleted knowledge.
  - Stale generation.
  - Scope permission.
- `crates/sail-flow/tests/memory_maintenance_test.rs` covers:
  - No model call while busy.
  - Retry after a model failure.
  - Commit, and no re-proposal of unusable replies.

## Conversation learning consent (November 2026)

The next accompaniment slice is an explicit, connection-scoped learning choice in the
existing Dialogue interface. It reuses Arena extraction/reconciliation, Guardian owner
bearer authority, Flow maintenance, and the existing evidence/correct/forget views.
It adds no model, process, database, navigation tab, or external dependency.

Acceptance contract:

- Connect: learning is off. Voice/text interaction still has the bounded in-RAM hot
  window; turns and large tool bodies are not automatically archived.
- In Settings → Voice, the authenticated owner enables **Learn from this conversation**.
  The backend registers consent only for a live session. A header indicator reflects
  the acknowledged state. Permission to use the microphone is independent; this grant
  does not authorize proactive actions or recording other people.
- Subsequent turns enter the existing local evidence buffer and maintenance pipeline.
  Deterministic extraction works without a model. Natural-language proposals remain
  opt-in through `SAIL_MEMORY_MODEL_PROPOSALS=1`; unavailable inference keeps sources
  pending under the existing retention policy. No extraction-quality claim is made.
- Consent has an epoch. Text dispatch and voice capture preserve their starting epoch;
  archival of turns and tool bodies rechecks it under the session lock. PCM processing
  discards capture/pre-roll when the epoch changes. An earlier turn cannot become
  persistent after consent is enabled or re-enabled.
- Disable: revoke consent before cleanup, atomically delete that session's pending
  archive/FTS rows and advance the deletion generation. Previously leased proposals/jobs
  cannot commit those sources. Already consolidated knowledge remains available for
  explicit correction or forgetting in Arena. SQL deletion is not forensic erasure of
  filesystem/WAL/backup bytes.
- Reset/reconnect/restart: require new consent. Disconnect releases live context; sources
  recorded while consent was valid may finish consolidation after disconnect. Consent
  is never saved in localStorage or restored from disk. No retroactive migration of
  historical archives is performed.
- Failure: no owner authority denies changes; disconnected sessions refuse consent;
  storage cleanup failure leaves learning revoked and reports an error. The browser
  shows an unknown state on failed requests and its next action disables learning.
- Logging: Dialogue command bodies, prompts, recalled records, tool arguments/results,
  Arena bus summaries, and Mind speech-worker request/response bodies are removed from
  these diagnostic paths. Existing logs are not rewritten.

Verification: `cargo test -p sail-arena -p sail-dialogue -p sail-flow`; browser-control
state tests: `node --test crates/sail-dialogue/tests/voice_learning_ui_test.cjs`.
The consent tests exercise real SQLite source/FTS cleanup, leased-job invalidation,
revocation/re-enable, and on-disk restart. The owner endpoint test uses a real HTTP
listener. The existing Dialogue memory acceptance sequence explicitly opts in and
continues to verify consolidation, corrections, scoped recall and deletion after restart.
These tests do not substitute for live microphone/model quality or Web/Android testing.

### Remaining migration toward ambient accompaniment

The existing SQLite evidence buffer is compressed, **not encrypted by SAIL**. The UI
states this before opt-in. Raw sources not released by consolidation are eligible for
cleanup after `SAIL_MEMORY_RETENTION_DAYS` (30 days by default); maintenance currently
runs only when foreground inference/coding permits. Application-level encrypted,
short-lived evidence and cleanup independent of inference admission remain prerequisites
for the broader always-on ambient collection described by master vision §4.2. Learning
off is the default; this change does not enable ambient/background microphone capture,
speaker attribution, Sense ingestion, Life planning, Discovery suggestions, or model
weight training. It establishes the consent boundary for the existing interactive path.
