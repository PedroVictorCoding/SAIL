//! Rust repository map for solver localization, after Aider's repo map.
//!
//! Item signatures are extracted line by line (no full parse), so the map still works
//! on code that does not compile, which is exactly the compile-fix case. Files are
//! ranked by how well their symbols and paths match the issue text, with the issue's
//! `## Target files` pinned first, and rendered under a character budget.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::admission::parse_target_files;

const SKIP_DIRS: &[&str] = &["target", ".git", ".claude", "node_modules", "vendor"];
const MAX_FILES: usize = 3_000;
const MAX_LINES_PER_FILE: usize = 2_000;
const ITEM_KEYWORDS: &[&str] = &[
    "fn ",
    "struct ",
    "enum ",
    "trait ",
    "impl ",
    "impl<",
    "mod ",
    "type ",
    "const ",
    "static ",
    "macro_rules!",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub line: usize,
    pub name: String,
    pub signature: String,
}

#[derive(Debug, Clone)]
pub struct FileSymbols {
    pub path: String,
    pub symbols: Vec<Symbol>,
}

fn strip_qualifiers(mut t: &str) -> &str {
    loop {
        let before = t;
        for q in [
            "pub(crate) ",
            "pub(super) ",
            "pub(self) ",
            "pub ",
            "async ",
            "unsafe ",
            "extern \"C\" ",
            "default ",
        ] {
            t = t.strip_prefix(q).unwrap_or(t);
        }
        // `const fn` is a function, not a constant.
        if let Some(rest) = t.strip_prefix("const ").filter(|r| r.starts_with("fn ")) {
            return rest;
        }
        if t == before {
            return t;
        }
    }
}

fn item_name(item: &str) -> String {
    let after_kw = item
        .split_once(' ')
        .map(|(_, r)| r)
        .unwrap_or(item)
        .trim_start_matches('<');
    if item.starts_with("impl") {
        // `impl<T> Trait for Type` -> "Trait for Type"; keep the readable part.
        let body = item.trim_start_matches("impl");
        let body = if body.starts_with('<') {
            body.find('>').map(|i| &body[i + 1..]).unwrap_or(body)
        } else {
            body
        };
        return body.split('{').next().unwrap_or("").trim().to_string();
    }
    after_kw
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '!'))
        .next()
        .unwrap_or("")
        .to_string()
}

/// Extract item signatures (`fn`, `struct`, `impl`, ...) with 1-based line numbers.
pub fn extract_signatures(content: &str) -> Vec<Symbol> {
    let mut out = Vec::new();
    for (i, raw) in content.lines().take(MAX_LINES_PER_FILE).enumerate() {
        let indent = raw.len() - raw.trim_start().len();
        if indent > 8 {
            continue;
        }
        let t = raw.trim();
        if t.starts_with("//") || t.starts_with('#') {
            continue;
        }
        let item = strip_qualifiers(t);
        if !ITEM_KEYWORDS.iter().any(|k| item.starts_with(k)) {
            continue;
        }
        let mut sig = t.split('{').next().unwrap_or(t).trim_end().to_string();
        if sig.chars().count() > 140 {
            sig = sig.chars().take(140).collect::<String>() + "…";
        }
        let name = item_name(item);
        if name.is_empty() {
            continue;
        }
        out.push(Symbol {
            line: i + 1,
            name,
            signature: sig,
        });
    }
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if out.len() >= MAX_FILES {
            return;
        }
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if ft.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                walk(root, &path, out);
            }
        } else if ft.is_file() && name.ends_with(".rs") {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().to_string());
            }
        }
    }
}

/// Scan all Rust files under `root` (symlinks are not followed).
pub fn scan_repo(root: &Path) -> Vec<FileSymbols> {
    let mut paths = Vec::new();
    walk(root, root, &mut paths);
    paths
        .into_iter()
        .filter_map(|p| {
            let content = fs::read_to_string(root.join(&p)).ok()?;
            Some(FileSymbols {
                symbols: extract_signatures(&content),
                path: p,
            })
        })
        .collect()
}

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "when", "must", "should", "not",
    "are", "fix", "add", "use", "returns", "return", "file", "test", "tests", "src", "crate",
    "cargo", "issue", "pub", "self", "true", "false", "none", "some", "error", "new",
];

/// Lowercased identifier parts from free text, splitting snake_case and CamelCase.
pub fn identifiers(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        if word.is_empty() {
            continue;
        }
        if word.len() >= 4 {
            out.insert(word.to_lowercase());
        }
        let mut part = String::new();
        let mut prev_lower = false;
        for ch in word.chars() {
            if ch == '_' || (ch.is_uppercase() && prev_lower) {
                if part.len() >= 3 {
                    out.insert(part.to_lowercase());
                }
                part.clear();
            }
            if ch != '_' {
                part.push(ch);
            }
            prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
        }
        if part.len() >= 3 {
            out.insert(part.to_lowercase());
        }
    }
    out.retain(|w| !STOPWORDS.contains(&w.as_str()));
    out
}

fn crate_of(path: &str) -> Option<&str> {
    path.strip_prefix("crates/")?.split('/').next()
}

fn is_target(path: &str, targets: &[String]) -> bool {
    targets
        .iter()
        .any(|t| path == t || (t.ends_with('/') && path.starts_with(t.as_str())))
}

/// Relevance of one file to the issue identifiers.
fn score(file: &FileSymbols, idents: &HashSet<String>, target_crates: &HashSet<&str>) -> u32 {
    let mut s = 0u32;
    for sym in &file.symbols {
        let name = sym.name.to_lowercase();
        if idents.contains(&name) {
            s += 10;
        } else {
            s += identifiers(&sym.name)
                .iter()
                .filter(|p| idents.contains(*p))
                .count() as u32;
        }
    }
    s += 2 * identifiers(&file.path.replace(['/', '.', '-'], " "))
        .iter()
        .filter(|p| idents.contains(*p))
        .count() as u32;
    if crate_of(&file.path).is_some_and(|c| target_crates.contains(c)) {
        s += 3;
    }
    s
}

/// Rank files: targets first (in issue order), then by descending score.
pub fn rank<'a>(
    files: &'a [FileSymbols],
    issue_text: &str,
    targets: &[String],
) -> Vec<&'a FileSymbols> {
    let idents = identifiers(issue_text);
    let target_crates: HashSet<&str> = targets.iter().filter_map(|t| crate_of(t)).collect();
    let mut pinned: Vec<&FileSymbols> = Vec::new();
    for t in targets {
        for f in files
            .iter()
            .filter(|f| is_target(&f.path, std::slice::from_ref(t)))
        {
            if !pinned.iter().any(|p| p.path == f.path) {
                pinned.push(f);
            }
        }
    }
    let mut scored: Vec<(u32, &FileSymbols)> = files
        .iter()
        .filter(|f| !pinned.iter().any(|p| p.path == f.path))
        .map(|f| (score(f, &idents, &target_crates), f))
        .filter(|(s, _)| *s > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));
    pinned
        .into_iter()
        .chain(scored.into_iter().map(|(_, f)| f))
        .collect()
}

/// Render ranked files with their signatures until `budget` characters are used.
pub fn render_map(ranked: &[&FileSymbols], budget: usize) -> String {
    let mut out = String::new();
    for f in ranked {
        let mut block = format!("{}:\n", f.path);
        for s in &f.symbols {
            block.push_str(&format!("{:5}: {}\n", s.line, s.signature));
        }
        if out.len() + block.len() > budget {
            // Fit at least the path so the model knows the file exists.
            let line = format!("{}: ({} items)\n", f.path, f.symbols.len());
            if out.len() + line.len() > budget {
                break;
            }
            out.push_str(&line);
            continue;
        }
        out.push_str(&block);
    }
    out
}

fn workspace_crates(root: &Path) -> Vec<String> {
    let mut crates: Vec<String> = fs::read_dir(root.join("crates"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().join("Cargo.toml").exists())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    crates.sort();
    crates
}

/// Initial solver context: crate list, numbered contents of the issue's target files,
/// and a ranked repo map, within `target_budget` and `map_budget` characters.
pub fn build_issue_context(
    root: &Path,
    issue_title: &str,
    issue_body: &str,
    target_budget: usize,
    map_budget: usize,
) -> String {
    let targets = parse_target_files(issue_body);
    let files = scan_repo(root);
    let mut out = String::new();

    let crates = workspace_crates(root);
    if !crates.is_empty() {
        out.push_str(&format!("Workspace crates: {}\n\n", crates.join(", ")));
    }

    let mut used = 0usize;
    let file_targets: Vec<&String> = targets.iter().filter(|t| !t.ends_with('/')).collect();
    if !file_targets.is_empty() {
        out.push_str("## Target files\n");
    }
    for t in file_targets {
        let Ok(content) = fs::read_to_string(root.join(t)) else {
            out.push_str(&format!("--- {t} (does not exist yet) ---\n"));
            continue;
        };
        let mut block = format!("--- File: {t} ---\n");
        for (i, line) in content.lines().enumerate() {
            block.push_str(&format!("{:4}: {}\n", i + 1, line));
        }
        if used + block.len() > target_budget {
            let remaining = target_budget.saturating_sub(used);
            let cut: String = block.chars().take(remaining).collect();
            out.push_str(&cut);
            out.push_str(&format!(
                "\n[{t} truncated; use repo:read_range for the rest]\n"
            ));
            used = target_budget;
        } else {
            used += block.len();
            out.push_str(&block);
        }
    }

    let ranked = rank(&files, &format!("{issue_title}\n{issue_body}"), &targets);
    let map = render_map(&ranked, map_budget);
    if !map.is_empty() {
        out.push_str("\n## Repository map (most relevant first; line: signature)\n");
        out.push_str(&map);
    } else if files.is_empty() {
        out.push_str("\n(no Rust files found)\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"use std::fmt;

pub struct Cache {
    items: Vec<u64>,
}

impl Cache {
    pub fn evict_oldest(&mut self) -> Option<u64> {
        let broken = ;
    }
    pub(crate) const fn capacity() -> usize { 4 }
}

impl<T: fmt::Debug> fmt::Display for Wrapper<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { Ok(()) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_evict() {}
}
"#;

    #[test]
    fn extracts_signatures_from_broken_code() {
        let syms = extract_signatures(SRC);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Cache"));
        assert!(names.contains(&"evict_oldest"));
        assert!(names.contains(&"capacity"));
        assert!(names.contains(&"fmt::Display for Wrapper<T>"));
        assert!(names.contains(&"tests"));
        let evict = syms.iter().find(|s| s.name == "evict_oldest").unwrap();
        assert_eq!(evict.line, 8);
        assert_eq!(
            evict.signature,
            "pub fn evict_oldest(&mut self) -> Option<u64>"
        );
    }

    #[test]
    fn identifiers_split_snake_and_camel() {
        let ids = identifiers("Fix EventEnvelope::matches_topic for audio_capture");
        for w in [
            "eventenvelope",
            "event",
            "envelope",
            "matches_topic",
            "matches",
            "topic",
            "audio",
            "capture",
        ] {
            assert!(ids.contains(w), "missing {w}: {ids:?}");
        }
        assert!(!ids.contains("fix"));
    }

    fn fs_of(path: &str, names: &[&str]) -> FileSymbols {
        FileSymbols {
            path: path.into(),
            symbols: names
                .iter()
                .enumerate()
                .map(|(i, n)| Symbol {
                    line: i + 1,
                    name: n.to_string(),
                    signature: format!("pub fn {n}()"),
                })
                .collect(),
        }
    }

    #[test]
    fn ranks_targets_then_symbol_matches() {
        let files = vec![
            fs_of("crates/a/src/noise.rs", &["unrelated"]),
            fs_of("crates/b/src/envelope.rs", &["matches_topic", "new"]),
            fs_of("crates/c/src/lib.rs", &["start"]),
        ];
        let ranked = rank(
            &files,
            "EventEnvelope::matches_topic matches audiobook wrongly",
            &["crates/c/src/lib.rs".to_string()],
        );
        let order: Vec<&str> = ranked.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            order,
            vec!["crates/c/src/lib.rs", "crates/b/src/envelope.rs"]
        );
    }

    #[test]
    fn render_respects_budget() {
        let big: Vec<String> = (0..200).map(|i| format!("function_{i}")).collect();
        let refs: Vec<&str> = big.iter().map(String::as_str).collect();
        let files = [fs_of("a.rs", &refs), fs_of("b.rs", &["x"])];
        let ranked: Vec<&FileSymbols> = files.iter().collect();
        let out = render_map(&ranked, 500);
        assert!(out.len() <= 500);
        assert!(out.contains("a.rs: (200 items)"));
        assert!(out.contains("b.rs:"));
    }

    #[test]
    fn context_includes_target_contents_and_skips_target_dir() {
        let root = std::env::temp_dir().join(format!("repo_map_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("src/cache.rs"), SRC).unwrap();
        fs::write(root.join("src/other.rs"), "pub fn evict_all() {}\n").unwrap();
        fs::write(
            root.join("target/debug/gen.rs"),
            "pub fn evict_generated() {}\n",
        )
        .unwrap();
        let ctx = build_issue_context(
            &root,
            "Fix evict_oldest",
            "## Target files\n- `src/cache.rs`\n",
            12_000,
            6_000,
        );
        assert!(ctx.contains("--- File: src/cache.rs ---"));
        assert!(ctx.contains("   8:     pub fn evict_oldest"));
        assert!(ctx.contains("src/other.rs:"));
        assert!(!ctx.contains("gen.rs"));
        let _ = fs::remove_dir_all(&root);
    }
}
