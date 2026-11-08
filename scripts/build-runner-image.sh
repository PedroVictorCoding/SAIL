#!/usr/bin/env bash
# Build the minimal SAIL runner root image for the PDF text operation: pdfinfo, pdftotext,
# their shared-library closure, the dynamic loader and Poppler/fontconfig data. Nothing
# else: no shell, no package manager, no network tools, no credentials.
#
#   scripts/build-runner-image.sh <output-dir>
#
# The output is a plain directory tree. scripts/setup-sail-runner.sh builds it as root,
# makes it root-owned and read-only, adds the marker and swaps it into place; running this
# script alone (as any user) is only for inspection and tests.
set -euo pipefail

out="${1:?usage: build-runner-image.sh <output-dir>}"
[[ -e "$out" ]] && { echo "refusing to reuse existing $out" >&2; exit 1; }

bins=(/usr/bin/pdfinfo /usr/bin/pdftotext)
for b in "${bins[@]}"; do
  [[ -x "$b" ]] || { echo "missing $b (install poppler-utils)" >&2; exit 1; }
done

mkdir -p "$out"
copy() {
  # Copy a host file to the same path in the image, resolving symlinks to real files.
  local src="$1"
  local dest="$out$src"
  mkdir -p "$(dirname "$dest")"
  cp -L --preserve=mode "$src" "$dest"
}

declare -A seen=()
for b in "${bins[@]}"; do
  copy "$b"
  # ldd lists "name => /path (addr)" and the loader as "/lib64/ld-linux… (addr)".
  while read -r lib; do
    [[ -n "$lib" && -z "${seen[$lib]:-}" ]] || continue
    seen[$lib]=1
    copy "$lib"
  done < <(ldd "$b" | awk '$2 == "=>" && $3 ~ /^\// {print $3} $1 ~ /^\// {print $1}')
done

# Poppler's encoding/CMap data and a minimal fontconfig setup. Text extraction works
# without fonts, but fontconfig warns loudly when its configuration is missing.
for data in /usr/share/poppler /etc/fonts /usr/share/fonts/truetype/dejavu; do
  if [[ -d "$data" ]]; then
    mkdir -p "$out$(dirname "$data")"
    cp -rL --preserve=mode "$data" "$out$data"
  fi
done

# Mount points bubblewrap needs inside a read-only root, and the PDF job's working root.
mkdir -p "$out/proc" "$out/dev" "$out/tmp" "$out/work"
# Some loaders look here; keep the merged-/usr layout of the host.
[[ -L /lib64 && ! -e "$out/lib64" ]] && ln -s usr/lib64 "$out/lib64" || true
[[ -L /lib && ! -e "$out/lib" ]] && ln -s usr/lib "$out/lib" || true
[[ -L /bin && ! -e "$out/bin" ]] && ln -s usr/bin "$out/bin" || true
echo "built $out ($(du -sh "$out" | cut -f1))"
