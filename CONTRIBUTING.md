# Contributing to SAIL

## AI coding agents

All LLMs and coding agents working on this project must follow
[AGENTS.md](AGENTS.md). The required delivery path is:

**New task branch → scoped changes → validation → commit → push branch → GitHub PR.**

Agents must not commit or push directly to the default branch, including for small
fixes and documentation. Keep unrelated local work out of the PR. Continue the
same branch for follow-ups to its active task and use a fresh branch for a new task.

Open the PR against the repository's default branch, normally `main`. Its description
should explain the problem, the resulting behavior, and actual validation results.
An agent must not merge or enable auto-merge without an explicit user request.

The repository includes instruction entry points for Codex/AGENTS-compatible
clients, Claude, Gemini, GitHub Copilot, and Cursor. Other agent clients must also
read `AGENTS.md` and follow the same workflow.

## Review and enforcement

Repository instructions govern agent behavior; they are not a substitute for
GitHub branch protection. Review the proposed changes before merging. Agents must
respect any configured GitHub rules and report authentication or permission
blockers rather than bypassing the pull-request workflow.
