"""Shared pieces of the YOLOv8 helpers: the pickle allowlist check and chart rendering."""

import io
import pickletools
import zipfile

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


def load_checked(repo, filename, revision):
    """Downloads the weights, refuses pickles that import anything outside the allowlist,
    turns off Ultralytics analytics, and returns (model, class names)."""
    import os

    from huggingface_hub import hf_hub_download

    path = hf_hub_download(repo, filename, revision=revision)
    bad = unsafe_imports(path)
    if bad:
        raise PermissionError("the YOLO weights import code SAIL does not allow: " + ", ".join(bad[:5]))
    import torch
    from ultralytics import YOLO, settings

    # Ultralytics sends anonymous usage analytics unless told not to.
    settings.update({"sync": False})
    torch.set_num_threads(max(1, min(8, (os.cpu_count() or 2) - 2)))
    model = YOLO(path)
    return model, model.names


def right_edge_score(result, width, direction, right_edge):
    """Sum of direction x confidence over boxes ending in the right part of the chart."""
    score, found = 0.0, 0
    boxes = result.boxes
    for box, cls, conf in zip(boxes.xyxy.tolist(), boxes.cls.tolist(), boxes.conf.tolist()):
        if box[2] / width < right_edge:
            continue
        found += 1
        score += direction(int(cls)) * float(conf)
    return score, found
