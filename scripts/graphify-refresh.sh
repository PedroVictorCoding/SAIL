#!/usr/bin/env bash
# Build or refresh the graphify code graph of a worktree (default: the current one).
#
# The graph lives in <worktree>/graphify-out/ (git-ignored). A cold build of SAIL takes
# about four minutes, so the last built graph of this repository is kept as a shared seed
# under $SAIL_GRAPHIFY_SEED_ROOT/<first 12 hex of the root commit> (default
# /srv/sail-data/graphify/seeds, on the HDD). A new worktree copies the seed and only
# re-extracts changed files (about 15 seconds). SAIL Forge uses the same seed layout
# (crates/sail-forge/src/graph.rs).
#
# The build is AST-only: GRAPHIFY_BACKEND=none disables every LLM backend, so nothing
# leaves the host and no API key is used.
#
# Usage: scripts/graphify-refresh.sh [worktree]
set -euo pipefail

root="$(git -C "${1:-.}" rev-parse --show-toplevel)"
cd "$root"

if ! command -v graphify >/dev/null 2>&1; then
  echo "graphify is not installed; see AGENTS.md (Code navigation with graphify)" >&2
  exit 1
fi

seed_root="${SAIL_GRAPHIFY_SEED_ROOT:-/srv/sail-data/graphify/seeds}"
key="$(git rev-list --max-parents=0 HEAD | sort | head -n1 | cut -c1-12)"
seed="$seed_root/${key:-unknown}"

export GRAPHIFY_BACKEND=none GRAPHIFY_QUERY_LOG_DISABLE=1

if [[ ! -f graphify-out/graph.json && -f "$seed/graph.json" ]]; then
  echo "Seeding graphify-out/ from $seed"
  mkdir -p graphify-out
  cp -a "$seed/." graphify-out/
fi

# A plain update re-extracts only changed files (seconds). --force rebuilds everything
# (minutes), so it is only the retry when graphify refuses to shrink a seeded graph.
if ! graphify update .; then
  echo "graphify update refused; retrying with --force" >&2
  graphify update . --force
fi

# Publish this graph as the new seed (atomic swap; graph.html is not needed).
mkdir -p "$seed_root"
tmp="$(mktemp -d "$seed_root/.seed-tmp.XXXXXX")"
cp -a graphify-out/. "$tmp/"
rm -f "$tmp/graph.html"
old="$seed_root/.seed-old.$$"
[[ -d "$seed" ]] && mv "$seed" "$old"
if ! mv "$tmp" "$seed"; then
  rm -rf "$tmp"
fi
rm -rf "$old"
echo "Graph ready: $root/graphify-out/graph.json (seed $seed)"
