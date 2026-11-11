"""Chronos-Bolt view for SAIL Capital (FoundationForecast).

For each origin, gives the pretrained model the last 512 closes up to that origin, and
returns ln(median forecast at the horizon / close at the origin). Higher means the model
expects the price to rise more.
"""

import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import cut, fail, read_request, reply

REPO = "amazon/chronos-bolt-small"
REVISION = "772f3d25d38aec6d914c8949dab4462e2d46f5d8"
CONTEXT = 512
BATCH = 128


def main():
    request = read_request()
    horizon = int(request["horizon"])
    if not 1 <= horizon <= 64:
        return fail("Chronos-Bolt forecasts at most 64 steps ahead")
    try:
        import torch
        from chronos import BaseChronosPipeline
    except Exception as exc:  # noqa: BLE001
        return fail(f"missing packages: {type(exc).__name__}")
    torch.set_num_threads(max(1, min(8, (os.cpu_count() or 2) - 2)))
    pipeline = BaseChronosPipeline.from_pretrained(
        REPO, revision=REVISION, device_map="cpu", torch_dtype=torch.float32
    )

    jobs = []  # (key, origin, last close, context tensor)
    skipped = 0
    for item in request["items"]:
        bars = item["bars"]
        for origin in item["origins"]:
            history = cut(bars, origin)
            if len(history) < 64 or history[-1][0] != origin:
                skipped += 1
                continue
            closes = [b[4] for b in history[-CONTEXT:]]
            if any(c is None or c <= 0 for c in closes):
                skipped += 1
                continue
            jobs.append((item["key"], origin, closes[-1], torch.tensor(closes, dtype=torch.float32)))

    values = []
    for start in range(0, len(jobs), BATCH):
        chunk = jobs[start : start + BATCH]
        quantiles, _mean = pipeline.predict_quantiles(
            [job[3] for job in chunk], prediction_length=horizon, quantile_levels=[0.5]
        )
        for job, q in zip(chunk, quantiles):
            median = float(q[horizon - 1][0])
            if median > 0 and math.isfinite(median):
                values.append([job[0], job[1], math.log(median / job[2])])
            else:
                skipped += 1

    reply({
        "model_version": f"{REPO}@{REVISION[:12]}",
        "values": values,
        "skipped": skipped,
        "notes": [
            f"Chronos-Bolt small, context {CONTEXT} closes, median at {horizon} sessions.",
        ],
    })
    return 0


if __name__ == "__main__":
    sys.exit(main())
