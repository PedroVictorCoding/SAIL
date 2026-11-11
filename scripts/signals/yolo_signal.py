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

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import cut, fail, read_request, reply
from yolo_common import load_checked, render, right_edge_score

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
def main():
    request = read_request()
    try:
        model, names = load_checked(REPO, "model.pt", REVISION)
    except PermissionError as exc:
        return fail(str(exc))
    except Exception as exc:  # noqa: BLE001
        return fail(f"could not load the model: {type(exc).__name__}")

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
            score, n = right_edge_score(
                result, image.shape[1], lambda c: DIRECTION.get(names[c], 0.0), RIGHT_EDGE
            )
            found += n
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
