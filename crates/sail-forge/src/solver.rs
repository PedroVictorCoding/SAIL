use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use futures_util::StreamExt;
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::contracts::TestExecutionRecord;
use crate::edit::{EditBlock, apply_edit_blocks, parse_edit_blocks};
use crate::worktree::{IsolatedWorktree, WorktreeError};

/// Bounded repository tool call emitted by the local coding model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum RepoToolCall {
    #[serde(rename = "repo:list", alias = "list")]
    List {
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename = "repo:search", alias = "search")]
    Search {
        query: String,
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename = "repo:read_range", alias = "read_range")]
    ReadRange {
        path: String,
        #[serde(default = "default_start_line")]
        start_line: usize,
        #[serde(default = "default_end_line")]
        end_line: usize,
    },
    #[serde(rename = "repo:diff", alias = "diff")]
    Diff,
    #[serde(rename = "repo:apply_patch", alias = "apply_patch")]
    ApplyPatch { patch: String },
    /// Single search/replace edit (JSON form of a SEARCH/REPLACE block).
    #[serde(rename = "repo:edit", alias = "edit")]
    Edit {
        path: String,
        #[serde(alias = "old_str", alias = "search")]
        old: String,
        #[serde(alias = "new_str", alias = "replace")]
        new: String,
    },
    /// SEARCH/REPLACE blocks parsed from the raw reply, applied atomically.
    #[serde(rename = "repo:edit_blocks")]
    EditBlocks { blocks: Vec<EditBlock> },
    #[serde(rename = "repo:test", alias = "test")]
    Test {
        #[serde(default)]
        filter: Option<String>,
    },
    #[serde(rename = "done", alias = "repo:done")]
    Done {
        #[serde(default)]
        summary: Option<String>,
    },
}

fn default_start_line() -> usize {
    1
}

fn default_end_line() -> usize {
    200
}

/// Result of executing a bounded repository tool inside an `IsolatedWorktree`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoToolResult {
    pub tool: String,
    pub ok: bool,
    pub output: String,
    #[serde(default)]
    pub modified_files: Vec<String>,
    #[serde(default)]
    pub test_record: Option<TestExecutionRecord>,
}

/// Identified local model metadata from endpoint and/or local GGUF file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentifiedModel {
    pub model_id: String,
    pub model_hash: String,
    pub endpoint: String,
}

/// Outcome of the bounded multi-turn local solver loop.
#[derive(Debug, Clone)]
pub struct SolverOutcome {
    pub success: bool,
    pub verification_status: String,
    pub failure_reason: Option<String>,
    pub final_diff: String,
    pub modified_files: Vec<String>,
    pub target_file: Option<String>,
    pub iterations: u32,
    pub turns: u32,
    pub test_records: Vec<TestExecutionRecord>,
    pub model: IdentifiedModel,
    pub summary: String,
    /// Edit/patch tool calls that failed to apply (malformed or mismatched edits).
    pub edit_failures: u32,
    /// Model replies that contained no parseable tool call or edit.
    pub parse_failures: u32,
}

/// Real-time progress update emitted during autonomous solver iterations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolverProgress {
    pub turn: u32,
    pub max_turns: u32,
    pub iteration: u32,
    pub stage: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<Vec<String>>,
}

impl RepoToolCall {
    fn edit_blocks(&self) -> Option<Vec<EditBlock>> {
        match self {
            RepoToolCall::Edit { path, old, new } => Some(vec![EditBlock {
                path: path.clone(),
                search: old.clone(),
                replace: new.clone(),
            }]),
            RepoToolCall::EditBlocks { blocks } => Some(blocks.clone()),
            _ => None,
        }
    }

    /// Files an edit-type call intends to change.
    pub fn edit_paths(&self) -> Vec<String> {
        match self {
            RepoToolCall::ApplyPatch { patch } => extract_modified_files_from_diff(patch),
            other => {
                let mut paths: Vec<String> = other
                    .edit_blocks()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|b| b.path)
                    .collect();
                paths.dedup();
                paths
            }
        }
    }

    /// Human-readable body of an edit-type call for progress events.
    pub fn edit_preview(&self) -> String {
        match self {
            RepoToolCall::ApplyPatch { patch } => patch.clone(),
            other => other
                .edit_blocks()
                .unwrap_or_default()
                .iter()
                .map(|b| {
                    format!(
                        "{}\n<<<<<<< SEARCH\n{}\n=======\n{}\n>>>>>>> REPLACE",
                        b.path, b.search, b.replace
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// Execute a bounded repository tool strictly inside `worktree`.
pub fn execute_repo_tool(
    worktree: &IsolatedWorktree,
    call: &RepoToolCall,
    log_dir: &Path,
    timeout_secs: u64,
    step_idx: u32,
) -> RepoToolResult {
    if let Some(blocks) = call.edit_blocks() {
        return match apply_edit_blocks(worktree, &blocks) {
            Ok(files) => RepoToolResult {
                tool: "repo:edit".into(),
                ok: true,
                output: format!(
                    "Edit applied ({} block(s)). Modified files: {}",
                    blocks.len(),
                    files.join(", ")
                ),
                modified_files: files,
                test_record: None,
            },
            Err(e) => RepoToolResult {
                tool: "repo:edit".into(),
                ok: false,
                output: format!("Error: {e}"),
                modified_files: Vec::new(),
                test_record: None,
            },
        };
    }
    match call {
        RepoToolCall::Edit { .. } | RepoToolCall::EditBlocks { .. } => {
            unreachable!("edit calls are handled above")
        }
        RepoToolCall::List { path } => {
            let rel = path.as_deref().unwrap_or(".");
            let rel_path = Path::new(rel);
            let target = match worktree.resolve_safe_path(rel_path, false) {
                Ok(p) => p,
                Err(e) => {
                    return RepoToolResult {
                        tool: "repo:list".into(),
                        ok: false,
                        output: format!("Error: {}", e),
                        modified_files: Vec::new(),
                        test_record: None,
                    };
                }
            };

            let mut entries = Vec::new();
            collect_files_bounded(&worktree.root_dir, &target, &mut entries, 200);
            entries.sort();
            RepoToolResult {
                tool: "repo:list".into(),
                ok: true,
                output: if entries.is_empty() {
                    "(no files found)".into()
                } else {
                    entries.join("\n")
                },
                modified_files: Vec::new(),
                test_record: None,
            }
        }
        RepoToolCall::Search { query, path } => {
            if query.trim().is_empty() {
                return RepoToolResult {
                    tool: "repo:search".into(),
                    ok: false,
                    output: "Error: search query must not be empty".into(),
                    modified_files: Vec::new(),
                    test_record: None,
                };
            }
            let rel = path.as_deref().unwrap_or(".");
            let target = match worktree.resolve_safe_path(Path::new(rel), false) {
                Ok(p) => p,
                Err(e) => {
                    return RepoToolResult {
                        tool: "repo:search".into(),
                        ok: false,
                        output: format!("Error: {}", e),
                        modified_files: Vec::new(),
                        test_record: None,
                    };
                }
            };

            let mut files = Vec::new();
            collect_files_bounded(&worktree.root_dir, &target, &mut files, 500);
            files.sort();

            let mut matches = Vec::new();
            let q_lower = query.to_lowercase();
            'outer: for rel_file in files {
                if let Ok(content) = worktree.read_file(Path::new(&rel_file)) {
                    for (idx, line) in content.lines().enumerate() {
                        if line.to_lowercase().contains(&q_lower) {
                            matches.push(format!("{}:{}: {}", rel_file, idx + 1, line));
                            if matches.len() >= 100 {
                                break 'outer;
                            }
                        }
                    }
                }
            }

            RepoToolResult {
                tool: "repo:search".into(),
                ok: true,
                output: if matches.is_empty() {
                    format!("No matches found for '{}'", query)
                } else {
                    matches.join("\n")
                },
                modified_files: Vec::new(),
                test_record: None,
            }
        }
        RepoToolCall::ReadRange {
            path,
            start_line,
            end_line,
        } => {
            let content = match worktree.read_file(Path::new(path)) {
                Ok(c) => c,
                Err(e) => {
                    return RepoToolResult {
                        tool: "repo:read_range".into(),
                        ok: false,
                        output: format!("Error reading '{}': {}", path, e),
                        modified_files: Vec::new(),
                        test_record: None,
                    };
                }
            };

            let lines: Vec<&str> = content.lines().collect();
            let start = (*start_line).max(1);
            let max_span = 200usize;
            let end = (*end_line)
                .max(start)
                .min(start + max_span - 1)
                .min(lines.len().max(1));

            if lines.is_empty() {
                return RepoToolResult {
                    tool: "repo:read_range".into(),
                    ok: true,
                    output: format!("{} (empty file)", path),
                    modified_files: Vec::new(),
                    test_record: None,
                };
            }

            let mut rendered = Vec::new();
            rendered.push(format!(
                "// File: {} (lines {}..={} of {})",
                path,
                start,
                end.min(lines.len()),
                lines.len()
            ));
            for line_no in start..=end.min(lines.len()) {
                rendered.push(format!("{:4}: {}", line_no, lines[line_no - 1]));
            }
            RepoToolResult {
                tool: "repo:read_range".into(),
                ok: true,
                output: rendered.join("\n"),
                modified_files: Vec::new(),
                test_record: None,
            }
        }
        RepoToolCall::Diff => match worktree.get_diff() {
            Ok(diff) => RepoToolResult {
                tool: "repo:diff".into(),
                ok: true,
                output: if diff.trim().is_empty() {
                    "(no working tree changes)".into()
                } else {
                    diff
                },
                modified_files: Vec::new(),
                test_record: None,
            },
            Err(e) => RepoToolResult {
                tool: "repo:diff".into(),
                ok: false,
                output: format!("Error computing diff: {}", e),
                modified_files: Vec::new(),
                test_record: None,
            },
        },
        RepoToolCall::ApplyPatch { patch } => {
            let cleaned = extract_unified_diff(patch);
            if !is_meaningful_code_diff(&cleaned) {
                return RepoToolResult {
                    tool: "repo:apply_patch".into(),
                    ok: false,
                    output: "Error: patch is empty or modifies only comments/whitespace".into(),
                    modified_files: Vec::new(),
                    test_record: None,
                };
            }

            match worktree.apply_patch(&cleaned) {
                Ok(()) => {
                    let current_diff = worktree.get_diff().unwrap_or_default();
                    let files = extract_modified_files_from_diff(&current_diff);
                    RepoToolResult {
                        tool: "repo:apply_patch".into(),
                        ok: true,
                        output: format!(
                            "Patch applied cleanly. Modified files ({}): {}",
                            files.len(),
                            files.join(", ")
                        ),
                        modified_files: files,
                        test_record: None,
                    }
                }
                Err(e) => RepoToolResult {
                    tool: "repo:apply_patch".into(),
                    ok: false,
                    output: format!("Failed to apply patch: {}", e),
                    modified_files: Vec::new(),
                    test_record: None,
                },
            }
        }
        RepoToolCall::Test { filter } => {
            let rec = run_staged_test_command(
                worktree,
                log_dir,
                timeout_secs,
                step_idx,
                filter.as_deref(),
            );
            match rec {
                Ok((record, combined_output)) => RepoToolResult {
                    tool: "repo:test".into(),
                    ok: record.exit_code == 0,
                    output: combined_output,
                    modified_files: Vec::new(),
                    test_record: Some(record),
                },
                Err(e) => RepoToolResult {
                    tool: "repo:test".into(),
                    ok: false,
                    output: format!("Sandbox execution error: {}", e),
                    modified_files: Vec::new(),
                    test_record: None,
                },
            }
        }
        RepoToolCall::Done { summary } => RepoToolResult {
            tool: "done".into(),
            ok: true,
            output: summary.clone().unwrap_or_else(|| "Done".into()),
            modified_files: Vec::new(),
            test_record: None,
        },
    }
}

/// Run a confined `cargo test --offline` inside the isolated worktree and write full stdout/stderr to `log_dir`.
pub fn run_staged_test_command(
    worktree: &IsolatedWorktree,
    log_dir: &Path,
    timeout_secs: u64,
    step_idx: u32,
    filter: Option<&str>,
) -> Result<(TestExecutionRecord, String), WorktreeError> {
    let start = Instant::now();
    let mut args = vec!["test", "--offline"];
    if let Some(f) = filter {
        if !f.trim().is_empty() {
            args.push(f.trim());
        }
    }

    let output = worktree.run_command("cargo", &args, timeout_secs)?;
    let duration_ms = start.elapsed().as_millis() as u64;

    let _ = fs::create_dir_all(log_dir);
    let log_path = log_dir.join(format!("test_step_{}.log", step_idx));
    let full_log = format!(
        "COMMAND: cargo {}\nEXIT_CODE: {}\nDURATION_MS: {}\n--- STDOUT ---\n{}\n--- STDERR ---\n{}\n",
        args.join(" "),
        output.status,
        duration_ms,
        output.stdout,
        output.stderr
    );
    let _ = fs::write(&log_path, &full_log);

    let record = TestExecutionRecord {
        command: format!("cargo {}", args.join(" ")),
        status: if output.status == 0 {
            "passed".into()
        } else {
            format!("failed (exit {})", output.status)
        },
        duration_ms,
        log_ref: log_path.to_string_lossy().to_string(),
        exit_code: output.status,
        stdout_log_path: Some(log_path.clone()),
        stderr_log_path: Some(log_path),
    };

    // Truncate returned tool output to keep model context concise while preserving compiler/test errors
    let mut summary = format!("exit_code: {} ({} ms)\n", output.status, duration_ms);
    if !output.stdout.trim().is_empty() {
        summary.push_str("stdout:\n");
        summary.push_str(&tail_lines(&output.stdout, 60));
        summary.push('\n');
    }
    if !output.stderr.trim().is_empty() {
        summary.push_str("stderr:\n");
        summary.push_str(&tail_lines(&output.stderr, 80));
    }

    Ok((record, summary))
}

fn tail_lines(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        text.to_string()
    } else {
        let skipped = lines.len() - max_lines;
        let mut out = format!("... ({} lines omitted) ...\n", skipped);
        out.push_str(&lines[skipped..].join("\n"));
        out
    }
}

fn collect_files_bounded(root_dir: &Path, curr: &Path, out: &mut Vec<String>, cap: usize) {
    if out.len() >= cap {
        return;
    }
    if curr.is_file() {
        if let Ok(rel) = curr.strip_prefix(root_dir) {
            out.push(rel.to_string_lossy().to_string());
        }
        return;
    }
    let entries = match fs::read_dir(curr) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut sorted: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    sorted.sort();
    for path in sorted {
        if out.len() >= cap {
            break;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name == ".git" || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            collect_files_bounded(root_dir, &path, out, cap);
        } else if path.is_file() {
            if let Ok(rel) = path.strip_prefix(root_dir) {
                out.push(rel.to_string_lossy().to_string());
            }
        }
    }
}

/// Extract modified file paths (`b/<path>`) from a unified diff.
pub fn extract_modified_files_from_diff(diff: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in diff.lines() {
        if let Some(p) = line.strip_prefix("+++ b/") {
            let trimmed = p.trim().to_string();
            if !trimmed.is_empty() && !files.contains(&trimmed) {
                files.push(trimmed);
            }
        }
    }
    files
}

/// Verify that a unified diff has code modifications and is non-empty.
pub fn is_meaningful_code_diff(diff: &str) -> bool {
    let mut has_target_header = false;
    let mut has_code_change = false;
    for line in diff.lines() {
        if line.starts_with("+++ b/") {
            has_target_header = true;
            continue;
        }
        if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
            continue;
        }
        if let Some(changed) = line.strip_prefix('+').or_else(|| line.strip_prefix('-')) {
            let trimmed = changed.trim();
            if !trimmed.is_empty() {
                has_code_change = true;
            }
        }
    }
    has_target_header && has_code_change
}

/// Extract a unified diff block from raw LLM output or markdown fences.
pub fn extract_unified_diff(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.contains("```") {
        let mut inside_fence = false;
        let mut current_block = Vec::new();
        let mut best_diff_block: Option<String> = None;
        for line in trimmed.lines() {
            if line.trim_start().starts_with("```") {
                if inside_fence {
                    let block = current_block.join("\n");
                    if block.contains("--- a/") && block.contains("+++ b/") {
                        best_diff_block = Some(block);
                        break;
                    }
                    current_block.clear();
                    inside_fence = false;
                } else {
                    inside_fence = true;
                }
            } else if inside_fence {
                current_block.push(line);
            }
        }
        if let Some(diff) = best_diff_block {
            return diff;
        }
    }

    // Fallback: extract lines starting from the first `--- a/` or `diff --git`
    let mut diff_lines = Vec::new();
    let mut started = false;
    for line in trimmed.lines() {
        if line.starts_with("diff --git ") || line.starts_with("--- a/") {
            started = true;
        }
        if started {
            if line.trim_start().starts_with("```") {
                break;
            }
            diff_lines.push(line);
        }
    }
    if !diff_lines.is_empty() {
        return diff_lines.join("\n");
    }
    trimmed.to_string()
}

/// Parse an LLM response into either a structured `RepoToolCall` or a direct `ApplyPatch` call if it emitted a unified diff.
pub fn parse_model_action(raw: &str) -> Option<RepoToolCall> {
    let trimmed = raw.trim();

    // 0. SEARCH/REPLACE blocks (preferred edit format)
    if trimmed.contains("SEARCH") {
        let blocks = parse_edit_blocks(trimmed);
        if !blocks.is_empty() {
            return Some(RepoToolCall::EditBlocks { blocks });
        }
    }

    // 1. Try direct JSON object or JSON inside markdown fence
    let mut json_candidates = vec![trimmed.to_string()];
    if trimmed.contains("```") {
        let mut in_fence = false;
        let mut block = Vec::new();
        for line in trimmed.lines() {
            if line.trim_start().starts_with("```") {
                if in_fence {
                    json_candidates.push(block.join("\n"));
                    block.clear();
                    in_fence = false;
                } else {
                    in_fence = true;
                }
            } else if in_fence {
                block.push(line);
            }
        }
    }
    // Also extract first `{ ... }` substring if model included prose around JSON
    if let (Some(first_brace), Some(last_brace)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if first_brace < last_brace {
            json_candidates.push(trimmed[first_brace..=last_brace].to_string());
        }
    }

    for candidate in json_candidates {
        if let Ok(call) = serde_json::from_str::<RepoToolCall>(candidate.trim()) {
            return Some(call);
        }
    }

    // 2. If the response contains a unified diff (`--- a/` and `+++ b/`), treat it as `repo:apply_patch`
    let extracted_diff = extract_unified_diff(trimmed);
    if extracted_diff.contains("--- a/") && extracted_diff.contains("+++ b/") {
        return Some(RepoToolCall::ApplyPatch {
            patch: extracted_diff,
        });
    }

    None
}

/// Probe candidate local endpoints and identify the active model ID and hash without hardcoding fake metadata.
pub async fn identify_local_model(
    client: &reqwest::Client,
    endpoint_url: &str,
    configured_model_path: Option<&Path>,
) -> IdentifiedModel {
    let mut model_id = std::env::var("SAIL_FORGE_MODEL").ok().unwrap_or_default();
    let mut discovered_path: Option<PathBuf> = configured_model_path.map(|p| p.to_path_buf());

    // Derive base URL (strip `/v1/chat/completions`)
    let base_url = endpoint_url
        .trim_end_matches('/')
        .strip_suffix("/v1/chat/completions")
        .unwrap_or(endpoint_url.trim_end_matches('/'));

    // Query `/v1/models` on the local endpoint
    let models_url = format!("{}/v1/models", base_url);
    if let Ok(resp) = client
        .get(&models_url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(val) = resp.json::<serde_json::Value>().await {
                if let Some(first) = val
                    .get("data")
                    .and_then(|d| d.as_array())
                    .and_then(|arr| arr.first())
                {
                    if let Some(id) = first.get("id").and_then(|v| v.as_str()) {
                        if model_id.is_empty() {
                            model_id = id.to_string();
                        }
                        let candidate_path = PathBuf::from(id);
                        if discovered_path.is_none() && candidate_path.is_file() {
                            discovered_path = Some(candidate_path);
                        }
                    }
                }
            }
        }
    }

    // Also query `/props` (llama-server exposes `model_path` here)
    let props_url = format!("{}/props", base_url);
    if let Ok(resp) = client
        .get(&props_url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(val) = resp.json::<serde_json::Value>().await {
                if let Some(mp) = val.get("model_path").and_then(|v| v.as_str()) {
                    let p = PathBuf::from(mp);
                    if model_id.is_empty() {
                        if let Some(fname) = p.file_name() {
                            model_id = fname.to_string_lossy().to_string();
                        }
                    }
                    if discovered_path.is_none() && p.is_file() {
                        discovered_path = Some(p);
                    }
                }
            }
        }
    }

    if model_id.is_empty() {
        if let Some(ref p) = discovered_path {
            if let Some(fname) = p.file_name() {
                model_id = fname.to_string_lossy().to_string();
            }
        }
    }
    if model_id.is_empty() {
        model_id = "unknown".to_string();
    }

    // Compute truthful fingerprint from actual model file if accessible; otherwise "unknown"
    let model_hash = if let Some(ref path) = discovered_path {
        compute_file_fingerprint(path).unwrap_or_else(|| "unknown".to_string())
    } else {
        "unknown".to_string()
    };

    IdentifiedModel {
        model_id,
        model_hash,
        endpoint: endpoint_url.to_string(),
    }
}

/// Compute a fast deterministic SHA-256 fingerprint of a model file (hashing file size + first/last 64KB chunks, or full file if <= 128KB).
fn compute_file_fingerprint(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut ctx = Context::new(&SHA256);
    ctx.update(&len.to_le_bytes());
    let chunk_size = 64 * 1024usize;
    if len <= (chunk_size * 2) as u64 {
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).ok()?;
        ctx.update(&buf);
    } else {
        let mut head = vec![0u8; chunk_size];
        f.read_exact(&mut head).ok()?;
        ctx.update(&head);
        f.seek(SeekFrom::End(-(chunk_size as i64))).ok()?;
        let mut tail = vec![0u8; chunk_size];
        f.read_exact(&mut tail).ok()?;
        ctx.update(&tail);
    }
    let digest = ctx.finish();
    let mut hex = String::new();
    for byte in digest.as_ref() {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{:02x}", byte);
    }
    Some(hex)
}

/// Multi-turn local issue solver controller.
pub struct LocalSolverController {
    pub max_turns: u32,
    pub max_iterations: u32,
    pub command_timeout_secs: u64,
    pub configured_model_path: Option<PathBuf>,
}

impl LocalSolverController {
    pub fn new(
        max_iterations: u32,
        command_timeout_secs: u64,
        configured_model_path: Option<PathBuf>,
    ) -> Self {
        let max_turns = (max_iterations * 3).clamp(6, 18);
        Self {
            max_turns,
            max_iterations: max_iterations.max(1),
            command_timeout_secs,
            configured_model_path,
        }
    }

    pub fn candidate_endpoints() -> Vec<String> {
        let mut urls = Vec::new();
        if let Ok(url) = std::env::var("SAIL_FORGE_LLM_URL") {
            if !url.trim().is_empty() {
                urls.push(url);
                return urls;
            }
        }
        if let Ok(url) = std::env::var("SAIL_LLM_URL") {
            if !url.trim().is_empty() && !urls.contains(&url) {
                urls.push(url);
            }
        }
        for fallback in [
            "http://127.0.0.1:8081/v1/chat/completions",
            "http://127.0.0.1:8080/v1/chat/completions",
            "http://127.0.0.1:11434/v1/chat/completions",
        ] {
            let s = fallback.to_string();
            if !urls.contains(&s) {
                urls.push(s);
            }
        }
        urls
    }

    /// Gather initial workspace snapshot (`repo:list` and up to 2 relevant entrypoint files)
    /// strictly bounded so the initial prompt stays under ~1,000 tokens while allowing
    /// the model to explore further with `repo:search`, `repo:read_range`, and `repo:list`.
    pub fn build_initial_workspace_context(&self, worktree: &IsolatedWorktree) -> String {
        let mut files = Vec::new();
        collect_files_bounded(&worktree.root_dir, &worktree.root_dir, &mut files, 40);
        files.sort();

        let mut out = String::new();
        out.push_str("Workspace files (`repo:list`):\n");
        for f in &files {
            out.push_str(&format!("- {}\n", f));
        }

        // Bounded pre-reading: pre-read at most 2 entry files (max 80 lines each)
        // and strictly cap the total initial context to 4,000 characters (~1,000 tokens)
        // to prevent prompt bloat and keep context well within local inference budgets.
        let mut read_count = 0usize;
        for f in &files {
            if read_count >= 2 || out.len() >= 4000 {
                break;
            }
            if f == "Cargo.toml" || f.ends_with("lib.rs") || f.ends_with("main.rs") {
                if let Ok(content) = worktree.read_file(Path::new(f)) {
                    let lines: Vec<&str> = content.lines().collect();
                    if lines.len() <= 80 {
                        out.push_str(&format!("\n--- File: {} ---\n", f));
                        for (idx, line) in lines.iter().enumerate() {
                            out.push_str(&format!("{:4}: {}\n", idx + 1, line));
                        }
                        read_count += 1;
                    }
                }
            }
        }
        out
    }

    /// Prune older intermediate tool turns if total message content exceeds the character budget,
    /// preserving the initial system prompt (index 0), the initial issue description (index 1),
    /// and the latest turn context.
    pub fn prune_messages_to_budget(messages: &mut Vec<serde_json::Value>, max_chars: usize) {
        if messages.len() <= 3 {
            return;
        }
        while messages.len() > 4 {
            let curr: usize = messages
                .iter()
                .map(|m| {
                    m.get("content")
                        .and_then(|c| c.as_str())
                        .map(str::len)
                        .unwrap_or(0)
                })
                .sum();
            if curr <= max_chars {
                break;
            }
            messages.remove(2);
        }
    }

    /// Run the bounded local solver loop inside `worktree`.
    pub async fn solve_issue<F, P>(
        &self,
        worktree: &IsolatedWorktree,
        issue_title: &str,
        issue_body: &str,
        log_dir: &Path,
        mut is_cancelled: F,
        mut on_progress: P,
    ) -> SolverOutcome
    where
        F: FnMut() -> bool,
        P: FnMut(SolverProgress),
    {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(90))
            .build()
            .unwrap_or_default();

        let endpoints = Self::candidate_endpoints();
        let initial_ctx = self.build_initial_workspace_context(worktree);

        on_progress(SolverProgress {
            turn: 1,
            max_turns: self.max_turns,
            iteration: 1,
            stage: "planning".into(),
            message: format!("Inspecting workspace context for issue: {}", issue_title),
            live_output: Some("Preparing repository context and workspace symbols...".into()),
            model_action: Some("Context initialization".into()),
            diff: worktree.get_diff().ok(),
            target_file: None,
            logs: Some(vec![
                format!("Loaded issue: {}", issue_title),
                "Inspecting workspace crates and candidate target files...".into(),
            ]),
        });

        let system_prompt = r#"You are SAIL Forge Local Issue Solver v0.2, an autonomous Rust coding agent operating inside an isolated sandbox worktree.
Each reply must be EITHER one JSON tool call OR one or more SEARCH/REPLACE edit blocks.

Tools (JSON):
1. {"tool": "repo:list", "path": "."}
2. {"tool": "repo:search", "query": "function_or_symbol"}
3. {"tool": "repo:read_range", "path": "src/lib.rs", "start_line": 1, "end_line": 120}
4. {"tool": "repo:diff"}
5. {"tool": "repo:test"}  (sandboxed cargo test)
6. {"tool": "done", "summary": "what changed"}  (only after repo:test passes)

Edits (preferred; no JSON escaping needed). Write the file path on its own line, then:
src/lib.rs
<<<<<<< SEARCH
exact existing lines to replace
=======
replacement lines
>>>>>>> REPLACE

Edit rules:
- SEARCH must copy the current file lines exactly and be unique in the file; include 2-3 surrounding lines if needed.
- Use several blocks for several places or files; they are applied together.
- To create a new file use an empty SEARCH section.
- Unified diffs (--- a/ +++ b/) are still accepted but SEARCH/REPLACE is more reliable.

Rules:
- Fix the root cause in real Rust code; never emit comment-only changes.
- Keep existing tests; do not delete or #[ignore] them.
- Edits are tested automatically; finish with done once tests pass."#;

        let user_prompt = format!(
            "Issue Title: {}\nIssue Description:\n{}\n\n{}",
            issue_title, issue_body, initial_ctx
        );

        let mut messages = vec![
            serde_json::json!({ "role": "system", "content": system_prompt }),
            serde_json::json!({ "role": "user", "content": user_prompt }),
        ];

        let mut active_endpoint: Option<String> = None;
        let mut identified_model = IdentifiedModel {
            model_id: "unknown".into(),
            model_hash: "unknown".into(),
            endpoint: "none".into(),
        };

        let mut turns = 0u32;
        let mut iterations = 0u32;
        let mut test_records: Vec<TestExecutionRecord> = Vec::new();
        let mut tests_verified = false;
        let mut last_error: Option<String> = None;
        let mut edit_failures = 0u32;
        let mut parse_failures = 0u32;

        while turns < self.max_turns && iterations < self.max_iterations {
            if is_cancelled() {
                let final_diff = worktree.get_diff().unwrap_or_default();
                let modified_files = extract_modified_files_from_diff(&final_diff);
                return SolverOutcome {
                    success: false,
                    verification_status: "cancelled".into(),
                    failure_reason: Some("Cancelled by operator".into()),
                    target_file: modified_files.first().cloned(),
                    final_diff,
                    modified_files,
                    iterations,
                    turns,
                    test_records,
                    edit_failures,
                    parse_failures,
                    model: identified_model,
                    summary: "Cancelled by operator".into(),
                };
            }

            turns += 1;

            on_progress(SolverProgress {
                turn: turns,
                max_turns: self.max_turns,
                iteration: iterations + 1,
                stage: "coding".into(),
                message: format!(
                    "Turn {}/{}: Consulting local coding model...",
                    turns, self.max_turns
                ),
                live_output: Some(
                    "Prompting model with issue requirements and workspace symbols...".into(),
                ),
                model_action: Some("Model inference started".into()),
                diff: worktree.get_diff().ok(),
                target_file: None,
                logs: Some(vec![format!(
                    "Turn {}/{}: Querying model at endpoint...",
                    turns, self.max_turns
                )]),
            });

            // Bound accumulated multi-turn history to keep prompts within context limits
            Self::prune_messages_to_budget(&mut messages, 24000);

            // Call local OpenAI-compatible chat completions endpoint with streaming support
            let mut turn_response_text: Option<String> = None;
            let mut turn_endpoint_err: Option<String> = None;

            let probe_list: Vec<String> = if let Some(ref ep) = active_endpoint {
                vec![ep.clone()]
            } else {
                endpoints.clone()
            };

            let model_name = std::env::var("SAIL_FORGE_MODEL").unwrap_or_else(|_| {
                if let Some(ref p) = self.configured_model_path {
                    p.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Qwen3.6-35B-A3B".to_string())
                } else {
                    "Qwen3.6-35B-A3B".to_string()
                }
            });

            for ep in &probe_list {
                let req_body = serde_json::json!({
                    "model": model_name,
                    "messages": messages,
                    "temperature": 0.1,
                    "max_tokens": 2048,
                    "stream": true,
                });

                match client.post(ep).json(&req_body).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        if active_endpoint.is_none() {
                            active_endpoint = Some(ep.clone());
                            identified_model = identify_local_model(
                                &client,
                                ep,
                                self.configured_model_path.as_deref(),
                            )
                            .await;
                        }

                        let is_sse = resp
                            .headers()
                            .get(reqwest::header::CONTENT_TYPE)
                            .and_then(|v| v.to_str().ok())
                            .map(|s| s.contains("text/event-stream"))
                            .unwrap_or(false);

                        if is_sse {
                            let mut stream = resp.bytes_stream();
                            let mut accumulated = String::new();
                            let mut line_buf = String::new();
                            let mut token_count = 0u32;
                            let mut last_emit = std::time::Instant::now();

                            while let Some(chunk_res) = stream.next().await {
                                if is_cancelled() {
                                    break;
                                }
                                if let Ok(bytes) = chunk_res {
                                    line_buf.push_str(&String::from_utf8_lossy(&bytes));
                                    while let Some(idx) = line_buf.find('\n') {
                                        let line = line_buf[..idx].trim().to_string();
                                        line_buf = line_buf[idx + 1..].to_string();
                                        if let Some(data) = line.strip_prefix("data:") {
                                            let data = data.trim();
                                            if data == "[DONE]" {
                                                break;
                                            }
                                            if let Ok(val) =
                                                serde_json::from_str::<serde_json::Value>(data)
                                            {
                                                let delta = val
                                                    .get("choices")
                                                    .and_then(|c| c.get(0))
                                                    .and_then(|c| c.get("delta"));
                                                let piece = delta
                                                    .and_then(|d| {
                                                        d.get("content").and_then(|v| v.as_str())
                                                    })
                                                    .or_else(|| {
                                                        delta.and_then(|d| {
                                                            d.get("reasoning_content")
                                                                .and_then(|v| v.as_str())
                                                        })
                                                    });

                                                if let Some(text_chunk) = piece {
                                                    accumulated.push_str(text_chunk);
                                                    token_count += 1;
                                                    if last_emit.elapsed()
                                                        > std::time::Duration::from_millis(120)
                                                        || text_chunk.contains('\n')
                                                    {
                                                        on_progress(SolverProgress {
                                                            turn: turns,
                                                            max_turns: self.max_turns,
                                                            iteration: iterations + 1,
                                                            stage: "coding".into(),
                                                            message: format!(
                                                                "Turn {}/{}: Generating code patch ({} tokens)...",
                                                                turns, self.max_turns, token_count
                                                            ),
                                                            live_output: Some(accumulated.clone()),
                                                            model_action: Some(
                                                                "Streaming model output".into(),
                                                            ),
                                                            diff: worktree.get_diff().ok(),
                                                            target_file: None,
                                                            logs: None,
                                                        });
                                                        last_emit = std::time::Instant::now();
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if !accumulated.trim().is_empty() {
                                turn_response_text = Some(accumulated);
                                break;
                            }
                        } else if let Ok(json_val) = resp.json::<serde_json::Value>().await {
                            // If response JSON itself carries a `model` field and identified_model was unknown, capture it
                            if identified_model.model_id == "unknown" {
                                if let Some(m) = json_val.get("model").and_then(|v| v.as_str()) {
                                    if !m.trim().is_empty() {
                                        identified_model.model_id = m.trim().to_string();
                                    }
                                }
                            }
                            let msg_obj = json_val
                                .get("choices")
                                .and_then(|c| c.get(0))
                                .and_then(|c| c.get("message"));

                            let content = msg_obj
                                .and_then(|m| m.get("content"))
                                .and_then(|c| c.as_str())
                                .unwrap_or_default();

                            let reasoning = msg_obj
                                .and_then(|m| m.get("reasoning_content"))
                                .and_then(|r| r.as_str())
                                .unwrap_or_default();

                            let text = if !content.trim().is_empty() {
                                content.to_string()
                            } else if !reasoning.trim().is_empty() {
                                reasoning.to_string()
                            } else {
                                String::new()
                            };

                            if !text.is_empty() {
                                turn_response_text = Some(text);
                                break;
                            }
                        }
                    }
                    Ok(resp) => {
                        let status = resp.status();
                        let body_snippet = resp.text().await.unwrap_or_default();
                        let snippet = if body_snippet.len() > 180 {
                            format!("{}...", &body_snippet[..180])
                        } else {
                            body_snippet
                        };
                        turn_endpoint_err = Some(format!(
                            "Endpoint {} returned HTTP {}: {}",
                            ep, status, snippet
                        ));
                    }
                    Err(e) => {
                        turn_endpoint_err = Some(format!("Endpoint {} unreachable: {}", ep, e));
                    }
                }
            }

            let raw_reply = match turn_response_text {
                Some(t) => t,
                None => {
                    let reason = turn_endpoint_err.unwrap_or_else(|| {
                        "No local OpenAI-compatible inference server available".to_string()
                    });
                    warn!(
                        "Local solver model unavailable on turn {}: {}",
                        turns, reason
                    );
                    let final_diff = worktree.get_diff().unwrap_or_default();
                    let modified_files = extract_modified_files_from_diff(&final_diff);
                    return SolverOutcome {
                        success: false,
                        verification_status: "model_unavailable".into(),
                        failure_reason: Some(reason.clone()),
                        target_file: modified_files.first().cloned(),
                        final_diff,
                        modified_files,
                        iterations,
                        turns,
                        test_records,
                        edit_failures,
                        parse_failures,
                        model: identified_model,
                        summary: reason,
                    };
                }
            };

            on_progress(SolverProgress {
                turn: turns,
                max_turns: self.max_turns,
                iteration: iterations + 1,
                stage: "coding".into(),
                message: format!(
                    "Turn {}/{}: Model generation complete ({} chars)",
                    turns,
                    self.max_turns,
                    raw_reply.len()
                ),
                live_output: Some(raw_reply.clone()),
                model_action: Some("Model response ready".into()),
                diff: worktree.get_diff().ok(),
                target_file: None,
                logs: Some(vec![format!(
                    "Turn {}: Model response received ({} chars)",
                    turns,
                    raw_reply.len()
                )]),
            });

            messages.push(serde_json::json!({
                "role": "assistant",
                "content": raw_reply
            }));

            let action = match parse_model_action(&raw_reply) {
                Some(a) => a,
                None => {
                    parse_failures += 1;
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": "Could not parse a tool call or edit. Reply with either one JSON tool call (e.g. {\"tool\":\"repo:read_range\",\"path\":\"src/lib.rs\",\"start_line\":1,\"end_line\":80}) or SEARCH/REPLACE edit blocks."
                    }));
                    continue;
                }
            };

            // Determine whether the model emitted a raw unified diff directly (vs an explicit JSON `repo:apply_patch` tool call)
            let is_direct_diff = !raw_reply.contains("\"tool\"")
                && matches!(
                    action,
                    RepoToolCall::ApplyPatch { .. } | RepoToolCall::EditBlocks { .. }
                );

            match &action {
                RepoToolCall::Done { summary } => {
                    on_progress(SolverProgress {
                        turn: turns,
                        max_turns: self.max_turns,
                        iteration: iterations + 1,
                        stage: "review".into(),
                        message: format!(
                            "Turn {}/{}: Model marked issue as resolved",
                            turns, self.max_turns
                        ),
                        live_output: Some(raw_reply.clone()),
                        model_action: Some("done".into()),
                        diff: worktree.get_diff().ok(),
                        target_file: None,
                        logs: Some(vec![format!(
                            "Done: {}",
                            summary.as_deref().unwrap_or("Resolved")
                        )]),
                    });

                    // Verify that we have a real diff and that staged tests pass before accepting `done`
                    let current_diff = worktree.get_diff().unwrap_or_default();
                    if !is_meaningful_code_diff(&current_diff) {
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": "Cannot finish yet: worktree has no meaningful code diff. Apply a patch with `repo:apply_patch` first."
                        }));
                        continue;
                    }
                    if !tests_verified {
                        iterations += 1;
                        let test_res = execute_repo_tool(
                            worktree,
                            &RepoToolCall::Test { filter: None },
                            log_dir,
                            self.command_timeout_secs,
                            iterations,
                        );
                        if let Some(rec) = test_res.test_record.clone() {
                            test_records.push(rec);
                        }
                        if !test_res.ok {
                            last_error = Some(test_res.output.clone());
                            messages.push(serde_json::json!({
                                "role": "user",
                                "content": format!("Tests failed when verifying completion:\n{}\nPlease inspect and apply a repair patch.", test_res.output)
                            }));
                            continue;
                        }
                    }

                    let final_diff = worktree.get_diff().unwrap_or_default();
                    let modified_files = extract_modified_files_from_diff(&final_diff);
                    return SolverOutcome {
                        success: true,
                        verification_status: "verified".into(),
                        failure_reason: None,
                        target_file: modified_files.first().cloned(),
                        final_diff,
                        modified_files,
                        iterations,
                        turns,
                        test_records,
                        edit_failures,
                        parse_failures,
                        model: identified_model,
                        summary: summary
                            .clone()
                            .unwrap_or_else(|| "Solved and verified inside sandbox".into()),
                    };
                }
                RepoToolCall::ApplyPatch { .. }
                | RepoToolCall::Edit { .. }
                | RepoToolCall::EditBlocks { .. } => {
                    let patch = &action.edit_preview();
                    let modified_files = action.edit_paths();
                    let target_candidate = modified_files.first().cloned();

                    on_progress(SolverProgress {
                        turn: turns,
                        max_turns: self.max_turns,
                        iteration: iterations + 1,
                        stage: "coding".into(),
                        message: format!(
                            "Turn {}/{}: Applying patch to {}",
                            turns,
                            self.max_turns,
                            modified_files.join(", ")
                        ),
                        live_output: Some(raw_reply.clone()),
                        model_action: Some(format!(
                            "repo:apply_patch ({})",
                            modified_files.join(", ")
                        )),
                        diff: Some(patch.clone()),
                        target_file: target_candidate.clone(),
                        logs: Some(vec![format!(
                            "Applying unified patch ({} lines)...",
                            patch.lines().count()
                        )]),
                    });

                    let patch_res = execute_repo_tool(
                        worktree,
                        &action,
                        log_dir,
                        self.command_timeout_secs,
                        iterations + 1,
                    );
                    if !patch_res.ok {
                        edit_failures += 1;
                        last_error = Some(patch_res.output.clone());
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": format!("Edit failed:\n{}\nRe-read the file with `repo:read_range` and emit a corrected SEARCH/REPLACE block whose SEARCH copies the current lines exactly.", patch_res.output)
                        }));
                        continue;
                    }

                    // Patch applied! Mark tests as needing verification.
                    tests_verified = false;

                    let current_diff = worktree.get_diff().unwrap_or_default();
                    on_progress(SolverProgress {
                        turn: turns,
                        max_turns: self.max_turns,
                        iteration: iterations + 1,
                        stage: "coding".into(),
                        message: format!(
                            "Turn {}/{}: Patch applied ({} modified lines). Ready for tests.",
                            turns,
                            self.max_turns,
                            current_diff.lines().count()
                        ),
                        live_output: Some(raw_reply.clone()),
                        model_action: Some("Patch applied successfully".into()),
                        diff: Some(current_diff.clone()),
                        target_file: target_candidate,
                        logs: Some(vec![
                            "Unified diff applied cleanly to isolated worktree.".into(),
                        ]),
                    });

                    // If the model emitted a raw unified diff directly, automatically run `repo:test` in this iteration
                    if is_direct_diff {
                        iterations += 1;
                        on_progress(SolverProgress {
                            turn: turns,
                            max_turns: self.max_turns,
                            iteration: iterations,
                            stage: "testing".into(),
                            message: format!(
                                "Turn {}/{}: Running sandbox cargo test...",
                                turns, self.max_turns
                            ),
                            live_output: Some(raw_reply.clone()),
                            model_action: Some("repo:test".into()),
                            diff: Some(current_diff.clone()),
                            target_file: None,
                            logs: Some(vec!["Executing cargo test in sandbox worktree...".into()]),
                        });

                        let test_res = execute_repo_tool(
                            worktree,
                            &RepoToolCall::Test { filter: None },
                            log_dir,
                            self.command_timeout_secs,
                            iterations,
                        );
                        if let Some(rec) = test_res.test_record.clone() {
                            test_records.push(rec);
                        }
                        if test_res.ok {
                            let final_diff = worktree.get_diff().unwrap_or_default();
                            let modified_files = extract_modified_files_from_diff(&final_diff);
                            info!(
                                "Local solver verified fix on iteration {} (modified files: {:?})",
                                iterations, modified_files
                            );
                            on_progress(SolverProgress {
                                turn: turns,
                                max_turns: self.max_turns,
                                iteration: iterations,
                                stage: "review".into(),
                                message: "All sandbox tests PASSED! Verified fix ready.".into(),
                                live_output: Some(raw_reply.clone()),
                                model_action: Some("cargo test: PASSED".into()),
                                diff: Some(final_diff.clone()),
                                target_file: modified_files.first().cloned(),
                                logs: Some(vec![
                                    "All sandbox tests passed with exit code 0.".into(),
                                ]),
                            });
                            return SolverOutcome {
                                success: true,
                                verification_status: "verified".into(),
                                failure_reason: None,
                                target_file: modified_files.first().cloned(),
                                final_diff,
                                modified_files,
                                iterations,
                                turns,
                                test_records,
                                edit_failures,
                                parse_failures,
                                model: identified_model,
                                summary: "Patch applied and cargo test passed".into(),
                            };
                        } else {
                            last_error = Some(test_res.output.clone());
                            let test_code = test_res
                                .test_record
                                .as_ref()
                                .map(|r| r.exit_code)
                                .unwrap_or(1);
                            on_progress(SolverProgress {
                                turn: turns,
                                max_turns: self.max_turns,
                                iteration: iterations,
                                stage: "coding".into(),
                                message: "cargo test failed. Preparing compiler error diagnostics for repair pass...".into(),
                                live_output: Some(format!("=== TEST FAILURE ===\n{}", test_res.output)),
                                model_action: Some("Tests failed -> Initiating repair pass".into()),
                                diff: Some(current_diff.clone()),
                                target_file: None,
                                logs: Some(vec![
                                    format!("cargo test failed (exit code {}).", test_code),
                                    "Feeding compiler errors to model for repair...".into(),
                                ]),
                            });
                            // Include current content of modified files so repair turn has exact line context
                            let mut current_files_snapshot = String::new();
                            for f in &patch_res.modified_files {
                                if let Ok(c) = worktree.read_file(Path::new(f)) {
                                    current_files_snapshot
                                        .push_str(&format!("\nCurrent content of {}:\n", f));
                                    for (idx, line) in c.lines().take(120).enumerate() {
                                        current_files_snapshot.push_str(&format!(
                                            "{:4}: {}\n",
                                            idx + 1,
                                            line
                                        ));
                                    }
                                }
                            }
                            messages.push(serde_json::json!({
                                "role": "user",
                                "content": format!(
                                    "{}\n`repo:test` failed:\n{}\n{}\nEmit a repair unified diff or tool call against the current file state.",
                                    patch_res.output, test_res.output, current_files_snapshot
                                )
                            }));
                        }
                    } else {
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": format!("{}\nNow call {{\"tool\": \"repo:test\"}} to verify the build and tests.", patch_res.output)
                        }));
                    }
                }
                RepoToolCall::Test { .. } => {
                    iterations += 1;
                    on_progress(SolverProgress {
                        turn: turns,
                        max_turns: self.max_turns,
                        iteration: iterations,
                        stage: "testing".into(),
                        message: format!(
                            "Turn {}/{}: Running sandboxed `cargo test`...",
                            turns, self.max_turns
                        ),
                        live_output: Some(raw_reply.clone()),
                        model_action: Some("repo:test".into()),
                        diff: worktree.get_diff().ok(),
                        target_file: None,
                        logs: Some(vec!["Executing cargo test in sandbox...".into()]),
                    });

                    let test_res = execute_repo_tool(
                        worktree,
                        &action,
                        log_dir,
                        self.command_timeout_secs,
                        iterations,
                    );
                    if let Some(rec) = test_res.test_record.clone() {
                        test_records.push(rec);
                    }
                    let current_diff = worktree.get_diff().unwrap_or_default();
                    if test_res.ok && is_meaningful_code_diff(&current_diff) {
                        let modified_files = extract_modified_files_from_diff(&current_diff);
                        on_progress(SolverProgress {
                            turn: turns,
                            max_turns: self.max_turns,
                            iteration: iterations,
                            stage: "review".into(),
                            message: "Sandboxed tests PASSED with verified code diff!".into(),
                            live_output: Some(raw_reply.clone()),
                            model_action: Some("cargo test: PASSED".into()),
                            diff: Some(current_diff.clone()),
                            target_file: modified_files.first().cloned(),
                            logs: Some(vec!["All sandbox tests passed with exit code 0.".into()]),
                        });
                        return SolverOutcome {
                            success: true,
                            verification_status: "verified".into(),
                            failure_reason: None,
                            target_file: modified_files.first().cloned(),
                            final_diff: current_diff,
                            modified_files,
                            iterations,
                            turns,
                            test_records,
                            edit_failures,
                            parse_failures,
                            model: identified_model,
                            summary: "Sandboxed cargo test passed with verified code diff".into(),
                        };
                    } else if test_res.ok {
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": format!("`repo:test` output:\n{}\nNote: no code changes have been applied yet.", test_res.output)
                        }));
                    } else {
                        tests_verified = false;
                        last_error = Some(test_res.output.clone());
                        let test_code = test_res
                            .test_record
                            .as_ref()
                            .map(|r| r.exit_code)
                            .unwrap_or(1);
                        on_progress(SolverProgress {
                            turn: turns,
                            max_turns: self.max_turns,
                            iteration: iterations,
                            stage: "coding".into(),
                            message: "Tests failed. Prompting model for repair pass...".into(),
                            live_output: Some(format!("=== TEST FAILURE ===\n{}", test_res.output)),
                            model_action: Some("Tests failed -> Initiating repair pass".into()),
                            diff: Some(current_diff),
                            target_file: None,
                            logs: Some(vec![
                                format!("cargo test failed (exit code {}).", test_code),
                                "Prompting model with compiler diagnostics for repair...".into(),
                            ]),
                        });
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": format!("`repo:test` failed:\n{}\nInspect the failure and apply a repair patch.", test_res.output)
                        }));
                    }
                }
                other_tool => {
                    let tool_desc = match other_tool {
                        RepoToolCall::Search { query, .. } => format!("repo:search \"{}\"", query),
                        RepoToolCall::ReadRange {
                            path,
                            start_line,
                            end_line,
                        } => format!("repo:read_range {}:{}-{}", path, start_line, end_line),
                        RepoToolCall::List { path } => {
                            format!("repo:list {}", path.as_deref().unwrap_or("."))
                        }
                        RepoToolCall::Diff => "repo:diff".into(),
                        _ => "tool_call".into(),
                    };
                    on_progress(SolverProgress {
                        turn: turns,
                        max_turns: self.max_turns,
                        iteration: iterations + 1,
                        stage: "coding".into(),
                        message: format!(
                            "Turn {}/{}: Executing {}",
                            turns, self.max_turns, tool_desc
                        ),
                        live_output: Some(raw_reply.clone()),
                        model_action: Some(tool_desc.clone()),
                        diff: worktree.get_diff().ok(),
                        target_file: None,
                        logs: Some(vec![format!("Executed {}", tool_desc)]),
                    });

                    let res = execute_repo_tool(
                        worktree,
                        other_tool,
                        log_dir,
                        self.command_timeout_secs,
                        iterations,
                    );
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": format!("Tool `{}` result:\n{}", res.tool, res.output)
                    }));
                }
            }
        }

        // Loop exhausted without passing tests
        let final_diff = worktree.get_diff().unwrap_or_default();
        let modified_files = extract_modified_files_from_diff(&final_diff);
        let reason = last_error.unwrap_or_else(|| {
            format!(
                "Exhausted solver budget ({} turns, {} test iterations) without passing verification",
                turns, iterations
            )
        });
        SolverOutcome {
            success: false,
            verification_status: "tests_failed".into(),
            failure_reason: Some(reason.clone()),
            target_file: modified_files.first().cloned(),
            final_diff,
            modified_files,
            iterations,
            turns,
            test_records,
            edit_failures,
            parse_failures,
            model: identified_model,
            summary: reason,
        }
    }
}
