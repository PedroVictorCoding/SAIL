"""Chronos-Bolt outlook component for SAIL Capital (chronos).

Request (stdin): {"horizons": [h], "steps": {"h": [t]}, "items": [{"key", "origins":
[date], "bars": [[date, open, high, low, close]]}]}.
Reply (stdout): {"model_version", "paths": [[key, origin, horizon, step, [q05, q16, q50,
q84, q95]]], "skipped", "notes"}, with values as log returns from the origin close.

Chronos-Bolt forecasts at most 64 steps, so only horizons up to 64 are answered. Bolt
gives quantiles between 10% and 90%. The 5% and 95% are extended from the 10–90% spread
with normal ratios (1.645 / 1.2816).
"""

import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import cut, fail, read_request, reply  # noqa: E402

REPO = "amazon/chronos-bolt-small"
REVISION = "772f3d25d38aec6d914c8949dab4462e2d46f5d8"
CONTEXT = 512
BATCH = 128
LEVELS = [0.1, 0.16, 0.5, 0.84, 0.9]
TAIL = 1.6449 / 1.2816


def main():
    request = read_request()
    horizons = [int(h) for h in request["horizons"] if 1 <= int(h) <= 64]
    if not horizons:
        return fail("Chronos-Bolt answers horizons up to 64 sessions only")
    steps = {int(h): [int(t) for t in ts] for h, ts in request["steps"].items()}
    longest = max(horizons)
    try:
        import torch
        from chronos import BaseChronosPipeline
    except Exception as exc:  # noqa: BLE001
        return fail(f"missing packages: {type(exc).__name__}")
    torch.set_num_threads(max(1, min(8, (os.cpu_count() or 2) - 2)))
    pipeline = BaseChronosPipeline.from_pretrained(
        REPO, revision=REVISION, device_map="cpu", torch_dtype=torch.float32
    )

    jobs, skipped = [], 0
    for item in request["items"]:
        for origin in item["origins"]:
            history = cut(item["bars"], origin)
            if len(history) < 64 or history[-1][0] != origin:
                skipped += 1
                continue
            closes = [b[4] for b in history[-CONTEXT:]]
            if any(c is None or c <= 0 for c in closes):
                skipped += 1
                continue
            jobs.append((item["key"], origin, closes[-1], torch.tensor(closes, dtype=torch.float32)))

    paths = []
    for start in range(0, len(jobs), BATCH):
        chunk = jobs[start : start + BATCH]
        quantiles, _mean = pipeline.predict_quantiles(
            [job[3] for job in chunk], prediction_length=longest, quantile_levels=LEVELS
        )
        for job, q in zip(chunk, quantiles):
            for h in horizons:
                for t in steps.get(h, [h]):
                    row = [float(v) for v in q[t - 1]]
                    if min(row) <= 0 or not all(math.isfinite(v) for v in row):
                        continue
                    logs = [math.log(v / job[2]) for v in row]
                    q10, q16, q50, q84, q90 = sorted(logs)
                    q05 = q50 + (q10 - q50) * TAIL
                    q95 = q50 + (q90 - q50) * TAIL
                    paths.append([job[0], job[1], h, t, [q05, q16, q50, q84, q95]])

    reply({
        "model_version": f"{REPO}@{REVISION[:12]}",
        "paths": paths,
        "skipped": skipped,
        "notes": [f"Chronos-Bolt small, context {CONTEXT} closes; 5%/95% extended from 10%/90%."],
    })
    return 0


if __name__ == "__main__":
    sys.exit(main())
