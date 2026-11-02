#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  bash scripts/local-issue-agent.sh <issue-number>

This helper is intentionally approval-gated:
- creates a task branch for the issue
- fetches issue context from GitHub
- identifies likely source/docs files using repository search
- runs validation checks only after the working branch is ready
- produces a draft PR body and review checklist
- never creates or merges a PR automatically

Environment variables:
  GH_REPO            GitHub repo to target (default: auto-detected or PedroVictorCoding/SAIL)
  DEFAULT_BRANCH     Base branch to branch from (default: repo default branch)
  SKIP_VALIDATION    Set to 1 to skip cargo validation
  ISSUE_APPROVAL     If set to 0, the script stops before any PR draft is emitted

Examples:
  bash scripts/local-issue-agent.sh 123
  GH_REPO=PedroVictorCoding/SAIL DEFAULT_BRANCH=main bash scripts/local-issue-agent.sh 42
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" || "${1:-}" == "" ]]; then
  usage
  exit 0
fi

issue_number="${1}"
repo_name="${GH_REPO:-$(gh repo view --json nameWithOwner --jq '.nameWithOwner' 2>/dev/null || echo PedroVictorCoding/SAIL)}"
base_branch="${DEFAULT_BRANCH:-$(gh repo view --json defaultBranchRef --jq '.defaultBranchRef.name' 2>/dev/null || echo main)}"

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "Missing required tool: $1" >&2
    exit 1
  }
}

slugify() {
  echo "$1" | tr '[:upper:]' '[:lower:]' | sed -E 's/[^a-z0-9]+/-/g; s/^-+|-+$//g; s/-+/-/g' | cut -c1-40
}

require_cmd gh
require_cmd jq
require_cmd rg
require_cmd git

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$repo_root"

issue_json="$(gh issue view "$issue_number" --repo "$repo_name" --json number,title,body,labels,url,author,assignees)"
issue_title="$(printf '%s' "$issue_json" | jq -r '.title // ""')"
issue_body="$(printf '%s' "$issue_json" | jq -r '.body // ""')"
issue_url="$(printf '%s' "$issue_json" | jq -r '.url // ""')"
issue_labels="$(printf '%s' "$issue_json" | jq -r '.labels[]?.name' | paste -sd ', ' -)"
issue_slug="$(slugify "$issue_title")"
branch_name="agent/issue-${issue_number}-${issue_slug}"

mkdir -p .agent
plan_path=".agent/issue-${issue_number}-plan.md"
pr_body_path=".agent/issue-${issue_number}-pr-body.md"

if git rev-parse --verify "refs/heads/${branch_name}" >/dev/null 2>&1; then
  echo "Branch ${branch_name} already exists; checking it out."
  git switch "$branch_name"
else
  echo "Creating branch ${branch_name} from ${base_branch}"
  git fetch origin "${base_branch}" --quiet || true
  if git rev-parse --verify "origin/${base_branch}" >/dev/null 2>&1; then
    git switch -C "$branch_name" "origin/${base_branch}"
  else
    git switch -C "$branch_name" "$base_branch"
  fi
fi

trimmed_title="$(printf '%s' "$issue_title" | tr -d '\r')"
trimmed_body="$(printf '%s' "$issue_body" | tr -d '\r')"
keywords="$(printf '%s\n%s\n' "$trimmed_title" "$trimmed_body" | \
  tr '[:upper:]' '[:lower:]' | \
  tr -cs 'a-z0-9' '\n' | \
  awk 'NF { print }' | \
  sort -u | \
  grep -vE '^(the|and|for|with|from|into|that|this|issue|bug|feature|fix|error|problem|saill|sail)$' | \
  head -n 25)

relevant_file_list="$(mktemp)"
if [[ -n "$keywords" ]]; then
  while IFS= read -r key; do
    [[ -n "$key" ]] || continue
    [[ ${#key} -lt 3 ]] && continue
    rg -l -i --glob '!target/**' --glob '!**/.venv*/**' --glob '!**/.git/**' --glob '!**/assets/generated/**' "$key" README.md docs crates scripts .github 2>/dev/null >> "$relevant_file_list" || true
  done <<< "$keywords"
else
  printf '%s\n' README.md docs crates scripts .github > "$relevant_file_list"
fi

sorted_files="$(sort -u "$relevant_file_list" | head -n 40)"

cat > "$plan_path" <<EOF
# Local issue plan for #${issue_number}

## Issue
- Title: ${issue_title}
- Labels: ${issue_labels:-none}
- URL: ${issue_url}
- Base branch: ${base_branch}
- Working branch: ${branch_name}

## Likely scope
$(printf '%s\n' "$sorted_files" | sed 's/^/- /' || echo '- no obvious file matches; read the issue, README, and relevant crates manually')

## Owner review checklist
1. Confirm the issue is in scope for the current branch.
2. Verify that the patch matches the issue description and does not broaden the behavior.
3. Check whether the fix belongs in a specific crate or in a design doc.
4. Review the validation commands before merge.
5. Approve or reject the PR only after the diff is understood.

## Suggested first reads
- README.md
- docs/README.md
- Cargo.toml
- crates/ that match the issue keywords
- the specific docs/SAIL_*.md file for the affected subsystem

## Validation commands
```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --workspace --locked
```

## PR draft plan
- Start with the root cause and affected subsystem.
- Keep the patch focused on the issue.
- Add or update tests when behavior is changed.
- Include a validation summary in the PR body.
- Leave merge to the human approver.
EOF

cat > "$pr_body_path" <<EOF
## Change

Describe the concrete problem and resulting behavior for issue #${issue_number}.

- Issue: ${issue_title}
- URL: ${issue_url}
- Branch: ${branch_name}
- Validation is intentionally kept to the smallest relevant cargo checks.

## Validation

- [ ] cargo fmt --all -- --check
- [ ] cargo check --workspace --locked
- [ ] cargo test --workspace --locked
- [ ] Review the diff for unrelated changes before approval

## Review notes

- This workflow is approval-gated and does not merge automatically.
- Keep the patch scoped to the issue and avoid unrelated refactors.
- If validation fails, stop and ask for human review before proceeding.
EOF

if [[ "${SKIP_VALIDATION:-0}" != "1" ]]; then
  echo "Running validation checks on the working branch..."
  cargo fmt --all -- --check
  cargo check --workspace --locked
  cargo test --workspace --locked
else
  echo "Validation skipped by SKIP_VALIDATION=1"
fi

cat <<EOF
Local issue workflow prepared successfully.

Issue: #${issue_number}
Title: ${issue_title}
Branch: ${branch_name}
Plan: ${plan_path}
Draft PR body: ${pr_body_path}

Next steps:
1. Review the plan and relevant files listed in ${plan_path}.
2. Implement the minimal fix in the affected crate(s).
3. Re-run the validation commands if you changed Rust code.
4. Create a draft PR only when ready:
   gh pr create --draft --base ${base_branch} --head ${branch_name} --fill

Safety rules:
- No direct push to ${base_branch}
- No merge without explicit human approval
- No unrelated file changes
- Keep the patch scoped to the issue
EOF

if [[ "${ISSUE_APPROVAL:-1}" == "0" ]]; then
  echo "ISSUE_APPROVAL=0: halted before PR creation as requested."
  exit 0
fi

exit 0
