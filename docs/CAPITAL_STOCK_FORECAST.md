# Capital stock forecast

Capital’s **Stock forecast** tab (`/capital#prediction`) reimplements the workflow of
[MaherGebrail/Stock-Prediction-with-TimesFM](https://github.com/MaherGebrail/Stock-Prediction-with-TimesFM),
reviewed on 2026-11-13: arbitrary Yahoo ticker, displayed history period, observation
interval, forecast horizon and observed/forecast price chart. It is a research view,
separate from the calibrated baselines, Outlook blend, signals and trading rules.
It neither places orders nor adds its outputs to portfolio recommendations.

## Ownership and fidelity

Rust owns the authenticated API, provider HTTP calls and limits, parsing, input
validation, completed-bar filtering, context selection, display-period filtering,
calendar arithmetic, durable job state, output validation and SVG chart. Browser
JavaScript only submits forms, polls status and presents server results. The only
Python code on the inference path loads and runs Google’s original TimesFM tensors.
There is no FastAPI server, yfinance/pandas dependency or cloud inference service.
A Rust tensor port remains a future migration requiring numerical parity tests;
changing the underlying model would not reproduce this reference accurately.

The model is **google/timesfm-2.5-200m-pytorch**, not TimesFM 3.0. Google’s source and
2.5 weights are Apache-2.0. The reference repository is MIT; this implementation
was written independently from the observed behavior. See
[Google’s TimesFM repository](https://github.com/google-research/timesfm).

Supported display periods: 1d, 5d, 1mo, 3mo, 6mo, 1y, 2y, 5y, 10y, ytd, max.
Intervals: 1m, 2m, 5m, 15m, 30m, 1h, 90m, 1d, 5d, 1wk, 1mo, 3mo.
Horizon: 1–1000 observation steps (reference UI allowed 1–20).
The displayed period never limits model input: latest 15,000 available completed
bars, with a minimum of 32, regardless of history shown.

TimesFM uses normalization, continuous quantile head, flip invariance, positivity
inference and quantile-crossing correction, matching the reference configuration.
The returned point is **the median**, not expected price. Unlike the reference,
SAIL also displays the model’s 10th–90th percentile band and retains all nine deciles
in JSON. Rust rejects wrong model identities, missing steps, negative/nonfinite
prices, crossed quantiles and disagreement between the point and q50 column.

## Install and enable

From the deployed SAIL source directory, using Python **3.11 or 3.12** with venv/pip:

```sh
SAIL_SETUP_PYTHON=python3.11 bash scripts/prediction/setup.sh
```

This explicitly downloads CPU PyTorch 2.8.0, TimesFM 2.0.2 and the 2.5 checkpoint.
Model weights and the interpreter go on the data disk, outside Git. Setup resolves
and downloads one immutable Hugging Face model revision, recorded in `REVISION`.
Inference always loads that local directory with Hub offline mode; it never fetches
weights or sends prices to a model service. The exported result records the revision.
Allow several GB of free disk/RAM. CPU inference runs one job at a time, uses two
Torch threads, and has a five-minute end-to-end deadline. No GPU is required.

Set these variables for the SAIL daemon and restart it:

```sh
SAIL_TIMESFM_ENABLED=1
SAIL_PRICES_SOURCE=yfinance
```

`SAIL_PRICES_SOURCE=yahoo` is also accepted by this tab. This reuses the existing
explicit Yahoo network opt-in; every run sends the entered ticker and chosen
interval to `query1.finance.yahoo.com`. It sends no account, position or ledger
information. Yahoo may reject/rate-limit this unofficial API. SAIL reports refusal
without bypassing authentication or blocking. Existing daily-price imports and
refreshes continue using their existing adapters.

Optional paths (relative script paths resolve from the daemon’s working directory):

| Variable | Default |
| --- | --- |
| `SAIL_TIMESFM_PYTHON` | `/srv/sail-data/venvs/timesfm/bin/python` |
| `SAIL_TIMESFM_SCRIPT` | `scripts/prediction/timesfm_worker.py` |
| `SAIL_TIMESFM_CHECKPOINT` | `/srv/sail-data/models/timesfm-2.5` |
| `SAIL_TIMESFM_VENV` | Setup-only destination `/srv/sail-data/venvs/timesfm` |
| `SAIL_SETUP_PYTHON` | Setup-only interpreter `python3` |

Run an offline smoke test after installation:

```sh
python3 -c 'import json; print(json.dumps({"closes":[100+i/10 for i in range(64)],"horizon":5}))' |
  /srv/sail-data/venvs/timesfm/bin/python -I scripts/prediction/timesfm_worker.py /srv/sail-data/models/timesfm-2.5
```

## Data and interpretation limits

Yahoo’s adjusted-close field is preferred; if unavailable (commonly intraday), the
result explicitly says dividend adjustment is not guaranteed. Null/nonpositive
bars are omitted. Duplicate timestamps retain the last usable price. Bars are
ordered by UTC timestamp. Completion is conservative: one full calendar interval
must have elapsed from the bar timestamp; this can delay the newest daily/weekly/
monthly observation. The count of excluded bars is reported. This avoids silently
forecasting from a still-forming latest bar.

Intraday history requests cover seven days for 1m, 59 days for other minute
intervals, and 729 days for 1h. Actual availability depends on Yahoo; unsupported
requests fail explicitly. Coarser requests begin at Unix epoch. Provider bodies
are capped at 8 MiB, worker replies at 1 MiB. Requests do not follow redirects.

Period windows and projected dates use UTC and month-end-clamped calendar arithmetic,
anchored to the last completed observation, matching the reference’s calendar-grid
approach. **Forecast steps are not known future exchange sessions.** Projections
include weekends, holidays and closed overnight periods. The UI calls them calendar
projections, gives the exchange timezone as provenance, and never labels them trading
days. An exchange-calendar migration needs market-specific holiday and DST tests.
The chart uses separately labelled horizontal scales for observed history and future
steps, so a long history does not hide the forecast. Bucket extrema retain historical
spikes; full observations are retained in JSON export.

Model quantiles are uncalibrated estimates, not guaranteed confidence bounds. No
accuracy percentage, positive-return probability or claim of beating a baseline is
published. Historical evaluation would need leakage-aware post-training holdouts
and comparisons against Capital’s existing no-change baseline before influencing
other Capital views.

## API and lifecycle

Both routes require the existing owner bearer token and use `Cache-Control: no-store`.
They work without an imported account.

- `GET /api/capital/prediction`: opt-in flags, model identity and latest job/result.
- `POST /api/capital/prediction`: `{symbol, period, interval, horizon}`; validates input,
  persists `running`, returns 202 and runs independently of the HTTP connection.
  Concurrent starts are rejected; unknown request fields are rejected.

The latest request/state/result lives in `<capital-dir>/predictions/latest.json`,
replaced atomically. Only one latest job is retained. A daemon restart exposes a
previously running job as `interrupted`; it never silently repeats provider calls
or labels it completed. The browser polls and reconnects without resubmitting. This
job is a bounded Capital computation, not a separate scheduler/daemon. A future
Core Continuity integration can add checkpointed resume and job history; current
jobs require an explicit rerun after interruption. Missing dependencies or checkpoint,
provider refusals and invalid model results surface as failed jobs, never baselines.
