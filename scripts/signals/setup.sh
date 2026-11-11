#!/usr/bin/env bash
# Installs the model-signal helpers (Chronos-Bolt, YOLOv8 chart patterns) for SAIL Capital.
# CPU only. About 1.8 GB on the data disk, plus model weights on first use.
set -euo pipefail
cd "$(dirname "$0")/../.."
VENV=${SAIL_SIGNALS_VENV:-/srv/sail-data/venvs/signals}
PYTHON=${SAIL_SETUP_PYTHON:-python3}
if [[ ! -x "$VENV/bin/python" ]]; then
  "$PYTHON" -m venv --without-pip "$VENV"
fi
"$PYTHON" -m pip --python "$VENV/bin/python" install --index-url https://download.pytorch.org/whl/cpu torch torchvision
"$PYTHON" -m pip --python "$VENV/bin/python" install -r scripts/signals/requirements.txt
"$VENV/bin/python" -I -c "import torch, chronos, ultralytics, mplfinance; print('ok', torch.__version__)"
