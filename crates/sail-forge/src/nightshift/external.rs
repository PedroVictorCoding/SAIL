//! K6: solve an errand through a vendor CLI when the local attempt failed.
//!
//! The caller enforces the per-night external budget (`external_solves_per_night` and the
//! count already used); this function only accepts a `Failed`/`NeedsYou` errand that has
//! not already been sent. The CLI reply is SEARCH/REPLACE text (`edit.rs` format), applied
//! in a fresh Forge worktree and verified through the same tests, gates and review as a
//! local patch.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sail_guardian::sandbox::SandboxPolicy;
use tokio_util::sync::CancellationToken;

use crate::contracts::IssueEnqueueRequest;
use crate::edit::{apply_edit_blocks, parse_edit_blocks};
use crate::nightshift::result::{job_patch, result_from_report};
use crate::nightshift::{Errand, ErrandState, ErrandStore, ExternalSolver, Transition, WorkOrder};
use crate::orchestrator::ForgeOrchestrator;
use crate::worktree::IsolatedWorktree;

/// The work order plus numbered target files sent to a vendor CLI is capped at 24 000
/// characters (owner decision: bounded cost and no giant file uploads).
const MAX_EXTERNAL_PROMPT_CHARS: usize = 24_000;

/// A vendor reply longer than 256 000 characters is rejected before parsing (fixed a
/// priori to bound work on a runaway CLI).
const MAX_EXTERNAL_REPLY_CHARS: usize = 256_000;

/// Timeout for reading one target file from git (same order as git operations).
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// Try one external solve, then verify the patch like a local one. `Err` means the attempt
/// did not produce a verified result; an errand already marked external is never sent twice.
pub async fn external_attempt(
    orchestrator: &ForgeOrchestrator,
    store: &ErrandStore,
    solver: &dyn ExternalSolver,
    repo: &str,
    errand: &Errand,
    cancel: CancellationToken,
) -> Result<(), String> {
    if errand.external_used {
        return Err("this errand already went to an external solver".into());
    }
    let (order, failure) = match &errand.state {
        ErrandState::Failed { order, reason, .. } => (order.clone(), reason.clone()),
        ErrandState::NeedsYou {
            order, question, ..
        } => (order.clone(), question.clone()),
        _ => {
            return Err("only a failed or needs-you errand can go to an external solver".into());
        }
    };
    if cancel.is_cancelled() {
        return Err("the external solve was cancelled".into());
    }
    let repo_root = orchestrator.config().workspace_root.clone();
    let base = errand_base_commit(errand).unwrap_or_else(|| "HEAD".into());
    let prompt = external_prompt(&order, &errand.notes, &failure, &repo_root, &base)?;
    if prompt.chars().count() > MAX_EXTERNAL_PROMPT_CHARS {
        return Err("too large to send".into());
    }
    let system = concat!(
        "You are fixing a small code change that a local model failed overnight. ",
        "Reply only with Aider-style SEARCH/REPLACE blocks: a path line, then ",
        "<<<<<<< SEARCH, the exact existing lines, =======, the replacement lines, ",
        ">>>>>>> REPLACE. Use an empty SEARCH only to create a new file. No prose."
    );
    // Set before sending so a crash cannot make the CLI see the same errand twice.
    store.transition(&errand.id, Utc::now(), Transition::MarkExternalUsed)?;
    let reply = solver
        .solve(system, &prompt, cancel.clone())
        .await
        .map_err(|error| format!("the external solver failed: {error}"))?;
    if cancel.is_cancelled() {
        return Err("the external solve was cancelled".into());
    }
    if reply.chars().count() > MAX_EXTERNAL_REPLY_CHARS {
        return Err("the external reply is too large".into());
    }
    let blocks = parse_edit_blocks(&reply);
    if blocks.is_empty() {
        return Err("the external reply had no SEARCH/REPLACE blocks".into());
    }

    let worktree_id = format!(
        "ns-ext-{}-{}",
        errand.id.as_str().trim_start_matches("e-"),
        uuid::Uuid::new_v4().simple()
    );
    let worktree = IsolatedWorktree::create(
        &worktree_id,
        &repo_root,
        &base,
        Arc::new(SandboxPolicy::new()),
    )
    .map_err(|error| format!("could not create the external worktree: {error}"))?;
    apply_edit_blocks(&worktree, &blocks)
        .map_err(|error| format!("could not apply the external reply: {error}"))?;
    let diff = worktree
        .get_diff()
        .map_err(|error| format!("could not read the external patch: {error}"))?;
    drop(worktree);
    if diff.trim().is_empty() {
        return Err("the external reply produced no changes".into());
    }

    let request = IssueEnqueueRequest {
        repo: repo.to_string(),
        issue_number: external_issue_number(errand.id.as_str()),
        priority: Some("nightly".into()),
        issue_title: Some(order.title.clone()),
        issue_body: Some(order.to_issue_body(&errand.notes)),
        ..Default::default()
    };
    let job_id = orchestrator.enqueue_managed_issue(request)?;
    let report = orchestrator
        .run_job_lifecycle(&job_id, Some(&diff))
        .await
        .map_err(|error| format!("the external patch failed verification: {error}"))?;

    let mut report = report;
    let patch = job_patch(orchestrator, &job_id, &report);
    report.patch_ref = patch.clone();
    let (mut result, outcome, reason, _) = result_from_report(&report, solver.ran_on(), Utc::now());
    result.job_id = job_id;
    store.save_patch(&errand.id, &patch)?;
    store.transition(
        &errand.id,
        Utc::now(),
        Transition::Finished {
            result,
            outcome,
            question_or_reason: reason,
        },
    )?;
    Ok(())
}

fn external_prompt(
    order: &WorkOrder,
    notes: &[crate::nightshift::OwnerNote],
    failure: &str,
    repo_root: &Path,
    base: &str,
) -> Result<String, String> {
    let mut prompt = String::new();
    prompt.push_str("Work order:\n");
    prompt.push_str(&order.to_issue_body(notes));
    prompt.push_str("\n\nLast local attempt failed:\n");
    prompt.push_str(failure.trim());
    prompt.push_str("\n\nTarget files (with line numbers):\n");
    for (index, path) in order.target_files.iter().enumerate() {
        let numbered = numbered_file_at(repo_root, base, path, MAX_EXTERNAL_PROMPT_CHARS)?;
        let block = format!("--- {}. {path} ---\n{numbered}\n", index + 1);
        if prompt.chars().count() + block.chars().count() > MAX_EXTERNAL_PROMPT_CHARS {
            return Err("too large to send".into());
        }
        prompt.push_str(&block);
    }
    Ok(prompt)
}

fn numbered_file_at(
    repo_root: &Path,
    base: &str,
    path: &str,
    budget: usize,
) -> Result<String, String> {
    let spec = format!("{base}:{path}");
    let output = crate::github::run_with_timeout(
        Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["show", &spec]),
        GIT_TIMEOUT,
    )
    .map_err(|_| format!("target file {path} is not available at {base}"))?;
    let mut out = String::new();
    for (line, text) in output.lines().enumerate() {
        out.push_str(&format!("{:5}: {text}\n", line + 1));
        if out.chars().count() > budget {
            return Err("too large to send".into());
        }
    }
    Ok(out)
}

fn errand_base_commit(errand: &Errand) -> Option<String> {
    match &errand.state {
        ErrandState::Failed {
            result: Some(result),
            ..
        }
        | ErrandState::NeedsYou { result, .. } => {
            Some(result.base_commit.clone()).filter(|base| !base.trim().is_empty())
        }
        _ => None,
    }
}

fn external_issue_number(id: &str) -> u64 {
    u64::from_str_radix(id.trim_start_matches("e-"), 16)
        .map(|number| number | (1 << 62))
        .unwrap_or(1 << 62)
}
