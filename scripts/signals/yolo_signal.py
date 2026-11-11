"""YOLOv8 chart-pattern view for SAIL Capital (ChartPattern).

For each origin, draws a candlestick chart of the last 120 sessions up to that origin
(the model was trained on screenshots of charts), runs the detector, and scores patterns
that end in the last quarter of the chart:
  bullish (W_Bottom, Head and shoulders bottom) +confidence,
  bearish (M_Head, Head and shoulders top) -confidence,
  Triangle and StockLine 0 (no direction).

The weights are a pickle from one author with no stated licence. Before loading them,
every import inside the pickle is checked against an allowlist; anything else is refused.
"""

import io
import os
import pickletools
import sys
import zipfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import cut, fail, read_request, reply

REPO = "foduucom/stockmarket-pattern-detection-yolov8"
REVISION = "31bbb9abc9843dc3d9c08349507c178cde56be31"
WINDOW = 120
RIGHT_EDGE = 0.75
CONFIDENCE = 0.25
DIRECTION = {
    "W_Bottom": 1.0,
    "Head and shoulders bottom": 1.0,
    "M_Head": -1.0,
    "Head and shoulders top": -1.0,
    "Triangle": 0.0,
    "StockLine": 0.0,
}
ALLOWED_MODULES = ("ultralytics.", "torch", "collections", "numpy", "_codecs", "builtins", "copyreg", "__builtin__")
ALLOWED_BUILTINS = {"getattr", "set", "dict", "list", "tuple", "slice", "range", "frozenset", "bytearray", "object"}


def pickle_globals(path):
    """(module, name) of every GLOBAL / STACK_GLOBAL in the checkpoint's pickles."""
    found = set()
    with zipfile.ZipFile(path) as archive:
        for name in archive.namelist():
            if not name.endswith(".pkl"):
                continue
            data = archive.read(name)
            strings = []
            for op, arg, _pos in pickletools.genops(io.BytesIO(data)):
                if op.name in ("SHORT_BINUNICODE", "BINUNICODE", "UNICODE", "BINUNICODE8"):
                    strings.append(arg)
                elif op.name == "GLOBAL":
                    module, _, attr = arg.partition(" ")
                    found.add((module, attr))
                elif op.name == "STACK_GLOBAL" and len(strings) >= 2:
                    found.add((strings[-2], strings[-1]))
    return found


def unsafe_imports(path):
    bad = []
    for module, name in sorted(pickle_globals(path)):
        if module in ("builtins", "__builtin__"):
            if name not in ALLOWED_BUILTINS:
                bad.append(f"{module}.{name}")
        elif not module.startswith(ALLOWED_MODULES):
            bad.append(f"{module}.{name}")
    return bad


def render(bars):
    import matplotlib

    matplotlib.use("Agg")
    import mplfinance as mpf
    import numpy as np
    import pandas as pd
    from PIL import Image

    frame = pd.DataFrame(
        [[b[1], b[2], b[3], b[4]] for b in bars],
        columns=["Open", "High", "Low", "Close"],
        index=pd.DatetimeIndex([b[0] for b in bars]),
    )
    buffer = io.BytesIO()
    mpf.plot(frame, type="candle", style="yahoo", figsize=(6.83, 7.68), savefig=dict(fname=buffer, dpi=100, format="png"))
    buffer.seek(0)
    return np.array(Image.open(buffer).convert("RGB"))[:, :, ::-1]  # BGR, like OpenCV


def main():
    request = read_request()
    try:
        from huggingface_hub import hf_hub_download
    except Exception as exc:  # noqa: BLE001
        return fail(f"missing packages: {type(exc).__name__}")
    path = hf_hub_download(REPO, "model.pt", revision=REVISION)
    bad = unsafe_imports(path)
    if bad:
        return fail("the YOLO weights import code SAIL does not allow: " + ", ".join(bad[:5]))
    import torch
    from ultralytics import YOLO, settings

    # Ultralytics sends anonymous usage analytics unless told not to.
    settings.update({"sync": False})

    torch.set_num_threads(max(1, min(8, (os.cpu_count() or 2) - 2)))
    model = YOLO(path)
    names = model.names

    values, skipped, found = [], 0, 0
    for item in request["items"]:
        for origin in item["origins"]:
            history = cut(item["bars"], origin)[-WINDOW:]
            if len(history) < WINDOW or history[-1][0] != origin or any(
                b[1] is None or b[2] is None or b[3] is None for b in history
            ):
                skipped += 1
                continue
            image = render(history)
            result = model.predict(image, conf=CONFIDENCE, verbose=False)[0]
            width = image.shape[1]
            score = 0.0
            for box, cls, conf in zip(result.boxes.xyxy.tolist(), result.boxes.cls.tolist(), result.boxes.conf.tolist()):
                if box[2] / width < RIGHT_EDGE:
                    continue
                found += 1
                score += DIRECTION.get(names[int(cls)], 0.0) * float(conf)
            values.append([item["key"], origin, score])

    reply({
        "model_version": f"{REPO}@{REVISION[:12]}",
        "values": values,
        "skipped": skipped,
        "notes": [
            f"{found} directional or neutral patterns ending in the last quarter of {len(values)} charts.",
            "Detector accuracy is self-reported (mAP@0.5 0.614); it has never been tested for predicting prices.",
        ],
    })
    return 0


if __name__ == "__main__":
    sys.exit(main())
