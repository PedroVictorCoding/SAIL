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

Round 8 adds the first research source, **SEC EDGAR filings** for the US companies the
owner holds (see below). Round 9 adds splits, received shares and portfolio insights.
Round 10 adds **daily prices and baseline forecasts** with an honest out-of-sample score
(see below). House/Senate, FRED/ECB and Polymarket adapters, the risk engine and any neural
or `.sea` model come in later rounds.

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

## Importing XTB reports

1. In XTB's **web** platform, open History. Choose the date range and **Export** the
   cash operations, closed positions and open positions. Reports exported from the mobile
   app leave out open positions.
2. Upload each file on the Capital page. `POST /api/capital/import/xtb` takes the raw
   bytes plus an `X-File-Name` header (`.xlsx` or `.csv`), and accepts these optional
   parameters:
   - `?account=` sets the account for files that don't name one. The account a file
     names always wins. Digits are masked to the last four.
   - `?tz=` sets the time zone for files whose times aren't labelled. XTB's current
     exports label them `(UTC)`, and then this option doesn't apply. The default is
     `Europe/Lisbon`.
   - `?currency=` sets the account currency, for files that don't state one. It is
     never assumed.
3. Re-uploading a file adds nothing. A longer export of the same history adds only the
   rows that are new. If a row comes back with different content, it is listed under
   `/conflicts` and is never applied silently.

The importer finds columns by their header text, in English or Portuguese. It ignores
column positions and sheet names. Every row it can't read is reported as an issue.

The layout was checked against a real web-platform export in November 2026, and
`tests/fixtures/xtb/xtb_2026_layout.xlsx` reproduces it with invented values:
- **Account block.** Each sheet starts with the account number and date range, above the
  table. The open-positions sheet also states the account currency.
- **Times.** They are in UTC, labelled `(UTC)`, and stored as spreadsheet serial numbers.
- **Instrument and Ticker.** "Instrument" is the security's name and "Ticker" its symbol.
- **Category.** It says whether a row is a STOCK, ETF, ETC or CFD. ETCs are treated as
  owned funds.
- **Totals rows.** They are skipped: `Total` and `Profit/loss` sum the rows above them and
  aren't operations.
- **Open positions.** Each instrument row is followed by its individual positions. The
  importer reads the instrument rows, which carry the totals and the value in account
  currency, so nothing is counted twice.
- **Cash operation types.** They include CFD "Dividend equivalent", which is kept with
  the CFD and never mixed with owned-share dividends. Its "Adjustment fee" is recorded
  as a broker fee, not a tax.
- **Comments.** "OPEN BUY SYM 3 @ …" repeats the symbol. In "CLOSE BUY 2/5 @ …", 2 of 5
  units were closed.
- **Splits.** A "Fractional shares" row whose comment says "SYM split 1 for 10" is
  imported as a split (see below).
- **Unclassified types.** "Spinoff", "Correction" and any other "Fractional shares" row
  are kept as unsupported rows with a warning. Their cash still counts. The ledger
  doesn't model spin-offs, so the reconciliation reports any resulting difference.

## Splits and received shares (round 9)

**Splits.** A split of `new` units for every `old` scales every open lot of the holding
by `new / old`. Each lot keeps its total cost, so its per-unit cost changes.

When the broker cashes out a fractional remainder (the split row carries the cash),
that fraction is removed from the **newest** lots first, as XTB does. The ledger records
it as a realized sale: the proceeds are the cash and the cost is the removed lots'
share. A split with nothing held changes nothing and adds a warning.

**Received shares.** Some units have no purchase in the export: promotional shares,
gifts, or transfers in from another broker. The owner records them on the Capital page
(`POST /grants`) with these fields:
- symbol;
- quantity;
- date received;
- an optional cost basis, where blank means zero;
- a note.

Each one opens a lot marked as granted. While granted units with zero cost are held, the
position carries a warning saying so.

Grants are kept apart from imported rows and are never edited or deleted. A mistaken
grant is **retracted**, once, and the time of retraction is recorded. A question asked
as of before the retraction still sees the grant; one asked after it doesn't.

## Portfolio insights (round 9)

`GET /insights` summarises the projection. Every figure uses only the broker's own values
and the ledger: there are no market prices, no FX conversion and no model.

| Figure | How it's computed |
|---|---|
| Allocation | Each owned position's share of the owned value, by the broker's value at its snapshot, plus the same by kind (stock, ETF). CFDs are never included. |
| Concentration | The largest position and the top 5. The Herfindahl–Hirschman index (the sum of squared shares) and its inverse, "as concentrated as N equal holdings". |
| Income | Owned-share dividends minus withholding, plus interest after its tax, by UTC month. Also the trailing 12 months, and a trailing yield on the owned value. CFD dividend equivalents are excluded. |
| Return on deposits | Ledger cash plus owned value, minus net deposits (deposits minus withdrawals; transfers between the owner's own sub-accounts don't count). It reports the gain and the gain over net deposits. |
| Freshness | The broker snapshot's time and age. The data counts as stale after 72 hours. |

A figure that would need two currencies combined is left out, with a warning. Positions
with no broker value are listed and named in a warning.

**Currencies.**
- Cash amounts, purchase values and P/L are in the account currency.
- Per-unit prices (`OPEN BUY 10 @ 150.00`, open price, market price) are in the
  instrument's own trading currency. XTB doesn't state that currency, so these prices
  are recorded as unknown rather than given the wrong label.
- Cost basis is the cash actually paid, in the account currency.
- The value at export is the broker's purchase value plus gross P/L.

## SEC filings (round 8)

SAIL reads SEC EDGAR's public JSON: the ticker map (`company_tickers.json`) and, for each
held US issuer, its recent filings (`data.sec.gov/submissions/CIK##########.json`). Only
"give me this company's filings" leaves the host; holdings, quantities and values never do.

**Setup.** SEC requires every request to name who is asking. Set
`SAIL_SEC_USER_AGENT="<name> <contact email>"`. Without it SAIL makes no SEC requests at
all, and `/sec/status` says so. `SAIL_SEC_REFRESH_HOURS` sets the background refresh
interval (default 12; the first run is a minute after start).

**Limits.** At most 2 requests per second, 16 MB per response, 200 issuers per refresh and
200 filings per issuer. A 429 or 403 stops the whole run and is reported; SAIL doesn't
retry until the next scheduled refresh.

**Linking holdings to issuers.** Only `.US` symbols are looked up. The ticker must match
exactly one issuer in the map SAIL had at the time; otherwise the match is `unresolved`,
never guessed. The map is refetched at most once a day, and every version is kept.

**Evidence and time.** Each filing is stored append-only with four times: the period it
covers (`period_end`), when SEC published it (`published_at`, EDGAR's acceptance time, or
23:59:59 New York on the filing date when only the date is known), when SAIL first saw it
(`first_observed_at`) and when SAIL stored this version (`ingested_at`). If SEC later changes
a filing, the new version is added and keeps the original `first_observed_at`. A question
asked `AsOf { occurred_by, known_by }` sees a filing only if it was published by
`occurred_by` and observed by `known_by`.

**Watch items.** A fixed rule (no model) turns each visible filing on a held issuer into a
**watch** item or a **research proposal**, with the reason quoting only the form type and
8-K item numbers. There is no buy or sell output.

## Prices and baseline forecasts (round 10)

**What it is.** Statistical ranges for each held instrument's price over 5, 20 and 60
trading sessions (about a week, a month and a quarter), from four simple baselines, scored
out of sample against "no change". It is not advice: nothing proposes or places a trade,
and the page opens with "Statistical ranges from past prices, not predictions of what to
do."

**Getting prices.** Either:
- **upload a price file** (CSV with a date and a close column, comma or semicolon
  separated, English or Portuguese headers). Nothing leaves the host;
- **fetch from Yahoo Finance** through the `yfinance` Python library. Run
  `scripts/prices/setup.sh` once (it installs into `/srv/sail-data/venvs/prices`), then set
  `SAIL_PRICES_SOURCE=yfinance`. Only ticker symbols are sent, never amounts, the account
  or the owner's name. SAIL runs `scripts/prices/yfinance_daily.py` once per symbol with a
  cleared environment, at most one per second, 100 per refresh, 8 MB of output and
  90 seconds each. Fetching only happens when the owner presses **Fetch prices**.
  `SAIL_YFINANCE_PYTHON`, `SAIL_YFINANCE_SCRIPT` and `SAIL_YFINANCE_CACHE` override the
  defaults.

XTB suffixes map to Yahoo's: `.US` has none (`BRK.B.US` becomes `BRK-B`), `.PT` becomes
`.LS`, `.NL` `.AS`, `.UK` `.L`, `.FR` `.PA`, `.ES` `.MC`, `.IT` `.MI` and `.DE` stays
`.DE`. CFDs and unknown suffixes are listed as unmapped and never sent. Yahoo's close is
split-adjusted but not dividend-adjusted. Pence-quoted listings keep `GBX`, so they are
never mistaken for pounds. A Stooq client also exists (`SAIL_PRICES_SOURCE=stooq`), but
Stooq now answers automated downloads with a browser check. SAIL reports that as blocked
and doesn't try to get past it.

**Storage and time.** Bars go into an append-only `price_bars` table. A changed bar is a
new version that keeps the session's original `first_available_at`, and every question
asked "as of" a time sees only the versions stored by then. Prices from machines (Yahoo,
Stooq, comma-separated files) are read as dot decimals. The report parser would refuse
`54.123` as ambiguous.

**The models** (`forecast/`, pure, no I/O):

| Model | Centre | Spread |
|---|---|---|
| `no_change` | zero drift | σ of the last 252 daily log returns × √h |
| `trailing_drift` | mean of the last 252 daily log returns × h | as `no_change` |
| `ewma_volatility` | zero drift | EWMA variance (λ = 0.94) × √h |
| `empirical_horizon` | the past non-overlapping h-session returns of the last 3 years (at least 20) | read directly |

**Safeguards from the EPIC:**
- With fewer than 252 daily returns, a forecast is `insufficient_evidence` with a reason.
  The same applies when the last 252 prices span more than 400 days (the source is missing
  trading days) or, at 60 sessions, when `empirical_horizon` has fewer than 20 samples.
- Typical (median) and average (mean) outcomes stay apart.
- `probability_positive` is published only for a model and horizon that passed the
  calibration check, and only by models that estimate a direction. Zero-drift models would
  always say 50%.

**Evaluation.** A walk-forward over held instruments: origins every 5 sessions after 252
returns of history, each forecast using only earlier closes. Scores:
- CRPS, and skill against `no_change` on the same forecasts;
- pinball loss and Gaussian NLL;
- coverage at each quantile, and the 5–95% interval;
- direction hit rate;
- cross-sectional rank IC (with at least 5 instruments).

**Calibrated** means at least 100 forecasts, a 90% interval holding 85–95% of outcomes, and
42–58% of outcomes below the median. Origins whose outcome spans missing trading days are
skipped and counted. The report's notes state its limits: overlapping windows,
survivorship (only held instruments), and no costs, taxes or FX. The result is cached until
prices change.

**What the first real evaluation showed (2026-11-11).** Volatility models are well
calibrated. EWMA volatility beats `no_change` by about 1–2% CRPS at a week and a month.
Trailing drift is worse than `no_change` at every horizon, by about 12% at a quarter. Rank
IC is near zero. In short, the baselines describe how much prices move, not which way.

## API (`/api/capital`, owner token required)

| Route | Returns |
|---|---|
| `GET /accounts` | Accounts, with currency, last import, last snapshot time and row count |
| `GET /portfolio?account=&occurred_by=&known_by=` | The ledger projection: positions, CFD positions, cash per currency, dividends, realized results, flows, reconciliation and warnings |
| `GET /positions?account=` | Positions and CFD positions, plus `snapshot_as_of` |
| `GET /cashflows?account=&from=&to=` | Ledger rows in `[from, to)`, at most 5,000 |
| `GET /dividends?account=&from=&to=` | Dividends, with withholding matched and net |
| `GET /sync/status?account=` | Import batches, newest first, with per-sheet counts and issues |
| `GET /conflicts?account=` | Keys that came back with different content |
| `POST /import/xtb?account=&tz=&currency=` | The batch record for the upload |
| `GET /filings?account=&occurred_by=&known_by=` | Watch items and research proposals for held issuers, plus how each position matched |
| `GET /sec/status` | Whether SEC access is configured, the last refresh and its counts or error |
| `POST /sec/refresh?account=` | Runs a refresh now and returns the new status |
| `GET /insights?account=&occurred_by=&known_by=` | Allocation, concentration, income, return on deposits and freshness |
| `GET /grants?account=` | Received shares the owner recorded, newest first, retracted ones included |
| `POST /grants?account=` | Records received shares. The body is JSON: `symbol`, `kind` (stock/etf), `quantity`, `received_at`, `cost` (or null), `note` |
| `POST /grants/{id}/retract?account=` | Retracts a grant, once |
| `GET /prices/status` | Whether fetching is on and its source, the last refresh and error, per-instrument coverage and unmapped symbols |
| `POST /prices/refresh?account=` | Fetches daily history for held instruments, then returns the status |
| `POST /prices/import?account=&symbol=&currency=&adjustment=` | Stores an uploaded price file (raw body, `X-File-Name`) for a held symbol |
| `GET /prices?symbol=&from=&to=` | Stored closes for one symbol |
| `GET /forecasts?account=&horizon=&model=` | One forecast per held instrument with prices, plus `without_prices` |
| `GET /forecasts/evaluation?account=` | The walk-forward report: scores, rank IC, skipped instruments and notes |

All amounts are decimal strings. Errors are `{"error": "..."}`, with these status codes:

| Status | Meaning |
|---|---|
| 400 | The input is wrong |
| 404 | No portfolio exists yet |
| 422 | The report can't be read |
| 401 / 503 | Authentication failed or isn't available |

Data lives in `$SAIL_CAPITAL_DIR`, or by default in SAIL's data dir under `capital/`:
- `capital.sqlite3`, the ledger;
- `reports/<sha256>.<ext>`, the original uploads, kept as evidence and readable only by
  the owner.

## Not in this slice

- FX conversion and EUR totals across currencies. Prices are stored in the trading
  currency and feed forecasts only.
- Tax computation. Withholding is recorded and matched to dividends, but the slice does
  not estimate Portuguese tax due.
- Research beyond SEC filings, models beyond the four baselines, and dossiers built from
  research proposals.
- The XTB browser companion (CAP-27), which depends on reviewing XTB's terms first.
- Multi-user or public use.
