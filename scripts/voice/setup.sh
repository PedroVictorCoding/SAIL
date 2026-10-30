#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
# Python 3.11/3.12 has supported wheels for all speech runtimes.
PYTHON=${SAIL_SETUP_PYTHON:-python3.11}
if [[ ! -x .venv-voice/bin/python ]]; then
  "$PYTHON" -m venv .venv-voice
fi
.venv-voice/bin/python -m ensurepip --upgrade
.venv-voice/bin/python -m pip install --upgrade pip
.venv-voice/bin/python -m pip install -r scripts/voice/requirements.lock
.venv-voice/bin/python scripts/voice/download.py
