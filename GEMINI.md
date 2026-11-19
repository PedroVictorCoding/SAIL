# SAIL agent instructions

Read and follow [AGENTS.md](AGENTS.md) before changing this repository.
Every task must use a fresh task branch in its own git worktree and be delivered
through a GitHub pull request. Never switch branches, commit, or edit files in
the checkout you were opened in: it stays on the default branch. Never commit or
push directly to the default branch. Preserve unrelated work and do not merge
the PR without an explicit user request.

## Code navigation

Use the graphify code graph before grepping or reading source: run
`scripts/graphify-refresh.sh` once per worktree, then `graphify query "<question>"`,
`graphify explain "<symbol>"`, or `graphify path "<A>" "<B>"`, and read only the lines it
locates. Details: [AGENTS.md](AGENTS.md), section "Code navigation with graphify".
