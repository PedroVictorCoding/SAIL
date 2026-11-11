# Public source, private local state

The repository publishes code, sanitized configuration examples and reviewed documentation images. `.gitignore` keeps common credentials, TLS keys, signing stores, environment files, personal databases, models and runtime logs out of new commits. Ignoring a file does not delete it locally.

Run `python3 scripts/check_public_files.py` before committing. CI runs the same check over tracked files. It detects prohibited paths, private-key material and several common token formats; it is a guardrail, not an exhaustive secret audit. Review diffs and images manually too. Never put a real credential in a template.

## Already tracked files

Ignore rules do not affect files Git already tracks. To retain a file locally but remove it from the next commit, run `git rm --cached -- path/to/file` in your task worktree, add a suitable ignore rule and review the staged deletion. Keep a secure local backup first: pulling that deletion into another checkout can remove its tracked copy. Do not run this against source files or public fixtures indiscriminately.

## Previous exposure

Deleting a file in a new commit does not remove it from old commits, pull requests, forks or clones. If a credential was published, revoke/rotate it first. Historical removal requires a separately coordinated history rewrite; this change does not rewrite history or alter existing credentials.

## Screenshots

Use isolated browser contexts with synthetic fixtures, never the owner's authenticated session. Inspect every image before publishing for tokens, addresses, account details, private documents and personal metrics. The README captures are generated from `scripts/ui/library_fill_browser_test.py` and do not access personal services.
