use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::contracts::TestExecutionRecord;
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
}

/// Execute a bounded repository tool strictly inside `worktree`.
pub fn execute_repo_tool(
    worktree: &IsolatedWorktree,
    call: &RepoToolCall,
    log_dir: &Path,
    timeout_secs: u64,
    step_idx: u32,
) -> RepoToolResult {
    match call {
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

    /// Gather initial workspace snapshot (`repo:list` and up to 6 relevant source files with line numbers)
    /// so local models have immediate grounded context and can also call `repo:search` / `repo:read_range` for larger repos.
    fn build_initial_workspace_context(&self, worktree: &IsolatedWorktree) -> String {
        let mut files = Vec::new();
        collect_files_bounded(&worktree.root_dir, &worktree.root_dir, &mut files, 100);
        files.sort();

        let mut out = String::new();
        out.push_str("Workspace files (`repo:list`):\n");
        for f in &files {
            out.push_str(&format!("- {}\n", f));
        }

        // Pre-read up to 6 small source files (`.rs` or `Cargo.toml`) so 1-2 file fixtures can be inspected immediately
        let mut read_count = 0usize;
        for f in &files {
            if read_count >= 6 {
                break;
            }
            if f.ends_with(".rs") || f == "Cargo.toml" {
                if let Ok(content) = worktree.read_file(Path::new(f)) {
                    if content.lines().count() <= 160 {
                        out.push_str(&format!("\n--- File: {} ---\n", f));
                        for (idx, line) in content.lines().enumerate() {
                            out.push_str(&format!("{:4}: {}\n", idx + 1, line));
                        }
                        read_count += 1;
                    }
                }
            }
        }
        out
    }

    /// Run the bounded local solver loop inside `worktree`.
    pub async fn solve_issue<F>(
        &self,
        worktree: &IsolatedWorktree,
        issue_title: &str,
        issue_body: &str,
        log_dir: &Path,
        mut is_cancelled: F,
    ) -> SolverOutcome
    where
        F: FnMut() -> bool,
    {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(90))
            .build()
            .unwrap_or_default();

        let endpoints = Self::candidate_endpoints();
        let initial_ctx = self.build_initial_workspace_context(worktree);

        let system_prompt = r#"You are SAIL Forge Local Issue Solver v0.1, an autonomous Rust coding agent operating inside an isolated sandbox worktree.
You have access to the following bounded repository tools. Respond with EITHER a single JSON tool call object OR a unified diff (`--- a/<path>` / `+++ b/<path>`):

1. List files:
{"tool": "repo:list", "path": "."}
2. Search lines/symbols across files:
{"tool": "repo:search", "query": "function_or_symbol"}
3. Read line range (up to 200 lines):
{"tool": "repo:read_range", "path": "src/lib.rs", "start_line": 1, "end_line": 120}
4. Inspect current git diff:
{"tool": "repo:diff"}
5. Apply a unified diff patch (supports modifying 1 or multiple files in a single patch):
{"tool": "repo:apply_patch", "patch": "--- a/src/file1.rs\n+++ b/src/file1.rs\n@@ -1,3 +1,3 @@\n-old\n+new\n"}
6. Run sandboxed `cargo test`:
{"tool": "repo:test"}
7. Finish after tests pass:
{"tool": "done", "summary": "Fixed issue and verified tests pass"}

Rules:
- Never emit comment-only patches. Modify real Rust code to fix the root cause.
- When fixing a multi-file bug, include hunks for all required files (`--- a/...` / `+++ b/...`) or apply patches sequentially.
- Always ensure `repo:test` passes before finishing."#;

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
                    model: identified_model,
                    summary: "Cancelled by operator".into(),
                };
            }

            turns += 1;

            // Call local OpenAI-compatible chat completions endpoint
            let mut turn_response_text: Option<String> = None;
            let mut turn_endpoint_err: Option<String> = None;

            let probe_list: Vec<String> = if let Some(ref ep) = active_endpoint {
                vec![ep.clone()]
            } else {
                endpoints.clone()
            };

            for ep in &probe_list {
                let req_body = serde_json::json!({
                    "model": std::env::var("SAIL_FORGE_MODEL").unwrap_or_else(|_| "local-coder".to_string()),
                    "messages": messages,
                    "temperature": 0.1,
                    "max_tokens": 2048
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
                        if let Ok(json_val) = resp.json::<serde_json::Value>().await {
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
                        turn_endpoint_err =
                            Some(format!("Endpoint {} returned HTTP {}", ep, resp.status()));
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
                        model: identified_model,
                        summary: reason,
                    };
                }
            };

            messages.push(serde_json::json!({
                "role": "assistant",
                "content": raw_reply
            }));

            let action = match parse_model_action(&raw_reply) {
                Some(a) => a,
                None => {
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": "Could not parse a valid tool call or unified diff. Emit a JSON object such as {\"tool\":\"repo:apply_patch\",\"patch\":\"--- a/...\\n+++ b/...\"} or a unified diff block."
                    }));
                    continue;
                }
            };

            // Determine whether the model emitted a raw unified diff directly (vs an explicit JSON `repo:apply_patch` tool call)
            let is_direct_diff = !raw_reply.contains("\"tool\"")
                && matches!(action, RepoToolCall::ApplyPatch { .. });

            match &action {
                RepoToolCall::Done { summary } => {
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
                        if test_res.ok {
                            tests_verified = true;
                        } else {
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
                        model: identified_model,
                        summary: summary
                            .clone()
                            .unwrap_or_else(|| "Solved and verified inside sandbox".into()),
                    };
                }
                RepoToolCall::ApplyPatch { .. } => {
                    let patch_res = execute_repo_tool(
                        worktree,
                        &action,
                        log_dir,
                        self.command_timeout_secs,
                        iterations + 1,
                    );
                    if !patch_res.ok {
                        last_error = Some(patch_res.output.clone());
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": format!("Tool `repo:apply_patch` failed:\n{}\nUse `repo:read_range` to check exact file lines and emit an accurate unified diff.", patch_res.output)
                        }));
                        continue;
                    }

                    // Patch applied! Mark tests as needing verification.
                    tests_verified = false;

                    // If the model emitted a raw unified diff directly, automatically run `repo:test` in this iteration
                    if is_direct_diff {
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
                        if test_res.ok {
                            let final_diff = worktree.get_diff().unwrap_or_default();
                            let modified_files = extract_modified_files_from_diff(&final_diff);
                            info!(
                                "Local solver verified fix on iteration {} (modified files: {:?})",
                                iterations, modified_files
                            );
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
                                model: identified_model,
                                summary: "Patch applied and cargo test passed".into(),
                            };
                        } else {
                            last_error = Some(test_res.output.clone());
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
                        tests_verified = true;
                        let modified_files = extract_modified_files_from_diff(&current_diff);
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
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": format!("`repo:test` failed:\n{}\nInspect the failure and apply a repair patch.", test_res.output)
                        }));
                    }
                }
                other_tool => {
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
            model: identified_model,
            summary: reason,
        }
    }
}
