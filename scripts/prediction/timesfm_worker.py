"""Tensor-only bridge. Rust owns prices, dates, validation, jobs and presentation.

No provider requests or model downloads are allowed during inference.
"""
import contextlib
import json
import pathlib
import sys


def main():
    checkpoint = pathlib.Path(sys.argv[1]).resolve(strict=True)
    revision = (checkpoint / "REVISION").read_text().strip()
    if len(revision) != 40 or any(c not in "0123456789abcdef" for c in revision):
        raise ValueError("Checkpoint REVISION must contain its Hugging Face commit SHA")
    data = json.load(sys.stdin)
    # Libraries may print progress to stdout. Reserve stdout for the protocol.
    with contextlib.redirect_stdout(sys.stderr):
        import numpy as np
        import torch
        import timesfm
        torch.set_num_threads(2)
        model = timesfm.TimesFM_2p5_200M_torch.from_pretrained(
            str(checkpoint), local_files_only=True, torch_compile=False
        )
        closes = np.asarray(data["closes"], dtype=np.float32)
        horizon = int(data["horizon"])
        if not (32 <= len(closes) <= 15000 and 1 <= horizon <= 1000):
            raise ValueError("Invalid tensor dimensions")
        if not np.isfinite(closes).all() or not (closes > 0).all():
            raise ValueError("Invalid input prices")
        model.compile(timesfm.ForecastConfig(
            max_context=len(closes), max_horizon=horizon,
            normalize_inputs=True, use_continuous_quantile_head=True,
            force_flip_invariance=True, infer_is_positive=True,
            fix_quantile_crossing=True, per_core_batch_size=1,
        ))
        with torch.inference_mode():
            median, quantiles = model.forecast(horizon=horizon, inputs=[closes])
        # Default output is [point head, q10, q20, ..., q90]. The returned point
        # forecast is the q50 column, not an expected/mean price.
        if quantiles.shape != (1, horizon, 10):
            raise ValueError("Unexpected TimesFM quantile shape")
    json.dump({"model": "google/timesfm-2.5-200m-pytorch",
               "revision": revision, "median": median[0].tolist(),
               "deciles": quantiles[0, :, 1:].tolist()}, sys.stdout, allow_nan=False)


if __name__ == "__main__":
    main()
