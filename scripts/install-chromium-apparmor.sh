#!/usr/bin/env bash
# Let SAIL's pinned Chromium use its own sandbox on hosts that restrict unprivileged
# user namespaces (Ubuntu 23.10+). Run as root:
#
#   sudo SAIL_CHROMIUM=/path/to/chrome scripts/install-chromium-apparmor.sh
#
# Signed-in browser profiles refuse to run without Chromium's sandbox, so this is
# required for `sail_aistudio` on such hosts.
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
  echo "run as root (sudo)" >&2
  exit 1
fi
if [[ -z "${SAIL_CHROMIUM:-}" ]]; then
  echo "set SAIL_CHROMIUM to the pinned Chromium binary" >&2
  exit 1
fi
binary=$(realpath -e -- "$SAIL_CHROMIUM")
if [[ ! -x "$binary" ]]; then
  echo "$binary is not executable" >&2
  exit 1
fi
case "$binary" in
  *[[:space:]\"]*) echo "the Chromium path must not contain spaces or quotes: $binary" >&2; exit 1 ;;
esac

here=$(dirname -- "$(realpath -e -- "${BASH_SOURCE[0]}")")
template="$here/../deploy/apparmor/sail-chromium"
target=/etc/apparmor.d/sail-chromium

sed "s|@CHROMIUM@|$binary|" "$template" > "$target.tmp"
mv "$target.tmp" "$target"
apparmor_parser -r "$target"
echo "installed $target for $binary"
