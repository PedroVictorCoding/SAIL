# SAIL Capital

SAIL Capital is the owner's private investment research subsystem (EPIC #422). Its job is
after-tax, risk-aware, long-horizon growth. It is not there to produce trading signals.
This page describes what Capital does today, how it is built, and the decisions behind it.

## Status

Capital reads the owner's official XTB exports into an append-only ledger and builds
everything else on that ledger:

- **Portfolio:** positions with trailing stops, CFDs kept apart, allocation, reconciliation
  against XTB's own figures, received shares and splits, and SEC filings for US holdings.
- **Income:** dividends received with withholding, and a projection of future dividends
  with reinvestment.
- **Outlook:** a blended range per holding from daily prices and models, with replays,
  plus trade signals that publish only after passing a replay with a holdout.
- **Track record:** one scorecard that says, in the same words for everything, whether
  any model, signal or setup has earned trust.
- **Today:** a stance and the suggestions that need the owner, sized against limits the
  owner confirmed.

On the owner's real data, nothing has yet earned trust as a predictor of direction. The
page says so plainly rather than inventing signals. House/Senate disclosures, FRED/ECB,
Polymarket, a risk engine and any neural or `.sea` model are not built yet.

## The page

`/capital` is a dashboard with a grouped sidebar (Overview, Account, Research, Others)
and seven sections; on a phone the sidebar becomes a top strip. A ticker of recent closes
runs under the top bar. Old links (`#holdings`, `#research`, `#forecasts`, `#data`)
still resolve.

| Tab | Answers | Holds |
|---|---|---|
| Dashboard | How is it doing, and what needs me? | Account value with the holdings line, figures, market breadth, holding price cards, index tiles, what needs you, calendar, market overview |
| Portfolio | What do I own, and is the ledger right? | Positions with trailing stops, CFDs, allocation, reconciliation, received shares, SEC filings |
| Income | What does it pay? | Dividend projection, dividends received, income summary |
| Signals | What do the signals say about each stock? | The signal board: every holding and watchlist stock or ETF with its read, the four price signals as percentiles, the 1- and 3-month outlook, any trade setup, and a CSV download |
| Outlook | What could happen to a holding? | Holding outlook (fan and replay), trade signals, one-off asset check |
| Track record | Has anything earned trust? | The scorecard, then the evidence: setups, live record, baselines, signal skill |
| Settings | How is it configured? | Limits, watchlist, prices, model helpers, imports |

**Files.** `web/capital.html` holds one region per tab. `capital-core.js` has the
shared state, owner-token fetch, formatting, tabs and boot. Each tab has its own
`capital-<tab>.js` and `capital-<tab>.css`, and `capital.css` holds the shared base.
The scripts are classic `defer` scripts that share globals. Each tab registers
`registerTab(id, { setup, load })`. The header (portfolio and Today's tiles) loads at
start, and a tab loads its own data the first time it opens and again on Refresh.

**House style.** The owner chose a trading-dashboard look (dark glass cards, a bento
grid, area sparklines, green/red change pills, instrument avatars). The rules are at the
end of `capital.css`, and `capital-core.js` has the shared pieces: `sparklineSvg`,
`changePill`, `instrumentAvatar`, `formatPrice` and `loadMarkets`. Detail sits behind
"Show …" disclosures, each caveat appears once, empty sections are hidden, and nothing
overflows at 390 px.

**Market breadth, not sentiment.** The gauge is the share of tracked instruments whose
last close is above their 200-day average: a measured number, not a mood score. The
account-value line is today's holdings valued at past closes (deposits and trades
ignored), and says so.

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
- No new inference daemon. The first slice had no model; later models run as optional local helpers.

**`.sea` / financial.sea.** This is a gated research experiment (CAP-20/CAP-28). Nothing in
Capital depends on it.

## Invariants

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

## Splits and received shares

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

## Allocation and income insights

`GET /insights` summarises the projection. Every figure uses only the broker's own values
and the ledger: there are no market prices, no FX conversion and no model.

| Figure | How it's computed |
|---|---|
| Allocation | Each owned position's share of the owned value, by the broker's value at its snapshot, plus the same by kind (stock, ETF). CFDs are never included. |
| Concentration | The largest position and the top 5. The Herfindahl–Hirschman index (the sum of squared shares) and its inverse, "as concentrated as N equal holdings". |
| Income | Owned-share dividends minus withholding, plus interest after its tax, by UTC month. Also the trailing 12 months, and a trailing yield on the owned value. CFD dividend equivalents are excluded. |
| Account | XTB's own terms, so the figures can be checked against the app: account value = free funds + positions; invested = what the open positions cost (their cost basis); free funds = ledger cash; open profit/loss = positions − invested. |
| Since the first deposit | Deposits and withdrawals added up (transfers between the owner's own sub-accounts don't count), and how far the account value is above or below that net amount. It includes dividends, interest, fees, closed trades and CFD results. |
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

## SEC filings

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

## Today, limits and suggestions

**Layout.** The page opens on **Today**: four account tiles, a one-line stance, and a
ranked list of suggestions. Everything else sits in tabs: Holdings, Income, Forecasts,
Research and Data. The open tab is kept in the URL hash.

**Limits.** SAIL starts from a draft: largest position 10%, free funds between 5% and
15%, high-volatility holdings (above 50% a year) at most 15% together, at most 20% of
the account moved by one proposal, a smallest change of 50, costs of 10 bp (plus 50 bp
for another currency), and caution 4. The owner edits and confirms them in Data →
Limits. Each save is a new append-only version. **Nothing is sized against a draft.**

**Suggestions.** Four kinds, built by fixed rules (`suggest::build`):
- **Attention:** old XTB data, figures that differ from XTB, holdings without prices,
  prices not being fetched, new SEC filings, positions XTB didn't value, and a draft
  policy.
- **Policy:** a confirmed limit is broken. The fix trims only the positions that break
  their own limits. Free funds outside the band become **one free-funds line** ("invest
  about X" or "raise about X"). SAIL has no evidence for which holdings to buy or sell
  with cash, so it doesn't choose them.
- **Signal:** what the signal board says per stock. One item lists the holdings the
  signals are against, and another the watchlist names they favour (at most five). Each
  stock gets its read and 3-month outlook, and each item says once whether the reads
  are validated or only describe the past. They are never sized. Validated reads on
  holdings are medium severity; unproven ones are low.
- **Idea:** validated signals favour a change, and its expected gain beats the extra risk
  and the trading costs. Each idea shows its confidence and what doing nothing means.

Dismissing a suggestion or marking it done is stored append-only. The suggestion's id
hashes its evidence, so it comes back only when the evidence changes. SAIL never places
orders.

**Signals** (`signals/`):
- Price signals: 12-1 momentum, short-term reversal, low volatility, and distance from
  the 200-session average.
- Each one is scored walk-forward by its rank IC on weekly cross-sections of the
  **signal universe**: the owned holdings (shares and ETFs, not CFDs) plus the
  watchlist's stocks and ETFs. Until November 2026 it ranked only the holdings, which
  left too few names for any signal to pass. FX, indices and commodities are left out,
  because ranking a currency against a share says nothing.
- The standard error is widened for overlapping outcomes. The horizon is converted to
  calendar days, about 1.4 per session.
- A signal needs 26 cross-sections, and 8 effectively independent ones, before it can
  pass.
- Its IC is shrunk by two standard errors, so one that isn't clearly better than chance
  gets weight 0.
- Survivors combine as `w = (C + 0.1·I)⁻¹·IC*`, with `α_j = IC_c · σ_j(h) · s_j`
  (Grinold–Kahn).
- Model scores from Python helpers count at a date only when they are at most 10 days
  old.

**Optimizer** (`optimize/`):
- One long-only problem over the holdings and free funds, limited by the confirmed
  limits.
- Ideas maximize `α'w − λ/2·w'Σw − costs`. They run only when some `α ≠ 0`, so caution
  alone never turns into trading advice.
- The covariance comes from weekly returns over two years, shrunk toward constant
  correlation.

**The signal board** (`board.rs`, `GET /signals/board`). One row per instrument in the
signal universe, holdings first:
- **Readings:** each price signal in plain words ("up 34% over the past year, skipping
  the last month") and as a percentile across the board, where 100 is the most
  favourable.
- **Outlook:** the blended outlook at 1 and 3 months: median, 5–95% range, and the
  chance of ending higher, read from the fan's quantiles. The outlook's replay record
  over the holdings (direction hit rate, share inside the 90% range) is shown once
  above the table.
- **Setup:** a trade setup that fired on the latest session, and whether it published.
- **Read:** with a validated combination, its sign (favoured or against beyond ±0.5
  SD, else neutral), marked *validated*. Without one, the agreement of the readings: a
  percentile of 67 or more counts for and 33 or less against, and a net of two either
  way leans. These are marked *unproven*, and the sentence says they describe the past.

The board is cached until prices, model values, the universe, the trade run or the
day change, and the background warm-up builds it every 5 minutes. A cold build over
45 instruments takes about 3 seconds; a cached one is instant. Any row opens the
outlook chart, including for watchlist names.

**First run on the owner's holdings (2026-11-11).** No price signal passed at 60
sessions, so there are no ideas. The draft limits flagged the idle free funds and one
position above 10%.

## Dividend projection with reinvestment

**Income → Dividend projection** projects net dividend income for 1–30 years, month by
month (`dividends/`).

**Inputs per holding:**
- the units held and the latest price;
- FX from the watchlist pairs, else the rate implied by XTB's own valuation;
- the per-share payout history from Yahoo (`scripts/prices/dividends.py`, refreshed
  weekly; only symbols are sent);
- the withholding actually observed in the ledger over 24 months, else a default by
  listing country.

**Reinvestment.** A share of each net payment buys more units the same month. The
allocation can be:
- **proportional** across paying holdings by current value (the default, keeping the
  mix);
- back into the **same holding** (a DRIP);
- **top value** (N largest paying holdings, by value; default N = 5);
- **equal weight** (every paying holding gets the same cash, moving toward equal
  weights);
- **top yield** (N highest-yielding paying holdings, by value);
- **underweight** (the paying holdings furthest below their equal-weight target get
  the pool, a partial rebalance).

The top-value and top-yield counts are 1–10 (`top_n`, default 5); top-yield and
top-value spread the month's pool by current value among the selected holdings. The
rest stays as cash. A comparison line shows the same projection without reinvesting.

**Growth.** Past payout growth over up to five complete years, shrunk toward zero by
`n/(n+3)` and capped at ±15%. A year without payment after a paying year counts as a
cut. Low and high scenarios shift each holding's growth by its own payout volatility.
They are scenarios, not probabilities.

**Prices.** Each holding follows its own price trend over up to three years, shrunk by
`years/(years+3)` and capped at −50% to +15% a year. A holding yielding more than 12%
pays a constant share of its current price, and its price is assumed to fall by at
least `yield − 8%` a year. Total return from yield alone is therefore capped at 8%, so a
fund paying out of its capital can't compound at its headline yield. On real holdings
an 81%-yield fund otherwise turned about 1,100 a year into 337,000.

**Tax.** Withholding is subtracted. The optional Portuguese reserve is 28% of the gross
minus what was withheld, floored at zero (simplified).

**Cross-check with the ledger.** Each receipt is matched to the published payment it
came from, using the units held on the payment date. A history is scaled only when the
receipts consistently differ by more than 1.6× (for example a fund quoted in its base
currency), and a note says so.

## Prices and baseline forecasts

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
  `SAIL_YFINANCE_PYTHON`, `SAIL_YFINANCE_SCRIPT`, `SAIL_YFINANCE_SEARCH_SCRIPT` and
  `SAIL_YFINANCE_CACHE` override the defaults. Symbol search uses
  `scripts/prices/search.py`; it sends only the typed query.

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

## Model signals and the holding outlook

**Model signals.** Python helpers in `scripts/signals/` add three views. Each one goes
through the same out-of-sample IC check as the price signals before it gets any weight:

| Name | Model (pinned revision) | What it gives | Licence | Scored from |
|---|---|---|---|---|
| `yolo` | `foduucom/stockmarket-pattern-detection-yolov8` | Head-and-shoulders, M/W and triangle patterns ending in the last quarter of a rendered 120-session candlestick chart | not stated; Ultralytics is AGPL-3.0, so personal use only | 2025-04-15 |
| `yolo_trend` | `foduucom/stockmarket-future-prediction` | Up/Down regions at the right edge of the same chart | not stated; personal use only | 2023-11-01 |
| `chronos` | `amazon/chronos-bolt-small` | Median 60-session return | Apache-2.0 | 2024-12-01 |

The helpers run over the whole signal universe (holdings plus the watchlist's stocks
and ETFs), not only the holdings, so their values can be ranked and tested like the
price signals. Each model's latest value, when at most 10 days older than the
instrument's last close, is also a **reading** on the signal board: plain text and a
percentile, counted in the read like the price signals. The first refresh after this
change scores the watchlist's back history and takes longer; later runs add only new
weeks.

The "scored from" dates come after each model's weights were published. A pretrained
model may have seen earlier prices, and scoring it on them would flatter it.

Both YOLO weight files are pickles from one author. Before loading one, the helper reads
every import inside the pickle and refuses the file if any import falls outside an
allowlist (`ultralytics`, `torch`, `numpy`, `collections` and a few builtins). Ultralytics
analytics and Hugging Face telemetry are turned off.

**Blended outlook** (`outlook/`). Each holding gets a fan at 1, 3, 6 and 12 months (21, 63,
126 and 252 sessions), built from five components:
- `gbm`: geometric Brownian motion, the stockspy cone, in closed form;
- `ewma`: zero drift with EWMA volatility (the "no change" reference);
- `historical`: past windows of the same length;
- `chronos`: Chronos-Bolt quantile paths, up to 64 sessions;
- `gbdt`: gradient-boosted quantile regression on price features, refit monthly on rows
  whose outcome was already known.

They are pooled linearly, with weights that minimize CRPS over the 156 weekly origins
before each origin. A component that forecast badly loses its weight. Validated signals
can shift the fan at 1 and 3 months. If nothing passed, there is no shift.

A model path older than 10 days is ignored, so a model that stopped running drops out
instead of going stale.

**Weight shrinkage.** Weekly origins overlap, so 156 of them hold only about
`156·5/h` independent outcomes. On real holdings, fitted weights swung to a single
component (100% GBDT one quarter, 100% Chronos the next). Fitted weights are therefore
trusted in proportion `λ = n_eff / (n_eff + 20)`, and the rest goes to equal weights.

**Replay.** The page can trim the last 1, 3, 6 or 12 months. SAIL then rebuilds the whole
outlook at that cutoff, including prices, model paths, weights and signal validation,
from data up to the cutoff only. It draws what actually happened over the fan and says
the percentile where the price ended.

**Track record.** Replays at the end of every month over the last three years, pooled
over the holdings:
- the share inside the 68% and 90% bands (aim 68% and 90%);
- the average percentile (aim 50);
- skill against `ewma` alone;
- how often the median got the direction right.

**First run on the owner's holdings (2026-11-11).** All five models ran on all
holdings in about 45 minutes. No model signal passed:

| Signal | t-statistic at 20 sessions |
|---|---|
| YOLO patterns | 0.67 |
| YOLO trend | 0.47 |
| Chronos | 0.60 |

So the fans have no direction tilt. With shrinkage, the blend's monthly replays over
three years:

| Horizon | Inside 68% band | Inside 90% band | Skill vs no change |
|---|---|---|---|
| 1 month | 68% | 89% | −1% |
| 3 months | 70% | 90% | +0.3% |
| 6 months | 72% | 92% | +0.3% |
| 1 year | 68% | 88% | +0.1% |

The ranges are honest, but no model sees direction better than "no change".

**Page layout.** The Forecasts tab shows one holding at a time: a compact holding,
horizon and replay row, the fan chart, three numbers (median, 68% range, 90% range),
and the replay sentence. Component weights, direction tilt and the full track record
sit behind **How this fan was built**. Baseline forecasts, the walk-forward scorecard
and signal skill live in **Data** as model diagnostics, so the Forecasts tab has one
prediction surface rather than two competing ones.

**Setup.** Run `scripts/signals/setup.sh` once. It installs CPU-only PyTorch, Chronos,
Ultralytics, mplfinance and scikit-learn into `/srv/sail-data/venvs/signals` (about
1.8 GB). `./run.sh` enables `chronos,yolo,yolo_trend,gbdt` by default, then press **Run
models** in Data. Set `SAIL_MODEL_SIGNALS=` (empty) to keep the helpers off, or use a
comma list to choose a subset. The first run downloads the weights and scores years of
weekly charts, which can take an hour. Later runs only add new weeks.

`SAIL_SIGNALS_PYTHON`, `SAIL_SIGNALS_SCRIPTS` and `SAIL_SIGNALS_CACHE` override the
defaults. Helpers receive instrument keys and prices only, never amounts or names.

## Trade signals with a verified record

The **Signals** tab gives near-daily long and short signals like a signals channel:
symbol, side, entry zone, stop loss and three targets, sized to the owner's risk per
trade. Each one carries **its own verified record**, which signal channels don't show.
SAIL never places orders.

**Universe.** The owner's holdings plus a watchlist. The 44 defaults are:
- FX majors and crosses;
- the S&P 500, Nasdaq 100, Dow, DAX, FTSE 100, Euro Stoxx 50 and Nikkei;
- gold, silver, WTI and natural gas;
- large US and EU stocks;
- SPY, QQQ, IWM, TLT and GLD.

Each entry maps the XTB symbol to the Yahoo symbol. The owner edits the list in Data,
and each change is stored append-only. Fetching sends only these symbols to Yahoo.

**Checking any other asset.** The Signals tab has **Check another asset**: a symbol or
name search over Yahoo's listed instruments. Choosing a result fetches that symbol's
daily bars (and earnings dates, for stocks and ETFs) for the look and replays the six
setups against the latest published scoreboard. It shows the same card when a setup fires and its group has passed
validation, or the setups that fired with the reason each did not publish. The one-off
check does not store the bars or add the symbol to the universe; adding it to the
watchlist includes it in later daily runs. A search needs
`SAIL_PRICES_SOURCE=yfinance`, because it is a Yahoo query.

**Setups** (`trade/`). Six fixed rules on daily bars, each with a long and a short mirror:
- trend pullback (RSI(2) against the 50/200 trend);
- volatility-squeeze breakout;
- 55-day Donchian breakout;
- Bollinger reversion with the 200-day trend;
- inside-bar break;
- three-day reversal.

**Plan.**
- Enter at the next open, only inside `close ± 0.25·ATR`.
- Stop at `k·ATR`. Targets at 1R, 2R and 3R. Close a third at each target, and move the
  stop to breakeven after the first.
- Time stop per setup.
- If a bar reaches both the stop and a target, the stop counts. A gap through the stop
  fills at the open.

**Costs** are subtracted in R: spread and commission, XTB's FX conversion on foreign
shares, and overnight financing on CFD positions (every short, and all FX, indices and
commodities).

**Who publishes.** Every past firing over about eight years is replayed as a trade, and
trades are pooled per group (FX; stocks and ETFs; indices and commodities). A setup and
side publish only with:
- at least 100 trades;
- a **lower 95% bound of expectancy above 0R after costs**, with the standard error
  widened for autocorrelated trades;
- a positive mean in a separately judged **two-year holdout**.

Cards are ranked by that lower bound, at most eight a day and one per instrument. Stocks
whose earnings fall inside the trade window are dropped. If nothing qualifies, the page
says so.

**Record.** Every published card is stored and never changed. After it plays out it is
graded by the same rules: closed with its R, or unfilled when the next open missed the
zone. The page shows the totals. That record is the real test of the setups' historical
evidence.

**Holding view.** For each holding, a chandelier trailing stop (highest close of 22
sessions minus 3·ATR(22)) with its distance, whether the close is below it, whether
the price is above the 200-day average, and the next earnings date.

**Schedule.** A run after the US close, at 22:30 UTC on weekdays, when price fetching is
on, plus **Run now** on the page. A run fetches prices for the holdings and the
watchlist, refreshes earnings weekly, publishes and grades. Risk per trade is a limit
(draft 0.5% of the account lost at the stop).

## The track record

Each evaluation used to speak its own language: CRPS skill and calibration for baselines,
rank IC for signals, replay coverage for the outlook, expectancy with a holdout for setups,
and the live record of published cards. `scorecard::summarize` maps all of them onto one
standing:

| Standing | Meaning |
|---|---|
| Earned | Passed its bar out of sample |
| Not yet | Too little evidence to judge |
| Failed | Enough evidence, and it did not pass |

| Family | Earned when | Not yet when |
|---|---|---|
| Baseline forecast (not "no change", the benchmark) | ≥ 100 forecasts, calibrated, CRPS skill > 0 | < 100 forecasts |
| Signal | It kept weight in the combination | Fewer cross-sections than `signals::MIN_ORIGINS` |
| Holding outlook | ≥ 24 replays, skill > 0, 90% band within 0.80–1.00 | < 24 replays |
| Trade setup | It publishes (selection lower bound > 0 and holdout mean > 0) | Fewer trades than `trade::MIN_TRADES` |
| Published signals (live) | ≥ 20 closed and mean R > 0 | < 20 closed |

Each row gets a plain name and one sentence with the number that decided it. The rows
are ordered Earned, Not yet, Failed. `GET /scorecard` serves the scorecard, and the Track
record tab shows it above the full tables.

## Talking to SAIL about it (voice tools)

The voice assistant can discuss the owner's investments through read-only Capital
tools over the same service the page uses. They are offered for finance prompts and
stay available for follow-ups after a finance turn, so "what about Nvidia?" can still
search and check it.

| Tool | Answers |
|---|---|
| `capital:overview` | Value, cash, top positions, stance, top suggestions, latest signals and risk |
| `capital:today` | Stance, active suggestions with reasons and evidence, and sized changes |
| `capital:portfolio` | Every position, cash, allocation, concentration, return, income and warnings |
| `capital:signals` | Published cards, the six-by-two scoreboard, suppressed firings, and the per-stock signal board (read, readings, 3-month outlook, setup) |
| `capital:check` | One-off check of any Yahoo asset against the latest scoreboard |
| `capital:search` | Yahoo symbol and name search for assets outside the universe |
| `capital:watchlist` | The daily signal universe and its symbol mappings |
| `capital:record` | The live graded record: closed count, hit rate, mean and cumulative R |
| `capital:guards` | Trailing stops, trend and next earnings for the holdings |
| `capital:dividends` | Low, base and high dividend income projections |
| `capital:forecast` | Model price distributions at 5, 20 or 60 sessions |
| `capital:policy` | Position, cash, volatility, turnover and risk-per-trade limits |
| `capital:filings` | Recent SEC filings for the holdings and why each matters |

The assistant may suggest ideas freely, including ideas that are not published signals.
It must say which are verified cards and which are its own view, give the main risk,
respect the saved policy, and never imply a guaranteed return. SAIL never places
orders. `capital:search` and `capital:check` need Yahoo prices
(`SAIL_PRICES_SOURCE=yfinance`), and the check needs a recent signal run for the
scoreboard.

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
| `GET /today?account=` | Account tiles, stance, suggestions (dismissed and done ones hidden and counted) and the plans behind them |
| `POST /suggestions/{id}/action?account=` | Body `{"action": "dismissed" \| "done"}`. Records it |
| `GET /policy?account=` | The policy in force (the built-in draft until one is saved) and every version |
| `POST /policy?account=` | Body `{"limits": {...}, "confirm": bool}`. Stores a new version, or returns 400 with the reason |
| `GET /signals?account=` | Signal skill per horizon, the combined IC and the latest combined scores |
| `GET /signals/board?account=` | The signal board: per instrument, the read, readings with percentiles, combined scores, outlook summaries and any setup; plus validated signals and the outlook's record |
| `GET /signals/models` | Each model signal and outlook model: what it is, its licence, whether it is on, the last run and what is stored |
| `POST /signals/refresh?account=` | Starts the model helpers in the background |
| `GET /outlook?account=&symbol=&horizon=&cutoff=` | The blended fan for one holding or watchlist stock or ETF (a replay when `cutoff` is given), with the closes before it and every session date |
| `GET /signals/trade?account=` | The latest run (cards, scoreboard, suppressed firings, notes), when it ran and the next run |
| `GET /signals/trade/search?q=` | Yahoo instrument matches: symbol, name, exchange and asset class (at most 10) |
| `GET /signals/trade/check?account=&symbol=&yahoo=&name=&exchange=&class=` | One asset checked against the latest scoreboard: cards, suppressed firings, group verdicts, recent closes and notes |
| `POST /signals/trade/run?account=` | Fetches prices and earnings, computes, publishes and grades now |
| `GET /signals/trade/record?account=` | Every published card with its grade, and totals |
| `GET /signals/trade/guards?account=` | Trailing stops and trend for each holding |
| `GET /universe/watchlist` / `POST /universe/watchlist` | The watchlist, and a change: `{"op": "add", "item": {...}}` or `{"op": "remove", "symbol": "..."}` |
| `GET /dividends/projection?account=&years=&reinvest_pct=&allocation=&top_n=&reserve_pt_tax=` | Low, base and high projections, the same without reinvesting, per-holding inputs and notes; allocation is `proportional`, `same_holding`, `top_value`, `equal_weight`, `top_yield` or `underweight` |
| `POST /dividends/refresh?account=` | Fetches per-share payout histories for the holdings |
| `GET /outlook/track-record?account=&horizon=` | Coverage, average percentile, skill and direction over monthly replays |
| `GET /markets?account=` | The market board from stored closes: per holding and watchlist instrument the last close, 1D/1W/1M changes, a 30-session spark and whether it is above its 200-day average; breadth; and today's holdings valued over the last 30 sessions |
| `GET /scorecard?account=` | The track record: one row per baseline, signal, outlook horizon, setup and the live record, each Earned / Not yet / Failed with a headline, plus a summary sentence |

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

## History

| Round | PR | Added |
|---|---|---|
| 7 | #425 | XTB import, append-only ledger, reconciliation |
| 8 | #426 | SEC EDGAR filings for US holdings |
| 9 | #429 | Splits, received shares, insights |
| 10 | #433 | Daily prices (yfinance), baseline forecasts, walk-forward score |
| 11 | #434 | Today, risk limits, suggestions, signal skill |
| 12 | #436 | Model signals (YOLO, Chronos), blended outlook with replay |
| 13 | #441 | Trade signals with a verified record, watchlist |
| 14 | #442, #447 | Dividend projection with reinvestment and allocation modes |
| 17 | — | One page structure, house style, lazy loading, the track record, backend modules |
| — | #463 | Fast dividend projection and track records, 30-year monthly dividends |
| — | — | Signals tab: the signal board, a wider signal universe, per-stock suggestions |

## Not built yet

- FX conversion and EUR totals across currencies. Prices are stored in the trading
  currency.
- Portuguese tax beyond the projection's optional 28% reserve. Withholding is recorded
  and matched to dividends.
- Spin-offs. They import as unsupported rows, and the reconciliation shows the difference.
- Research beyond SEC filings: House/Senate disclosures, FRED/ECB, Polymarket, and
  dossiers built from research proposals.
- The XTB browser companion (CAP-27), which waits on a review of XTB's terms.
- Multi-user or public use.
