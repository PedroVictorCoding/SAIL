"""YOLOv8 Up/Down trend view for SAIL Capital (ChartTrend).

For each origin, draws a candlestick chart of the last 120 sessions up to that origin and
runs the `foduucom/stockmarket-future-prediction` detector, which marks "Up" and "Down"
regions. Detections ending in the last quarter of the chart score +confidence for Up and
-confidence for Down. The weights are checked like the pattern detector's (yolo_common).
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import cut, fail, read_request, reply
from yolo_common import load_checked, render, right_edge_score

REPO = "foduucom/stockmarket-future-prediction"
REVISION = "310919ed9d2d9cce9c1095cd50bb0a04de8c9e68"
WINDOW = 120
RIGHT_EDGE = 0.75
CONFIDENCE = 0.25
DIRECTION = {"up": 1.0, "down": -1.0}
def main():
    request = read_request()
    try:
        model, names = load_checked(REPO, "best.pt", REVISION)
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
                result, image.shape[1], lambda c: DIRECTION.get(names[c].lower(), 0.0), RIGHT_EDGE
            )
            found += n
            values.append([item["key"], origin, score])

    reply({
        "model_version": f"{REPO}@{REVISION[:12]}",
        "values": values,
        "skipped": skipped,
        "notes": [
            f"{found} directional or neutral patterns ending in the last quarter of {len(values)} charts.",
            "The trend detector publishes no accuracy figure and has never been tested for predicting prices.",
        ],
    })
    return 0


if __name__ == "__main__":
    sys.exit(main())
