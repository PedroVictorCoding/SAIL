//! Context compaction: keep the live prompt small and retrieve archived data on demand.
//!
//! Policy ("hold little, retrieve often"):
//! 1. **Hot window.** Only the last `hot_turns` exchanges stay verbatim in the prompt.
//! 2. **Archive, never summarise away.** Every turn and every large tool result is written
//!    whole to the [`MemoryStore`](crate::store::MemoryStore); nothing is lost to compaction.
//! 3. **Breadcrumbs.** Older turns appear as one-line extractive gists with a `mem:<id>`
//!    handle, newest first, inside a fixed character budget.
//! 4. **Per-turn recall.** The current prompt is searched against the archive and the best
//!    few excerpts (outside the hot window) are injected, bounded.
//! 5. **Tool offload.** A tool result over `tool_inline_chars` is archived and replaced in
//!    context by a head/tail preview plus its handle; the model reads more via recall.
//!
//! Gists are extractive and deterministic: no model call is spent on compaction, so the
//! single local inference slot stays free for the user's turn.

/// Character budgets for one model request. Characters, not tokens: roughly 3.5–4 chars per
/// token for the local Qwen/Gemma tokenizers, so the defaults cost about 4k tokens in total.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextBudget {
    /// Verbatim exchanges (user + assistant) kept in the prompt.
    pub hot_turns: usize,
    /// Upper bound on the hot window's characters; older hot turns spill to breadcrumbs.
    pub hot_chars: usize,
    /// Breadcrumb block budget.
    pub digest_chars: usize,
    /// Longest single breadcrumb line.
    pub gist_chars: usize,
    /// Number of recalled excerpts per turn.
    pub recall_hits: usize,
    /// Recalled excerpts block budget.
    pub recall_chars: usize,
    /// Tool results longer than this are archived and replaced by a preview.
    pub tool_inline_chars: usize,
    /// Largest chunk the `memory_recall` tool returns at once.
    pub recall_tool_max_chars: usize,
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            hot_turns: 3,
            hot_chars: 8_000,
            digest_chars: 1_600,
            gist_chars: 180,
            recall_hits: 3,
            recall_chars: 1_800,
            tool_inline_chars: 3_000,
            recall_tool_max_chars: 6_000,
        }
    }
}

impl ContextBudget {
    /// Defaults with optional overrides: `SAIL_CONTEXT_HOT_TURNS`,
    /// `SAIL_CONTEXT_TOOL_INLINE_CHARS`, `SAIL_CONTEXT_RECALL_CHARS`.
    pub fn from_env() -> Self {
        let mut budget = Self::default();
        let read = |name: &str| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
        };
        if let Some(v) = read("SAIL_CONTEXT_HOT_TURNS") {
            budget.hot_turns = v.clamp(1, 20);
        }
        if let Some(v) = read("SAIL_CONTEXT_TOOL_INLINE_CHARS") {
            budget.tool_inline_chars = v.clamp(500, 64_000);
        }
        if let Some(v) = read("SAIL_CONTEXT_RECALL_CHARS") {
            budget.recall_chars = v.clamp(0, 16_000);
        }
        budget
    }
}

/// A turn that left the hot window but is still referenced from the prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct Breadcrumb {
    pub id: i64,
    pub turn: u64,
    pub gist: String,
}

/// An archived excerpt recalled for the current prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct Recall {
    pub id: i64,
    pub turn: Option<u64>,
    pub kind: String,
    pub excerpt: String,
}

/// What Arena contributes to one model request, already within budget.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkingContext {
    pub breadcrumbs: Vec<Breadcrumb>,
    pub recalled: Vec<Recall>,
}

impl WorkingContext {
    pub fn is_empty(&self) -> bool {
        self.breadcrumbs.is_empty() && self.recalled.is_empty()
    }

    /// Prompt lines, oldest breadcrumb first so the model reads the conversation in order.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for b in self.breadcrumbs.iter().rev() {
            out.push(format!("[mem:{} t{}] {}", b.id, b.turn, b.gist));
        }
        for r in &self.recalled {
            let at = r.turn.map(|t| format!(" t{t}")).unwrap_or_default();
            out.push(format!(
                "[mem:{}{at} {} excerpt] {}",
                r.id, r.kind, r.excerpt
            ));
        }
        out
    }

    pub fn chars(&self) -> usize {
        self.lines().iter().map(|l| l.chars().count()).sum()
    }
}

/// Keep at most `max` characters, cutting on a char boundary and marking the cut.
pub fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first sentence (or line) of `text`, whitespace squashed.
fn lead(text: &str) -> String {
    let text = squash(text);
    let end = text
        .char_indices()
        .find(|&(i, c)| {
            matches!(c, '.' | '!' | '?' | '\n')
                && text[i + c.len_utf8()..].starts_with(' ')
                && i > 12
        })
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(text.len());
    text[..end].to_string()
}

/// One-line extractive gist of an exchange: the user's ask and the start of the reply.
pub fn turn_gist(user: &str, reply: &str, max: usize) -> String {
    let user_part = clip(&squash(user), max * 2 / 5);
    let remaining = max.saturating_sub(user_part.chars().count() + 8);
    let reply_part = clip(&lead(reply), remaining.max(16));
    clip(&format!("U: {user_part} → S: {reply_part}"), max)
}

/// Gist for an archived tool result.
pub fn tool_gist(tool: &str, success: bool, body: &str, max: usize) -> String {
    let status = if success { "ok" } else { "failed" };
    clip(
        &format!(
            "{tool} {status}, {} chars: {}",
            body.chars().count(),
            squash(body)
        ),
        max,
    )
}

/// Replacement for an archived tool result: small, valid JSON the model can act on.
pub fn tool_stub(id: i64, tool: &str, success: bool, body: &str, budget: &ContextBudget) -> String {
    let total = body.chars().count();
    let head_chars = budget.tool_inline_chars * 2 / 3;
    let tail_chars = budget.tool_inline_chars / 6;
    let head: String = body.chars().take(head_chars).collect();
    let tail: String = body
        .chars()
        .skip(total.saturating_sub(tail_chars).max(head_chars))
        .collect();
    serde_json::json!({
        "tool": tool,
        "success": success,
        "archived": format!("mem:{id}"),
        "total_chars": total,
        "preview_head": head,
        "preview_tail": tail,
        "note": format!(
            "Only a preview is shown. Call memory_recall with id \"mem:{id}\" and a query to read the relevant parts, or an offset to page."
        ),
    })
    .to_string()
}

/// Fallback when the archive is unavailable: truncate honestly, without a handle.
pub fn tool_truncated(tool: &str, success: bool, body: &str, budget: &ContextBudget) -> String {
    serde_json::json!({
        "tool": tool,
        "success": success,
        "total_chars": body.chars().count(),
        "preview_head": body.chars().take(budget.tool_inline_chars).collect::<String>(),
        "note": "Result truncated; the memory archive is unavailable so the rest cannot be recalled.",
    })
    .to_string()
}

/// Parse `mem:123`, `[mem:123]` or `123`.
pub fn parse_handle(raw: &str) -> Option<i64> {
    let trimmed = raw.trim().trim_matches(|c| c == '[' || c == ']');
    trimmed
        .strip_prefix("mem:")
        .unwrap_or(trimmed)
        .trim()
        .parse()
        .ok()
}

/// The parts of `body` most relevant to `terms`, in document order, within `max` chars.
/// Splits on paragraph/line boundaries into ~`window` char chunks and scores by term hits.
pub fn relevant_windows(body: &str, terms: &[String], window: usize, max: usize) -> String {
    let chunks = chunk(body, window);
    if terms.is_empty() || chunks.len() <= 1 {
        return clip(body, max);
    }
    let mut scored: Vec<(usize, usize)> = chunks
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let lower = c.to_lowercase();
            (
                terms.iter().filter(|t| lower.contains(t.as_str())).count(),
                i,
            )
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    if scored.is_empty() {
        return clip(body, max);
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut picked = Vec::new();
    let mut used = 0;
    for (_, i) in scored {
        let len = chunks[i].chars().count() + 5;
        if used + len > max && !picked.is_empty() {
            continue;
        }
        used += len;
        picked.push(i);
    }
    picked.sort_unstable();
    let mut out = String::new();
    let mut last = None;
    for i in picked {
        if last.is_some_and(|l: usize| l + 1 != i) || (last.is_none() && i > 0) {
            out.push_str("\n…\n");
        }
        out.push_str(&chunks[i]);
        last = Some(i);
    }
    if last.is_some_and(|l| l + 1 < chunks.len()) {
        out.push_str("\n…");
    }
    clip(&out, max)
}

fn chunk(body: &str, window: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in body.split_inclusive('\n') {
        if !current.is_empty() && current.chars().count() + line.chars().count() > window {
            chunks.push(std::mem::take(&mut current));
        }
        if line.chars().count() > window {
            // A single huge line (minified JSON): hard-split it.
            let chars: Vec<char> = line.chars().collect();
            for piece in chars.chunks(window) {
                chunks.push(piece.iter().collect());
            }
            continue;
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gist_is_bounded_and_keeps_both_sides() {
        let user = "Can you check the Forge status of issue 125 and tell me whether tests passed? "
            .repeat(5);
        let reply = "Job 125 is ready to publish. All 42 tests passed and the review approved it. More detail follows.";
        let g = turn_gist(&user, reply, 180);
        assert!(g.chars().count() <= 180, "{g}");
        assert!(g.starts_with("U: Can you check the Forge"), "{g}");
        assert!(g.contains("→ S: Job 125 is ready"), "{g}");
    }

    #[test]
    fn tool_stub_is_small_valid_json_with_handle() {
        let budget = ContextBudget::default();
        let body = format!("{{\"diff\":\"{}\"}}", "+line\\n".repeat(5_000));
        let stub = tool_stub(42, "forge_status", true, &body, &budget);
        assert!(
            stub.len() < budget.tool_inline_chars + 600,
            "{}",
            stub.len()
        );
        let v: serde_json::Value = serde_json::from_str(&stub).unwrap();
        assert_eq!(v["archived"], "mem:42");
        assert_eq!(v["total_chars"], body.chars().count());
    }

    #[test]
    fn handles_parse_in_all_forms() {
        assert_eq!(parse_handle("mem:7"), Some(7));
        assert_eq!(parse_handle("[mem:7]"), Some(7));
        assert_eq!(parse_handle(" 7 "), Some(7));
        assert_eq!(parse_handle("job-7"), None);
    }

    #[test]
    fn relevant_windows_pick_matching_chunks_in_order() {
        let mut body = String::new();
        for i in 0..50 {
            body.push_str(&format!("line {i} filler text about nothing\n"));
        }
        body.push_str("fn evict_oldest(&mut self) { borrow error here }\n");
        for i in 50..100 {
            body.push_str(&format!("line {i} filler text about nothing\n"));
        }
        let out = relevant_windows(&body, &["evict_oldest".into()], 200, 600);
        assert!(out.contains("evict_oldest"), "{out}");
        assert!(out.chars().count() <= 600);
        assert!(out.starts_with("…\n"), "marks the skipped prefix: {out}");
    }

    #[test]
    fn working_context_lines_are_chronological() {
        let wc = WorkingContext {
            breadcrumbs: vec![
                Breadcrumb {
                    id: 9,
                    turn: 5,
                    gist: "newer".into(),
                },
                Breadcrumb {
                    id: 3,
                    turn: 2,
                    gist: "older".into(),
                },
            ],
            recalled: vec![Recall {
                id: 1,
                turn: Some(1),
                kind: "turn".into(),
                excerpt: "x".into(),
            }],
        };
        assert_eq!(
            wc.lines(),
            vec![
                "[mem:3 t2] older",
                "[mem:9 t5] newer",
                "[mem:1 t1 turn excerpt] x"
            ]
        );
    }
}
