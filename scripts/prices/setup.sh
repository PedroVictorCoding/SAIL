#!/usr/bin/env bash
# Installs the yfinance helper SAIL Capital uses when SAIL_PRICES_SOURCE=yfinance.
# The environment lives on the data disk (about 230 MB).
set -euo pipefail
cd "$(dirname "$0")/../.."
VENV=${SAIL_YFINANCE_VENV:-/srv/sail-data/venvs/prices}
PYTHON=${SAIL_SETUP_PYTHON:-python3}
if [[ ! -x "$VENV/bin/python" ]]; then
  # --without-pip works where Debian's python3-venv (ensurepip) isn't installed.
  "$PYTHON" -m venv --without-pip "$VENV"
fi
"$PYTHON" -m pip --python "$VENV/bin/python" install -r scripts/prices/requirements.txt
"$VENV/bin/python" -I -c "import yfinance; print('yfinance', yfinance.__version__)"
