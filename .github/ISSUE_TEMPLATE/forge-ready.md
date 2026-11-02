---
name: Forge-ready task
about: A small, test-verifiable task that SAIL Forge may solve autonomously
labels: forge-ready
---

<!--
Forge selects only issues labelled `forge-ready` that contain an `## Acceptance`
checklist. Keep the scope to one crate and at most 3 files. Patches touching files
outside `## Target files` (tests excepted), `.github/`, `deploy/`, Guardian or Forge's
own review/gate code are rejected before review.
-->

## Problem
<!-- What is wrong or missing, in 2-5 sentences. Link the parent epic if any. -->

## Target files
<!-- One bullet per file or directory (trailing `/`), repo-relative. -->
- `crates/<crate>/src/<file>.rs`

## Acceptance
<!-- Each item must be verifiable by `cargo test` or by inspecting the diff. -->
- [ ] 
- [ ] A test covers the new behavior

## Hints
<!-- Optional: functions to change, existing helpers to reuse, pitfalls. -->
