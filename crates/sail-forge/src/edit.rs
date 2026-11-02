//! SEARCH/REPLACE edits for the local solver.
//!
//! Unified diffs fail often with small local models (hunk headers, exact context). This
//! module accepts Aider-style blocks instead:
//!
//! ```text
//! src/lib.rs
//! <<<<<<< SEARCH
//! old lines
//! =======
//! new lines
//! >>>>>>> REPLACE
//! ```
//!
//! Matching tries an exact unique match, then ignores trailing whitespace, then ignores
//! indentation (re-indenting the replacement). All blocks in one reply apply atomically,
//! and every write goes through `IsolatedWorktree::write_file` (safe path resolution).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::worktree::IsolatedWorktree;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditBlock {
    pub path: String,
    pub search: String,
    pub replace: String,
}

fn marker(line: &str, ch: char, word: &str) -> bool {
    let t = line.trim();
    let n = t.chars().take_while(|c| *c == ch).count();
    n >= 5 && t[n..].trim() == word
}

/// Clean a path line such as "`src/lib.rs`", "File: src/lib.rs" or "src/lib.rs:".
fn clean_path(line: &str) -> Option<String> {
    let mut t = line.trim().trim_start_matches('#').trim();
    for prefix in ["File:", "file:", "Path:", "path:"] {
        t = t.strip_prefix(prefix).unwrap_or(t).trim();
    }
    let t = t
        .trim_matches('`')
        .trim_matches('*')
        .trim_end_matches(':')
        .trim();
    let looks_like_path = !t.is_empty()
        && !t.contains(' ')
        && !t.starts_with("```")
        && (t.contains('/') || t.contains('.'));
    looks_like_path.then(|| t.trim_start_matches("./").to_string())
}

/// Extract all SEARCH/REPLACE blocks from a model reply. A block without a preceding
/// path line reuses the previous block's path.
pub fn parse_edit_blocks(raw: &str) -> Vec<EditBlock> {
    let lines: Vec<&str> = raw.lines().collect();
    let mut blocks = Vec::new();
    let mut last_path: Option<String> = None;
    let mut i = 0;
    while i < lines.len() {
        if !marker(lines[i], '<', "SEARCH") {
            i += 1;
            continue;
        }
        let path = lines[..i]
            .iter()
            .rev()
            .take(3)
            .find_map(|l| clean_path(l))
            .or_else(|| last_path.clone());
        let mut j = i + 1;
        let mut search = Vec::new();
        while j < lines.len() && !marker(lines[j], '=', "") {
            search.push(lines[j]);
            j += 1;
        }
        let mut k = j + 1;
        let mut replace = Vec::new();
        while k < lines.len() && !marker(lines[k], '>', "REPLACE") {
            replace.push(lines[k]);
            k += 1;
        }
        if let (Some(path), true) = (path, k < lines.len()) {
            last_path = Some(path.clone());
            blocks.push(EditBlock {
                path,
                search: search.join("\n"),
                replace: replace.join("\n"),
            });
        }
        i = k + 1;
    }
    blocks
}

fn indent_of(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

fn numbered(lines: &[&str], start: usize, end: usize) -> String {
    (start..end.min(lines.len()))
        .map(|i| format!("{:4}: {}", i + 1, lines[i]))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Line windows of `content` equal to `search` under `eq`.
fn find_windows(content: &[&str], search: &[&str], eq: impl Fn(&str, &str) -> bool) -> Vec<usize> {
    if search.is_empty() || search.len() > content.len() {
        return Vec::new();
    }
    (0..=content.len() - search.len())
        .filter(|&s| {
            search
                .iter()
                .enumerate()
                .all(|(o, l)| eq(content[s + o], l))
        })
        .collect()
}

/// Re-indent `replace` by the indentation difference between the matched file lines and
/// the model's SEARCH lines.
fn reindent(replace: &[&str], file_indent: &str, search_indent: &str) -> Vec<String> {
    replace
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else if let Some(rest) = l.strip_prefix(search_indent) {
                format!("{file_indent}{rest}")
            } else {
                format!("{file_indent}{}", l.trim_start())
            }
        })
        .collect()
}

/// Apply one SEARCH/REPLACE to `content`, or explain precisely why it cannot apply.
pub fn apply_block(content: &str, block: &EditBlock) -> Result<String, String> {
    if block.search.trim().is_empty() {
        return Err(format!(
            "{}: SEARCH is empty but the file exists; include the exact lines to replace",
            block.path
        ));
    }

    // 1. Exact, unique substring.
    let exact = content.matches(block.search.as_str()).count();
    if exact == 1 {
        return Ok(content.replacen(&block.search, &block.replace, 1));
    }

    let file: Vec<&str> = content.lines().collect();
    let search: Vec<&str> = block.search.lines().collect();
    let replace: Vec<&str> = block.replace.lines().collect();
    let trailing_newline = content.ends_with('\n');
    let rebuild = |start: usize, new_lines: Vec<String>| {
        let mut out: Vec<String> = file[..start].iter().map(|s| s.to_string()).collect();
        out.extend(new_lines);
        out.extend(file[start + search.len()..].iter().map(|s| s.to_string()));
        let mut s = out.join("\n");
        if trailing_newline {
            s.push('\n');
        }
        s
    };

    let ambiguous = |starts: &[usize]| {
        let at: Vec<String> = starts.iter().map(|s| (s + 1).to_string()).collect();
        format!(
            "{}: SEARCH matches {} places (lines {}); add surrounding lines so it is unique",
            block.path,
            starts.len(),
            at.join(", ")
        )
    };
    if exact > 1 {
        let starts = find_windows(&file, &search, |a, b| a == b);
        return Err(ambiguous(&starts));
    }

    // 2. Ignore trailing whitespace.
    let starts = find_windows(&file, &search, |a, b| a.trim_end() == b.trim_end());
    if starts.len() == 1 {
        return Ok(rebuild(
            starts[0],
            replace.iter().map(|s| s.to_string()).collect(),
        ));
    }
    if starts.len() > 1 {
        return Err(ambiguous(&starts));
    }

    // 3. Ignore indentation, then shift the replacement to the file's indentation.
    let starts = find_windows(&file, &search, |a, b| a.trim() == b.trim());
    if starts.len() == 1 {
        let start = starts[0];
        let offset = search
            .iter()
            .position(|l| !l.trim().is_empty())
            .unwrap_or(0);
        let file_indent = indent_of(file[start + offset]);
        let search_indent = indent_of(search[offset]);
        return Ok(rebuild(
            start,
            reindent(&replace, file_indent, search_indent),
        ));
    }
    if starts.len() > 1 {
        return Err(ambiguous(&starts));
    }

    // No match: point at the most likely location.
    let anchor = search
        .iter()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim());
    let hint = anchor
        .and_then(|a| file.iter().position(|l| l.trim() == a || l.contains(a)))
        .map(|pos| {
            format!(
                "\nClosest match for the first SEARCH line is at line {}:\n{}",
                pos + 1,
                numbered(&file, pos.saturating_sub(3), pos + search.len() + 3)
            )
        })
        .unwrap_or_else(|| {
            "\nThe first SEARCH line does not occur in the file; read it with repo:read_range first.".into()
        });
    Err(format!(
        "{}: SEARCH text not found (SEARCH must copy existing lines exactly).{hint}",
        block.path
    ))
}

/// Apply all blocks atomically. Returns the modified paths, or an error naming the
/// first failing block; on error nothing is written.
pub fn apply_edit_blocks(
    worktree: &IsolatedWorktree,
    blocks: &[EditBlock],
) -> Result<Vec<String>, String> {
    if blocks.is_empty() {
        return Err("no SEARCH/REPLACE blocks found".into());
    }
    let mut staged: BTreeMap<String, String> = BTreeMap::new();
    for (n, block) in blocks.iter().enumerate() {
        let current = match staged.get(&block.path) {
            Some(c) => Some(c.clone()),
            None => match worktree.read_file(Path::new(&block.path)) {
                Ok(c) => Some(c),
                Err(_) => None,
            },
        };
        let updated = match current {
            Some(content) => apply_block(&content, block),
            None if block.search.trim().is_empty() => {
                let mut c = block.replace.clone();
                if !c.ends_with('\n') {
                    c.push('\n');
                }
                Ok(c)
            }
            None => Err(format!(
                "{}: file does not exist (use an empty SEARCH to create it)",
                block.path
            )),
        }
        .map_err(|e| format!("block {} of {}: {e}", n + 1, blocks.len()))?;
        staged.insert(block.path.clone(), updated);
    }
    for (path, content) in &staged {
        worktree
            .write_file(Path::new(path), content)
            .map_err(|e| format!("{path}: write rejected: {e}"))?;
    }
    Ok(staged.into_keys().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(path: &str, search: &str, replace: &str) -> EditBlock {
        EditBlock {
            path: path.into(),
            search: search.into(),
            replace: replace.into(),
        }
    }

    #[test]
    fn parses_blocks_with_fences_prose_and_path_reuse() {
        let raw = "I'll fix it.\n\n`src/lib.rs`\n```rust\n<<<<<<< SEARCH\nfn a() {}\n=======\nfn a() { 1 }\n>>>>>>> REPLACE\n```\n\n```rust\n<<<<<<< SEARCH\nfn b() {}\n=======\nfn b() { 2 }\n>>>>>>> REPLACE\n```\nFile: src/new.rs\n<<<<<<< SEARCH\n=======\npub fn c() {}\n>>>>>>> REPLACE\n";
        let blocks = parse_edit_blocks(raw);
        assert_eq!(
            blocks,
            vec![
                block("src/lib.rs", "fn a() {}", "fn a() { 1 }"),
                block("src/lib.rs", "fn b() {}", "fn b() { 2 }"),
                block("src/new.rs", "", "pub fn c() {}"),
            ]
        );
    }

    #[test]
    fn unterminated_block_is_ignored() {
        assert!(parse_edit_blocks("src/a.rs\n<<<<<<< SEARCH\nx\n=======\ny\n").is_empty());
    }

    #[test]
    fn exact_match_preserves_everything_else() {
        let c = "a\n  b\nc\n";
        assert_eq!(
            apply_block(c, &block("f", "  b", "  B")).unwrap(),
            "a\n  B\nc\n"
        );
    }

    #[test]
    fn trailing_whitespace_is_tolerated() {
        let c = "fn x() {   \n    1\n}\n";
        let out = apply_block(c, &block("f", "fn x() {\n    1\n}", "fn x() {\n    2\n}")).unwrap();
        assert_eq!(out, "fn x() {\n    2\n}\n");
    }

    #[test]
    fn indentation_shift_is_reindented() {
        let c = "impl A {\n    fn f(&self) -> u8 {\n        1\n    }\n}\n";
        // Model dropped the 4-space impl indentation.
        let out = apply_block(
            c,
            &block(
                "f",
                "fn f(&self) -> u8 {\n    1\n}",
                "fn f(&self) -> u8 {\n    let v = 2;\n    v\n}",
            ),
        )
        .unwrap();
        assert_eq!(
            out,
            "impl A {\n    fn f(&self) -> u8 {\n        let v = 2;\n        v\n    }\n}\n"
        );
    }

    #[test]
    fn ambiguous_match_lists_lines() {
        let err = apply_block("x = 1;\ny\nx = 1;\n", &block("f", "x = 1;", "x = 2;")).unwrap_err();
        assert!(err.contains("2 places"), "{err}");
        assert!(err.contains("lines 1, 3"), "{err}");
    }

    #[test]
    fn missing_match_points_at_closest_line() {
        let c = "fn a() {}\nfn compute() -> i32 {\n    41\n}\n";
        let err = apply_block(c, &block("f", "fn compute() -> i32 {\n    40\n}", "x")).unwrap_err();
        assert!(err.contains("not found"), "{err}");
        assert!(err.contains("line 2"), "{err}");
        assert!(err.contains("   3:     41"), "{err}");
    }

    #[test]
    fn empty_search_on_existing_file_is_rejected() {
        assert!(apply_block("a\n", &block("f", "", "b")).is_err());
    }
}
