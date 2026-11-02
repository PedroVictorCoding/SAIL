# SAIL Cognitive Memory — architecture audit and implementation plan

**Status:** Phase 0 (audit) and Phase 1 (storage and contracts) are implemented. Phases 2–6
are planned. Related issues: [#5](https://github.com/PedroVictorCoding/SAIL/issues/5)
Arena, [#21](https://github.com/PedroVictorCoding/SAIL/issues/21) Persona and retrospective
memory, [#52](https://github.com/PedroVictorCoding/SAIL/issues/52) conversation index,
[#100](https://github.com/PedroVictorCoding/SAIL/issues/100) Evidence Ledger,
[#68](https://github.com/PedroVictorCoding/SAIL/issues/68) contradiction audit.

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
2. **Dioxus Web/Android UI.** No Dioxus code exists. Phase 4 needs a decision: either build
   the memory views in the existing web UI (`sail-dialogue/web`, following DESIGN.md), or
   first migrate the frontend to Dioxus, which is a separate, much larger project.
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
