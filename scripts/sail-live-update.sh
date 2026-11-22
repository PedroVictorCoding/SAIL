#!/usr/bin/env bash
# Opt-in deployer: build a merged main commit away from the owner's checkout,
# then atomically switch the active release and restart the managed service.
set -euo pipefail

ROOT="${SAIL_LIVE_ROOT:-${HOME}/.local/share/sail-live}"
REMOTE="${SAIL_LIVE_REMOTE:-https://github.com/PedroVictorCoding/SAIL.git}"
SERVICE="${SAIL_LIVE_SERVICE:-sail-live.service}"
ACTION="${1:-check}"

[[ "$ROOT" = /* && "$ROOT" != / ]] || { echo 'SAIL_LIVE_ROOT must be an absolute, non-root path' >&2; exit 2; }
[[ "$ACTION" == check || "$ACTION" == status ]] || { echo 'Usage: sail-live-update.sh [check|status]' >&2; exit 2; }
mkdir -p "$ROOT"
exec 9>"$ROOT/update.lock"
flock -n 9 || { echo 'Another SAIL update is running'; exit 0; }

if [[ "$ACTION" == status ]]; then
  [[ -L "$ROOT/current" ]] && readlink "$ROOT/current" || echo 'No active release'
  exit 0
fi

if [[ ! -d "$ROOT/repository.git" ]]; then
  git clone --bare --filter=blob:none "$REMOTE" "$ROOT/repository.git"
fi

GIT=(git --git-dir="$ROOT/repository.git")
"${GIT[@]}" fetch --no-tags --prune origin +refs/heads/main:refs/remotes/origin/main
SHA="$("${GIT[@]}" rev-parse refs/remotes/origin/main)"
[[ "$SHA" =~ ^[a-f0-9]{40}$ ]] || exit 1
CANDIDATE="$ROOT/releases/$SHA"
PREVIOUS="$(readlink "$ROOT/current" 2>/dev/null || true)"
if [[ "$PREVIOUS" == "$CANDIDATE" ]]; then
  echo "Already deployed $SHA"
  exit 0
fi

mkdir -p "$ROOT/releases"
if [[ ! -d "$CANDIDATE" ]]; then
  "${GIT[@]}" worktree add --detach "$CANDIDATE" "$SHA"
fi

# Never build into the root filesystem by accident; use the SAIL runtime disk.
TARGET="${CARGO_TARGET_DIR:-${SAIL_RUNTIME:-/srv/sail}/cargo-target}"
[[ -d "$TARGET" && -w "$TARGET" ]] || { echo "Cargo target unavailable: $TARGET" >&2; exit 1; }
export CARGO_TARGET_DIR="$TARGET"
(
  cd "$CANDIDATE"
  cargo test -p sail-core --lib --locked
  cargo build --release -p sail-core --locked
)

# Atomic symlink exchange. Failed builds never touch the running service.
ln -sfn "$CANDIDATE" "$ROOT/current.next"
mv -Tf "$ROOT/current.next" "$ROOT/current"
if ! systemctl --user restart "$SERVICE"; then
  echo "Restart failed for $SHA; rolling back" >&2
  if [[ -n "$PREVIOUS" ]]; then
    ln -sfn "$PREVIOUS" "$ROOT/current.next"
    mv -Tf "$ROOT/current.next" "$ROOT/current"
    systemctl --user restart "$SERVICE" || true
  fi
  exit 1
fi

sleep 3
if ! systemctl --user is-active --quiet "$SERVICE"; then
  echo "Service exited after restart for $SHA" >&2
  if [[ -n "$PREVIOUS" ]]; then
    ln -sfn "$PREVIOUS" "$ROOT/current.next"
    mv -Tf "$ROOT/current.next" "$ROOT/current"
    systemctl --user restart "$SERVICE" || true
  fi
  exit 1
fi

# Optional endpoint verification (e.g. https://127.0.0.1:8088/health).
# The URL must be configured by the operator; do not disable TLS verification.
if [[ -n "${SAIL_LIVE_HEALTH_URL:-}" ]]; then
  healthy=false
  for _ in {1..12}; do
    if curl --fail --silent --show-error --max-time 3 "$SAIL_LIVE_HEALTH_URL" >/dev/null 2>&1; then
      healthy=true
      break
    fi
    sleep 5
  done
  if [[ "$healthy" != true ]]; then
    echo "Health check failed for $SHA" >&2
    if [[ -n "$PREVIOUS" ]]; then
      ln -sfn "$PREVIOUS" "$ROOT/current.next"
      mv -Tf "$ROOT/current.next" "$ROOT/current"
      systemctl --user restart "$SERVICE" || true
    fi
    exit 1
  fi
fi
echo "Deployed $SHA"
