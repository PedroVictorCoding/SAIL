#!/usr/bin/env bash
# Installs an opt-in user service and polling timer; does not start either.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="${SAIL_LIVE_ROOT:-${HOME}/.local/share/sail-live}"
[[ "$ROOT" = /* && "$ROOT" != / ]] || { echo 'Invalid SAIL_LIVE_ROOT' >&2; exit 2; }
mkdir -p "$HOME/.local/bin" "$HOME/.config/systemd/user" "$HOME/.config/sail-live"
install -m 0755 "$REPO/scripts/sail-live-update.sh" "$HOME/.local/bin/sail-live-update"
install -m 0644 "$REPO/deploy/live/sail-live.service" "$HOME/.config/systemd/user/"
install -m 0644 "$REPO/deploy/live/sail-live-update.service" "$HOME/.config/systemd/user/"
install -m 0644 "$REPO/deploy/live/sail-live-update.timer" "$HOME/.config/systemd/user/"
if [[ ! -f "$HOME/.config/sail-live/environment" ]]; then
  printf 'SAIL_LIVE_ROOT=%s\nSAIL_LIVE_REMOTE=https://github.com/PedroVictorCoding/SAIL.git\n' "$ROOT" > "$HOME/.config/sail-live/environment"
  chmod 600 "$HOME/.config/sail-live/environment"
fi
systemctl --user daemon-reload
echo 'Installed but not enabled. Read docs/SAIL_LIVE_UPDATES.md before starting.'
