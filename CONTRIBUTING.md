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

## Required checks and branch protection

Every pull request and every push to `main` runs **Rust CI** (`.github/workflows/rust-ci.yml`).
Its single job, **`Rust Format, Check & Test`**, runs these steps:

- `cargo fmt --check`
- `cargo check --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo nextest run --workspace --profile ci` (every test in its own process; every
  failure reported; JUnit report uploaded as the `junit-report` artifact)
- `cargo test --workspace --doc` (nextest does not run doc tests)

It uses a pinned Rust toolchain. Bump that version deliberately, in a PR that also fixes
any new lints. Nextest and cargo-mutants versions are pinned in the workflow too.

Two more jobs run beside it:

- **`Dependency licenses & advisories`** runs `cargo deny check` against
  [`deny.toml`](deny.toml): permissive licenses only, no RustSec vulnerabilities or yanked
  crates, crates.io as the only source. A new dependency with another license fails until
  the license is added to `deny.toml` with the reason. Make this a required check.
- **`Mutation testing (changed code)`** runs on pull requests only. It runs
  `cargo mutants --in-diff` on the Rust lines the PR changes and lists *missed* mutants:
  changed functions that can be broken without any test failing. The job summary shows the
  counts. It is advisory for now. Make it required once its run time on typical PRs is
  known.

To run the same checks locally: `cargo install --locked cargo-nextest cargo-deny cargo-mutants`,
then `cargo nextest run --workspace`, `cargo deny check`, and
`git diff origin/main... > pr.diff && cargo mutants --in-diff pr.diff --test-tool nextest`.

To make CI a merge gate, a repository admin configures branch protection (or a ruleset)
for `main` under **Settings → Branches**:

1. **Require a pull request before merging.**
2. **Require status checks to pass**, and add `Rust Format, Check & Test` and
   `Dependency licenses & advisories` as required checks.
   Also add `verify` (UI asset integrity) if asset changes should block merging. It only
   runs when asset paths change.
3. **Require branches to be up to date before merging.** This makes a check result describe
   the code that is actually merged.
4. Leave auto-merge and force pushes to `main` disabled.

CI runs PR code with a read-only token and no repository secrets: it uses `pull_request`,
not `pull_request_target`. Keep it that way. Never skip, ignore or quarantine a failing test
to get a green check; fix the cause or report it.
