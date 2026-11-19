---
trigger: always_on
description: Navigate SAIL code through the graphify knowledge graph in graphify-out/ before searching or reading files.
---

## graphify

This repository has a graphify code knowledge graph in `graphify-out/` (git-ignored, AST-only,
built locally). Navigate code through it instead of grepping or reading the whole tree:

- In a new worktree, run `scripts/graphify-refresh.sh` once (seeded; about 15 seconds).
- Before the first grep, glob, search, or source-file read for a codebase question, run
  `graphify query "<question>"` (add `--seed <file or symbol>` to start from a known place).
  Use `graphify explain "<symbol>"` for definition, callers, and callees, and
  `graphify path "<A>" "<B>"` for how two symbols connect.
- Read raw files only for the line ranges the graph located; pass this rule to subagents.
- Read `graphify-out/GRAPH_REPORT.md` only for broad architecture review.
- After changing code, run `scripts/graphify-refresh.sh` again.
- If graphify is unavailable, fall back to normal search.

Full rule: AGENTS.md, section "Code navigation with graphify".
