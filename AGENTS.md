# Instructions for AI coding agents

These instructions apply to every LLM, coding assistant, autonomous agent, and
agent-assisted tool working anywhere in this repository, including documentation,
configuration, tests, and scripts. Read this file before making changes. More
specific instructions may add requirements but must retain this branch/PR workflow.

## Mandatory SAIL product vision

Before planning or editing any feature, read [SAIL Master Vision](docs/SAIL_MASTER_VISION.md). It defines the authoritative intended product behavior, subsystem boundaries, memory philosophy, real-action verification, and local-first constraints from the SAIL design conversation. The code defines current implementation; do not mistake it for the whole product scope. If they conflict, document the intended migration rather than silently replacing the vision.

## UI design requirements

Before creating or changing UI, read and follow [DESIGN.md](DESIGN.md). Its shared
ambient glass, living light, accessibility, and verification rules are acceptance criteria for
UI changes by every coding assistant. Apply them within the requested scope.

## Mandatory branch and pull-request workflow

1. **Never change the workspace you were opened in.** The owner keeps their
   checkout (and editor) on the default branch at all times. In that checkout do
   not run `git checkout`/`git switch` to another branch, create commits, stash,
   reset, rebase, or edit tracked files. Read-only commands (`status`, `log`,
   `diff`, `fetch`, `worktree add/list`) are fine. This applies even when the
   checkout happens to be clean and even for one-line or documentation changes.
2. **Do every task in its own worktree on a fresh branch.** Fetch the remote,
   then create the branch and worktree in one step from the latest default
   branch, outside the owner's checkout:

   ```sh
   git fetch origin
   git worktree add -b agent/<context-driven-name> \
     /home/main/sail-worktrees/<context-driven-name> origin/main
   ```

   Run all edits, builds, tests, commits, and pushes from that worktree. Use
   `agent/<context-driven-name>` for every agent task branch, for example
   `agent/mind-local-inference-routing` or `agent/update-usage-guide`. Continue
   the same branch and worktree for follow-ups to that active task; do not reuse
   a branch from an unrelated, completed, or merged task. Inspect remotes and
   existing work first, and preserve work belonging to other tasks: never
   silently include, stash, reset, or discard it. When the task's PR is merged
   and the worktree is clean, remove it with `git worktree remove`.
3. **Never commit or push directly to the default branch.** Do not develop on
   `main`, `master`, or a protected/release branch. If a session starts there,
   create the task worktree (step 2) before changing files. This requirement also applies
   to small fixes, documentation-only changes, and instructions to “commit and
   push”: push the task branch and open/update its pull request.
4. **Keep the commit scoped.** Stage explicit task files, review the staged diff,
   and keep unrelated changes out of the commit and PR. Do not commit model
   weights, virtual environments, runtime logs, TLS private keys, or credentials.
5. **Validate the change.** Run checks appropriate to the affected behavior.
   Documentation-only changes need link/content checks, not the entire runtime
   test suite. Describe actual results and any checks that could not run.
6. **Commit and push the task branch.** Set an upstream for the new branch. Do not
   force-push shared history or bypass repository protections. If the remote base
   advances, integrate it on the task branch while preserving unrelated work.
7. **Open a GitHub pull request.** Target the repository's default branch unless
   the user explicitly identifies another integration branch. Use the available
   GitHub integration or authenticated `gh` CLI. Update an existing PR for the
   same active task instead of creating duplicates. A local commit or pushed
   branch alone does not complete delivery.
8. **Leave merging to the user/reviewer.** Do not merge the PR, enable auto-merge,
   or delete branches unless explicitly asked to do so.
9. **Report the reviewable result.** Include the task branch, commit, PR URL, and
   validation results. State if unrelated work remains uncommitted. If GitHub
   authentication, permissions, or another external requirement blocks delivery,
   report the exact blocker; never fall back to pushing the default branch or
   claim a PR exists when it does not.

## Pull-request description

Explain the concrete problem and resulting behavior, summarize the final change,
and list the relevant validation. Include material limitations or risks. Keep the
PR focused on the task and avoid conversational history or claims of checks that
were not performed.

## Code navigation with graphify

This repository is indexed by [graphify](https://github.com/Graphify-Labs/graphify) as a code
knowledge graph in `graphify-out/`: files, symbols, and their call/use/contain edges. The build
is AST-only (`GRAPHIFY_BACKEND=none`), so no model runs and nothing leaves the host. Every agent
(Claude Code, Codex, Antigravity, Kilo, and SAIL Forge's local model) navigates code through the
graph instead of grepping or reading the whole tree.

1. **Build it once per worktree.** Run `scripts/graphify-refresh.sh` in a new worktree. It copies
   this repository's shared seed graph from `/srv/sail-data/graphify/seeds/` and re-extracts only
   changed files (about 15 seconds; about 4 minutes only when no seed exists yet).
   `graphify-out/` is git-ignored; never commit it.
2. **Query before you search.** Before the first grep, glob, search, or source-file read for a
   codebase question, run `graphify query "<question>"`. Add `--seed <file path or symbol>` to start
   from a known place, and `--budget <tokens>` to widen or narrow the answer (default 2000). Use
   `graphify explain "<symbol>"` for a symbol's definition, callers, and callees;
   `graphify path "<A>" "<B>"` for how two symbols connect; and `graphify section "<heading>"` for
   one Markdown section without opening the file.
3. **Read only what the graph located.** Open raw files only for the line ranges that the graph or
   an earlier read pointed to. Put this rule in every subagent prompt that explores code.
4. Read `graphify-out/GRAPH_REPORT.md` only for broad architecture reviews, or when query, path,
   and explain do not surface enough context.
5. **Keep it current.** After changing code, run `scripts/graphify-refresh.sh` (or
   `graphify update .`) so later queries see your edits.
6. If `graphify` is not installed or cannot answer, fall back to normal search.

Client entry points that carry this rule: `CLAUDE.md`, `.claude/settings.json` hooks and
`.claude/skills/graphify/` (Claude Code); this file and `.codex/` (Codex); `GEMINI.md` and
`.agents/rules/`, `.agents/workflows/`, `.agents/skills/` (Antigravity); this file and
`.kilocode/rules/graphify.md` (Kilo); `crates/sail-forge/src/graph.rs` (SAIL Forge).

## Instructions for other agent clients

`CLAUDE.md`, `GEMINI.md`, `.github/copilot-instructions.md`, and
`.cursor/rules/agent-workflow.mdc` point to this policy; `.agents/rules/graphify.md` and
`.kilocode/rules/graphify.md` repeat the graphify rule above. `AGENTS.md` is the shared
source of truth; keep those entry points consistent when changing this workflow.
These files define agent behavior. Server-side branch protections, if separately
configured, remain additional requirements and must not be bypassed.
