#!/usr/bin/env bash
# Download the lightweight local vision describer used when the main model has no vision
# projector: LiquidAI LFM3-Omni-2B (GGUF, Q4_K_M weights + Q8_0 multimodal projector).
# About 1.6 GB. Needs llama.cpp b11960 or newer. Licence: LFM Open License v1.0
# (https://huggingface.co/LiquidAI/LFM3-Omni-2B). SAIL never downloads models by itself;
# run this once, then restart SAIL.
set -euo pipefail

DEST="${SAIL_VISION_MODEL_DIR:-${SAIL_RUNTIME:-/srv/sail}/models/vision}"
REPO="https://huggingface.co/LiquidAI/LFM3-Omni-2B-GGUF/resolve/main"
FILES=("LFM3-Omni-2B-Q4_K_M.gguf" "mmproj-LFM3-Omni-2B-Q8_0.gguf")

mkdir -p "$DEST"
for file in "${FILES[@]}"; do
  if [[ -s "$DEST/$file" ]]; then
    echo "Already present: $DEST/$file"
    continue
  fi
  echo "Downloading $file to $DEST ..."
  curl -fL --retry 3 --continue-at - -o "$DEST/$file.part" "$REPO/$file"
  mv "$DEST/$file.part" "$DEST/$file"
done
echo "Vision describer ready in $DEST. Restart SAIL to use it."
