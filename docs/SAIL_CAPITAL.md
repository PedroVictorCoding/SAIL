# SAIL Capital

SAIL Capital is the owner's private investment research subsystem (EPIC #422). Its job is
after-tax, risk-aware, long-horizon growth. It is not there to produce trading signals. This
page records the architecture decision (CAP-01) and what the first vertical slice does.

## Status

Round 7 (2026-11-10) builds the **portfolio foundation**:

- import official XTB report exports (CAP-26);
- keep an append-only private ledger;
- project positions, cash, dividends and fees from that ledger;
- reconcile the projection against the broker's own figures;
- serve the result read-only at `/api/capital`.

Research adapters (SEC, House/Senate, FRED/ECB, Polymarket), the risk engine and any
forecasting model come in later rounds, and only once this data foundation is trustworthy.

## ADR-0001: one crate, ledger first

**Context.** The EPIC proposes six `sail-capital-*` crates. SAIL has no financial code yet.
Each existing connector (Moodle, Tracker) is a single crate with its HTTP routes in
`sail-dialogue` and a page under `crates/sail-dialogue/web/`.

**Decision.** Start with **one crate, `sail-capital`**, organised by module:

| Module | Owns |
|---|---|
| `contracts` | Typed money (`Decimal` plus currency), instruments, times, parsed rows, snapshots, issues, and the helpers every slice shares |
| `import::xtb` | Turning official XTB XLSX/CSV exports into a `ParsedReport`. Pure: no I/O, no network, no clock |
| `store` | SQLite, append-only: import batches with file hashes, entries keyed by natural key, conflicts, snapshots |
| `ledger` | A pure, deterministic projection with FIFO lots, per-currency cash, dividends with withholding, fees, realized results and reconciliation |
| `service` | Wiring the store, importers and ledger together for the HTTP layer |

The HTTP facade lives in `sail-dialogue/src/capital_http.rs`, behind owner authentication
like the Moodle routes. Split into more crates only when a second consumer needs one module
without the others. Research and quant work are the likely first candidates.

**Why not six crates now.** Six crates would mean six manifests and six sets of public
API, all for one consumer. Every boundary would be guessed before any real data had passed
through it.

**Integration with the rest of SAIL.**

- The ledger is its own authoritative typed store. It is **not** Arena memory, and
  conversational mentions of holdings never become positions.
- Flow, Arena, Agents and Mind come in with the research rounds.
- No new inference daemon, and no model in this slice.

**`.sea` / financial.sea.** This is a gated research experiment (CAP-20/CAP-28). Nothing in
this slice depends on it.

## Invariants this slice enforces

1. **Read-only, no credentials.** SAIL reads only files the owner exports and uploads. It
   never signs in to XTB and never asks for a password, session or 2FA. It never places,
   modifies or cancels orders. XTB's API was discontinued on 2025-03-14.
2. **Exact money.** Every amount is a `rust_decimal::Decimal` with a currency, and JSON
   carries amounts as strings. Different currencies are never summed. There is no FX feed
   yet, so totals are per currency.
3. **Nothing is silently dropped.** A row the importer can't classify becomes an
   `Unsupported` entry or an `ImportIssue`. Every sheet reports rows seen, accepted and
   rejected.
4. **Append-only and idempotent.** An entry's identity is its natural key: the broker id
   when the row has one, else a hash of the normalised row. Re-importing the same file adds
   nothing. If the same key comes back with different content, that is a conflict for the
   owner to resolve and is never overwritten.
5. **Point in time.** Every record has an event time (`occurred`) and a "known since" time
   (`ingested_at`). A projection `AsOf { occurred_by, known_by }` sees only records inside
   both bounds, so a later import can't leak into an earlier answer.
6. **CFDs apart.** Leveraged contracts never merge with owned shares, in identity
   (`Instrument::key`) or in positions.
7. **The broker is evidence, not truth.** The ledger reconciles against the broker's
   snapshot and reports each variance. The UI shows "XTB data as of <time>", never a live
   balance.
8. **Private.** Account numbers are masked to their last four digits. Reports and the
   ledger stay on the host, and nothing is sent to any external service.

## Not in this slice

- Market prices, FX conversion and EUR totals across currencies.
- Tax computation. Withholding is recorded and matched to dividends, but the slice does
  not estimate Portuguese tax due.
- Research, forecasts and proposals.
- The XTB browser companion (CAP-27), which depends on reviewing XTB's terms first.
- Multi-user or public use.
