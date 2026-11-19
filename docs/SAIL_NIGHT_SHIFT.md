# SAIL Night Shift: ask by day, review in the morning

**Status:** first slice implemented (round 27) · **Owners:** Forge (errands, planning, the night,
the morning report, shipping), Dialogue (service, routes, voice tools, greeting), Mind (Mesh
worker choice) · **Related:** [Forge](SAIL_FORGE.md) §8, [Mesh](SAIL_MESH.md),
[Presence](SAIL_PRESENCE.md), [Telegram gateway](SAIL_TELEGRAM_GATEWAY.md), [Android](SAIL_ANDROID.md)

You ask SAIL for a change to its own code during the day ("tonight, make the Capital Today tab
show the next dividend date"). The local model turns the ask into a short **work order**, and you
approve it. Overnight, the Forge nightly runner solves approved errands in isolated worktrees.
It uses your paired PC when it's free and the host otherwise, and verifies each patch with the
usual gates. In the morning SAIL tells you what's ready. You read the diff, then **Ship** it as a
draft PR, **Redo** it with a note, or **Drop** it. Nothing merges, and your checkout is never
touched.

## 1. What the owner gets

| Where | What |
|---|---|
| **Night Shift page** (web: the ☾ button in the top bar; Android: Home → Night Shift) | Ask box, errands by state, tonight's plan preview, the morning report, a diff viewer, and Ship / Redo / Drop. |
| **Voice** | "Tonight, fix …" → `nightshift:add`. "What did you do overnight?" or "tell me about that change" → `nightshift:status` (includes a diff excerpt). "Ship it" → `nightshift:review`, which first asks a yes/no question. |
| **Telegram** | `/tonight <ask>` adds an errand. Approval still happens in the app. |
| **Arrival greeting** | The morning report becomes one Pulse signal (`night_shift_morning`, at most once per date), so the presence greeting can say what's ready. |
| **Settings → Night Shift** | On/off (**off by default**), window hours, items per night, include `forge-ready` issues, use the Mesh PC, external solves per night and provider, morning greeting. |

## 2. Lifecycle

```text
 ask ─► Shaping ─► Proposed ──approve──► Queued ─► Working ─► Ready ──Ship──► Shipping ─► Shipped (draft PR)
          │  ▲         │                  ▲  ▲         │        │                 │
          ▼  └─reshape─┘                  │  └─requeue─┘        ├──Redo(note)─► Queued
     Unshapeable                          │  (host outage,      ├─► NeedsYou (question)
                                          │   voice pre-empt)   └─► Failed (reason)
                                          └──────────── Redo ───────────┘
 Drop: any state except Shipping/Shipped.     ShipFailed: Shipping → Ready.
```

- **Shaping** uses the host model at `Background` priority, so live voice pre-empts it. If the
  model is down, the errand stays in Shaping and is retried every 10 min. The work order is
  validated: title, why, ≤ 8 acceptance checks, 1–6 target files that exist, and size S/M (L means
  "split it"). An edited order is validated the same way when you approve it.
- **Planning** is deterministic. Approved errands come before `forge-ready` issues, up to
  `max_items`, within the window. The plan preview shows the deferred items and why they were
  left out.
- **The night** runs one item at a time through Forge's pipeline: solve, then tests, gates and
  self-review. A failure that isn't the patch's fault (model server outage, or the owner
  speaking) requeues the item without spending the attempt. A host outage also stops the night,
  and the morning report says why (`halted`).
- **Where it runs:** `MeshPool::night_worker` picks a Ready, non-resting, non-busy paired worker
  that offloads Background work. Workers are ranked by load, then round-trip time. The solver
  talks to it over the worker's pinned TLS client. If the Mesh transport fails, the job retries
  once on the host. `ran_on` on the result card says where the job ran.
- **External solve** (optional, budget 0 by default): when a local attempt ends in NeedsYou or
  Failed, Claude Code or Codex (`external_provider`) may get **one** try per errand, within the
  nightly budget. The CLI sees the work order, the failure and the target files as they were at
  the base commit. Its reply must be SEARCH/REPLACE blocks, and the patch is verified exactly
  like a local one.
- **Shipping** applies the stored patch in a throwaway worktree on `origin/main`, pushes
  `nightshift/<errand id>` and opens a **draft** PR. Owner errands are only shipped when the
  owner presses Ship. `forge-ready` GitHub issues keep their old behaviour: a verified issue
  opens its draft PR automatically when publishing is on.

## 3. HTTP (owner bearer token, nested at `/api/nightshift`, `Cache-Control: no-store`)

| Route | Result |
|---|---|
| `GET /` | `{settings, tonight:{window, plan}, errands, morning, mesh, external}` |
| `POST /errands` `{ask, source?}` | 201 errand; 422 if the ask is empty or too long |
| `POST /errands/:id/approve` `{order?}` | 200; 422 `{reasons}` for an invalid edited order |
| `POST /errands/:id/reshape` `{note}` | 202 |
| `POST /errands/:id/redo` `{note}` | 200 |
| `POST /errands/:id/ship` | 202 (`shipping`); 409 if there is no stored patch |
| `POST /errands/:id/drop` | 200 |
| `GET /errands/:id/diff` | `text/x-diff` |
| `PUT /settings` | the saved settings object |

Status codes: an unknown errand returns 404, an illegal transition ("can't … while …") returns
409, and anything else is 422. Bodies are capped at 32 KB. Open dialogue sockets receive
`{"type":"nightshift","errand"}` and `{"type":"nightshift_morning","report"}` as things change.

## 4. State on disk

Everything lives under the Forge state dir (`SAIL_FORGE_STATE_DIR`, default
`/srv/sail-data/forge`):

- `nightshift/errands.json`: all errands, written atomically. A corrupt file is quarantined, not
  overwritten.
- `nightshift/patches/<id>.patch`: the stored patch of each result.
- `nightshift/settings.json`: live settings. Changes apply to the next decision without a
  restart.
- `nightshift/morning/YYYY-MM-DD.json`: the morning reports.
- `nightly_state.json`: Forge's per-issue attempts, plus `external_used` (date → count).

The Mesh client and its bearer secret are kept in memory only and never written or printed.

## 5. Operate

- Turn it on in Settings → Night Shift. The scheduler wakes every 10 min and runs once inside the
  window.
- Kill switch: `touch $SAIL_FORGE_STATE_DIR/PAUSE`, as for Forge.
- With Night Shift attached, `forge-ready` issues run only while Night Shift is on, and only when
  "include issues" is checked. `SAIL_FORGE_NIGHTLY` alone no longer starts the issue-only path,
  except when the Night Shift store can't open.
- Tests:
  - `cargo test -p sail-forge --test 'nightshift_*' --test nightly_runner_test` for the store,
    shaping, plan, morning, runner and external solve;
  - `cargo test -p sail-dialogue --test nightshift_e2e` for ask → draft PR through the real
    routes;
  - `cargo test -p sail-mind --test mesh_e2e` for the worker choice;
  - `scripts/ui/nightshift_browser_test.py` for the web page at 4 widths;
  - `./gradlew :app:testDebugUnitTest` for Android.
