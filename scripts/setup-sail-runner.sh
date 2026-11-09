#!/usr/bin/env bash
# Install the SAIL sandbox runner so SAIL can read PDFs (Moodle files, Course Factory
# sources) without parsing them in its own process.
#
#   cargo build --release -p sail-runner
#   sudo scripts/setup-sail-runner.sh --user <daemon-user> [--binary PATH] [--dry-run]
#
# What it does (idempotent):
# - installs poppler-utils and bubblewrap if missing;
# - creates the `sail` group and the `sail-runner` system user (no login, no home), and
#   adds <daemon-user> to `sail` so SAIL can reach the runner socket;
# - builds the runner root image (scripts/build-runner-image.sh: pdfinfo, pdftotext and
#   their libraries only; no shell) at /srv/sail-runner/images/approved-rootfs, root-owned
#   and read-only, with the image marker, swapping it in atomically;
# - installs the runner binary to /usr/local/libexec/sail-runner;
# - writes /etc/sail/runner.env, the systemd socket/service and the tmpfiles rule from
#   deploy/runner, then enables the socket;
# - runs a self-test as `sail-runner` inside the sandbox.
#
# It never adds `sail-runner` to `sail-data`: sandboxed work can't touch SAIL's data.
# Afterwards, start SAIL from a new login shell (for the group) with scripts/run.sh, which
# picks up the socket automatically.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/.." && pwd)"
RUNNER_USER=sail-runner
GROUP=sail
IMAGES=/srv/sail-runner/images
ROOTFS="$IMAGES/approved-rootfs"
MARKER_TEXT="SAIL isolated root image v1"
DAEMON_USER="${SUDO_USER:-}"
# `cargo build` writes to ~/.cargo/config.toml's target-dir (the data disk), and run.sh
# builds on the runtime SSD. Under sudo neither setting is visible, so take the first
# built runner found.
BINARY=""
for dir in "${CARGO_TARGET_DIR:-}" /srv/sail-data/cargo-target /srv/sail/cargo-target; do
  if [[ -n "$dir" && -x "$dir/release/sail-runner" ]]; then
    BINARY="$dir/release/sail-runner"
    break
  fi
done
BINARY="${BINARY:-/srv/sail-data/cargo-target/release/sail-runner}"
DRY_RUN=0

usage() {
  sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'
  echo "Options: --user USER  --binary PATH  --dry-run"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --user) DAEMON_USER="${2:?}"; shift 2 ;;
    --binary) BINARY="${2:?}"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

run() {
  if [[ $DRY_RUN -eq 1 ]]; then echo "+ $*"; else "$@"; fi
}

[[ $EUID -eq 0 || $DRY_RUN -eq 1 ]] || { echo "run with sudo" >&2; exit 1; }
[[ -n "$DAEMON_USER" ]] || { echo "--user <daemon-user> is required" >&2; exit 2; }
id "$DAEMON_USER" >/dev/null 2>&1 || { echo "no such user: $DAEMON_USER" >&2; exit 1; }
[[ "$DAEMON_USER" != "$RUNNER_USER" && "$DAEMON_USER" != root ]] || {
  echo "the daemon user must be a normal account, not $DAEMON_USER" >&2; exit 1; }
[[ -x "$BINARY" ]] || { echo "runner binary not found: $BINARY (cargo build --release -p sail-runner)" >&2; exit 1; }

echo "== packages"
missing=()
command -v pdftotext >/dev/null || missing+=(poppler-utils)
[[ -x /usr/bin/bwrap ]] || missing+=(bubblewrap)
if [[ ${#missing[@]} -gt 0 ]]; then
  run apt-get install -y --no-install-recommends "${missing[@]}"
fi

echo "== users and groups"
getent group "$GROUP" >/dev/null || run groupadd --system "$GROUP"
if ! id "$RUNNER_USER" >/dev/null 2>&1; then
  run useradd --system --gid "$GROUP" --no-create-home --home-dir /nonexistent \
    --shell /usr/sbin/nologin "$RUNNER_USER"
fi
if id -nG "$RUNNER_USER" 2>/dev/null | tr ' ' '\n' | grep -qx sail-data; then
  echo "refusing: $RUNNER_USER is in sail-data; remove it first" >&2; exit 1
fi
id -nG "$DAEMON_USER" | tr ' ' '\n' | grep -qx "$GROUP" || run usermod -aG "$GROUP" "$DAEMON_USER"

echo "== root image"
run install -d -o root -g root -m 0755 /srv/sail-runner "$IMAGES"
staging="$IMAGES/.staging-$$"
run "$SCRIPT_DIR/build-runner-image.sh" "$staging"
if [[ $DRY_RUN -eq 0 ]]; then
  printf '%s\n' "$MARKER_TEXT" > "$staging/.sail-runner-image-v1"
  chown -R root:root "$staging"
  chmod -R go-w "$staging"
  chmod 0644 "$staging/.sail-runner-image-v1"
  chmod 0755 "$staging"
  if [[ -d "$ROOTFS" ]]; then
    mv "$ROOTFS" "$IMAGES/.previous-$$"
  fi
  mv "$staging" "$ROOTFS"
  rm -rf "$IMAGES/.previous-$$"
fi

echo "== binary, config and units"
run install -o root -g root -m 0755 -D "$BINARY" /usr/local/libexec/sail-runner
run install -d -o root -g root -m 0755 /etc/sail
daemon_uid="$(id -u "$DAEMON_USER")"
if [[ $DRY_RUN -eq 1 ]]; then
  echo "+ write /etc/sail/runner.env (daemon uid $daemon_uid, rootfs $ROOTFS)"
else
  umask 022
  cat > /etc/sail/runner.env <<ENV
# Written by scripts/setup-sail-runner.sh. No credentials.
SAIL_RUNNER_DAEMON_UID=$daemon_uid
SAIL_RUNNER_ROOTFS=$ROOTFS
ENV
fi
run install -o root -g root -m 0644 "$REPO/deploy/runner/sail-runner.socket" /etc/systemd/system/sail-runner.socket
run install -o root -g root -m 0644 "$REPO/deploy/runner/sail-runner.service" /etc/systemd/system/sail-runner.service
run install -o root -g root -m 0644 "$REPO/deploy/runner/sail-runner.tmpfiles.conf" /etc/tmpfiles.d/sail-runner.conf
run systemd-tmpfiles --create /etc/tmpfiles.d/sail-runner.conf
run systemctl daemon-reload
run systemctl enable --now sail-runner.socket
# A running service keeps the old binary/image; restart it so the new ones are used.
run systemctl try-restart sail-runner.service

echo "== self-test (as $RUNNER_USER, inside the sandbox)"
if [[ $DRY_RUN -eq 0 ]]; then
  # The same isolation flags the runner uses; prints Poppler's version from the image.
  runuser -u "$RUNNER_USER" -- /usr/bin/bwrap --unshare-all --unshare-user --disable-userns \
    --die-with-parent --new-session --cap-drop ALL --clearenv --ro-bind "$ROOTFS" / \
    --proc /proc --dev /dev --tmpfs /work --chdir /work -- /usr/bin/pdfinfo -v 2>&1 | head -1
  if runuser -u "$RUNNER_USER" -- /usr/bin/bwrap --unshare-all --unshare-user --disable-userns \
    --ro-bind "$ROOTFS" / --proc /proc --dev /dev -- /bin/sh -c true 2>/dev/null; then
    echo "self-test failed: the image contains a shell" >&2; exit 1
  fi
  [[ -S /run/sail-runner/command.sock ]] || { echo "socket missing: /run/sail-runner/command.sock" >&2; exit 1; }
fi

runner_uid="$(id -u "$RUNNER_USER" 2>/dev/null || echo '<uid>')"
cat <<DONE

Runner installed. SAIL needs these when it starts (scripts/run.sh sets them when the
socket exists):

  SAIL_RUNNER_SOCKET=/run/sail-runner/command.sock
  SAIL_RUNNER_UID=$runner_uid

$DAEMON_USER was added to '$GROUP'. Open a new login shell (or log out and in) before
restarting SAIL, so the group applies. Then allow PDF reading once on SAIL's School page.
DONE
