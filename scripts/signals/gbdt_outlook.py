"""Gradient-boosted quantile regression component for SAIL Capital outlooks (gbdt).

Request (stdin): {"horizons": [h], "items": [{"key", "origins": [date], "bars": [[date,
open, high, low, close]]}]}.
Reply (stdout): {"model_version", "paths": [[key, origin, horizon, step, [q05, q16, q50,
q84, q95]]], "skipped", "notes"}. Only the horizon step is given; SAIL fills the path.

No look-ahead. Models are refit once per calendar month, at the month's first origin. A
refit trains only on rows whose outcome session is on or before that origin, pooled over
every instrument, one row every 5 sessions. Every origin in the month uses that model.
"""

import math
import os
import sys
from collections import defaultdict

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import fail, read_request, reply  # noqa: E402

LEVELS = [0.05, 0.16, 0.5, 0.84, 0.95]
MIN_ROWS = 400
ROW_STEP = 5
LOOKBACK = 252


def features(closes, i):
    """Features at index i from closes[: i + 1] only, or None."""
    if i < LOOKBACK:
        return None
    c = closes
    logc = [math.log(x) for x in c[i - LOOKBACK : i + 1]]
    last = logc[-1]

    def ret(n):
        return last - logc[-1 - n]

    rets = [logc[k] - logc[k - 1] for k in range(1, len(logc))]

    def vol(n):
        window = rets[-n:]
        m = sum(window) / n
        return math.sqrt(sum((r - m) ** 2 for r in window) / (n - 1))

    def dist_ma(n):
        window = c[i - n + 1 : i + 1]
        return math.log(c[i] / (sum(window) / n))

    high = max(c[i - LOOKBACK + 1 : i + 1])
    gains = [max(r, 0) for r in rets[-14:]]
    losses = [max(-r, 0) for r in rets[-14:]]
    avg_gain, avg_loss = sum(gains) / 14, sum(losses) / 14
    rsi = 100.0 if avg_loss == 0 else 100 - 100 / (1 + avg_gain / avg_loss)
    return [
        ret(5), ret(21), ret(63), ret(126), ret(252),
        vol(21), vol(63),
        dist_ma(50), dist_ma(200),
        math.log(c[i] / high),
        rsi / 100.0,
    ]


def main():
    request = read_request()
    try:
        import numpy as np
        import sklearn
        from sklearn.ensemble import HistGradientBoostingRegressor
    except Exception as exc:  # noqa: BLE001
        return fail(f"missing packages: {type(exc).__name__}")

    horizons = [int(h) for h in request["horizons"]]
    series = []  # (key, dates, closes, index by date)
    for item in request["items"]:
        bars = [b for b in item["bars"] if b[4] and b[4] > 0]
        dates = [b[0] for b in bars]
        closes = [float(b[4]) for b in bars]
        series.append((item["key"], dates, closes, {d: k for k, d in enumerate(dates)}, item["origins"]))

    # Feature rows once per instrument and index (every ROW_STEP sessions from LOOKBACK).
    rows = []  # (instrument, index, feature vector)
    feature_at = {}
    for s, (key, dates, closes, _index, _origins) in enumerate(series):
        for i in range(LOOKBACK, len(closes)):
            f = features(closes, i)
            if f is not None:
                feature_at[(s, i)] = f
                if (len(closes) - 1 - i) % ROW_STEP == 0:
                    rows.append((s, i))

    by_month = defaultdict(list)  # month -> [(series idx, origin index, origin date)]
    skipped = 0
    for s, (key, dates, closes, index, origins) in enumerate(series):
        for origin in origins:
            i = index.get(origin)
            if i is None or (s, i) not in feature_at:
                skipped += 1
                continue
            by_month[origin[:7]].append((s, i, origin))

    paths = []
    fits = 0
    for month in sorted(by_month):
        jobs = by_month[month]
        cutoff = min(origin for _s, _i, origin in jobs)
        for h in horizons:
            x, y = [], []
            for s, i in rows:
                dates, closes = series[s][1], series[s][2]
                j = i + h
                if j < len(closes) and dates[j] <= cutoff:
                    x.append(feature_at[(s, i)])
                    y.append(math.log(closes[j] / closes[i]))
            if len(y) < MIN_ROWS:
                skipped += len(jobs)
                continue
            x, y = np.asarray(x), np.asarray(y)
            preds = []
            for q in LEVELS:
                model = HistGradientBoostingRegressor(
                    loss="quantile", quantile=q, max_iter=200, learning_rate=0.05,
                    max_leaf_nodes=15, min_samples_leaf=40, l2_regularization=1.0, random_state=0,
                )
                model.fit(x, y)
                fits += 1
                preds.append(model.predict(np.asarray([feature_at[(s, i)] for s, i, _o in jobs])))
            for n, (s, _i, origin) in enumerate(jobs):
                qs = sorted(float(p[n]) for p in preds)  # repair any quantile crossing
                paths.append([series[s][0], origin, h, h, qs])

    reply({
        "model_version": f"sklearn-hgb-quantile@{sklearn.__version__}/features-v1",
        "paths": paths,
        "skipped": skipped,
        "notes": [f"{fits} quantile models fitted (monthly refits, pooled over {len(series)} instruments)."],
    })
    return 0


if __name__ == "__main__":
    sys.exit(main())
