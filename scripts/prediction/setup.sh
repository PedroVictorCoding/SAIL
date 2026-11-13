#!/usr/bin/env bash
# Explicit installation only; inference never downloads weights.
set -euo pipefail
cd "$(dirname "$0")/../.."
TIMESFM_VENV=${SAIL_TIMESFM_VENV:-/srv/sail-data/venvs/timesfm}
TIMESFM_CHECKPOINT=${SAIL_TIMESFM_CHECKPOINT:-/srv/sail-data/models/timesfm-2.5}
SETUP_PYTHON=${SAIL_SETUP_PYTHON:-python3}
"$SETUP_PYTHON" -m venv "$TIMESFM_VENV"
"$TIMESFM_VENV/bin/python" -m pip install --index-url https://download.pytorch.org/whl/cpu 'torch==2.8.0'
"$TIMESFM_VENV/bin/python" -m pip install 'timesfm[torch]==2.0.2'
"$TIMESFM_VENV/bin/python" - "$TIMESFM_CHECKPOINT" <<'PY'
import pathlib
import sys
from huggingface_hub import HfApi, snapshot_download
model = "google/timesfm-2.5-200m-pytorch"
revision = HfApi().model_info(model).sha
path = pathlib.Path(snapshot_download(model, revision=revision,
    local_dir=sys.argv[1], allow_patterns=["config.json", "model.safetensors"]))
(path / "REVISION").write_text(revision + "\n")
print(f"Installed {model}@{revision} at {path}")
PY
