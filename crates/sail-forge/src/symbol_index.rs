//! Symbol-level code retrieval for the solver (#119).
//!
//! `repo:map` and `repo:search` match text. This module answers "where is this function
//! defined?" and "who calls it?" from a real semantic index: `rust-analyzer scip` runs
//! inside the job's sandbox and writes a SCIP index of the worktree, which is parsed here.
//!
//! Tools exposed to the model:
//! - `code:find_symbol {"name": "parse_target_files"}`: definitions whose name matches, with
//!   signature, location, reference count and the first documentation line.
//! - `code:find_references {"name": "parse_target_files"}`: every place the best match is
//!   used, grouped by file, with test files marked.
//!
//! The index is built once per worktree, on the first `code:*` call (about 2 minutes for the
//! SAIL workspace on 4 cores). Edits after that are not re-indexed: results in files changed
//! since indexing are flagged so the model re-reads them. When rust-analyzer is missing or
//! fails, the tools say so and point to `repo:search`; nothing else changes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use protobuf::Message;

use crate::worktree::IsolatedWorktree;

/// A position in the worktree (1-based line).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Location {
    pub path: String,
    pub line: u32,
}

/// One defined symbol.
#[derive(Debug, Clone)]
pub struct SymbolInfo {
    /// SCIP symbol string, unique within the index.
    pub symbol: String,
    /// Short name, for example `parse_target_files`.
    pub name: String,
    /// Path-like qualifier, for example `admission/parse_target_files().`.
    pub descriptor: String,
    pub signature: Option<String>,
    pub doc: Option<String>,
    pub definition: Option<Location>,
}

/// Parsed index: definitions and references by symbol.
#[derive(Debug, Default)]
pub struct SymbolIndex {
    symbols: HashMap<String, SymbolInfo>,
    by_name: HashMap<String, Vec<String>>,
    references: HashMap<String, Vec<Location>>,
    /// Files that differed from the base commit when the index was built, with their diff.
    indexed_diff: String,
    pub build_secs: f64,
}

impl SymbolIndex {
    /// Parse a SCIP index (protobuf bytes).
    pub fn from_scip_bytes(bytes: &[u8]) -> Result<Self, String> {
        let index = scip::types::Index::parse_from_bytes(bytes)
            .map_err(|e| format!("invalid SCIP index: {e}"))?;
        let mut out = SymbolIndex::default();
        for doc in &index.documents {
            for info in &doc.symbols {
                if info.symbol.starts_with("local ") || info.symbol.is_empty() {
                    continue;
                }
                let descriptor = descriptor_of(&info.symbol);
                let name = if info.display_name.is_empty() {
                    name_of(&descriptor)
                } else {
                    info.display_name.clone()
                };
                if name.is_empty() {
                    continue;
                }
                let signature = info
                    .signature_documentation
                    .as_ref()
                    .map(|d| d.text.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .or_else(|| {
                        // rust-analyzer puts the signature in a fenced block in the docs.
                        info.documentation.first().and_then(|d| fenced_code(d))
                    });
                let doc_line = info
                    .documentation
                    .iter()
                    .flat_map(|d| strip_fences(d))
                    .find(|l| !l.trim().is_empty())
                    .map(|l| l.trim().to_string());
                out.by_name
                    .entry(name.to_ascii_lowercase())
                    .or_default()
                    .push(info.symbol.clone());
                out.symbols.insert(
                    info.symbol.clone(),
                    SymbolInfo {
                        symbol: info.symbol.clone(),
                        name,
                        descriptor,
                        signature,
                        doc: doc_line,
                        definition: None,
                    },
                );
            }
        }
        for doc in &index.documents {
            for occ in &doc.occurrences {
                if occ.symbol.starts_with("local ") || occ.range.is_empty() {
                    continue;
                }
                let loc = Location {
                    path: doc.relative_path.clone(),
                    line: occ.range[0].max(0) as u32 + 1,
                };
                let is_definition =
                    occ.symbol_roles & scip::types::SymbolRole::Definition as i32 != 0;
                if is_definition {
                    if let Some(info) = out.symbols.get_mut(&occ.symbol) {
                        info.definition.get_or_insert(loc);
                    }
                } else {
                    out.references
                        .entry(occ.symbol.clone())
                        .or_default()
                        .push(loc);
                }
            }
        }
        for refs in out.references.values_mut() {
            refs.sort();
            refs.dedup();
        }
        for list in out.by_name.values_mut() {
            list.sort();
            list.dedup();
        }
        Ok(out)
    }

    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    /// Definitions matching `query`: exact name, then a `::`/`/` path suffix, then substring.
    /// Symbols defined in the worktree come first; dependencies after.
    pub fn find(&self, query: &str, limit: usize) -> Vec<&SymbolInfo> {
        let q = query.trim().trim_end_matches("()").to_ascii_lowercase();
        let last = q.rsplit([':', '/', '.']).next().unwrap_or(&q).to_string();
        let qualifier = q.replace("::", "/");
        let mut hits: Vec<(u8, &SymbolInfo)> = Vec::new();
        let mut seen = HashSet::new();
        if let Some(exact) = self.by_name.get(&last) {
            for s in exact {
                let info = &self.symbols[s];
                let rank = if qualifier.contains('/')
                    && !info.descriptor.to_ascii_lowercase().contains(&qualifier)
                {
                    1
                } else {
                    0
                };
                hits.push((rank, info));
                seen.insert(s.as_str());
            }
        }
        if hits.len() < limit && last.len() >= 3 {
            let mut partial: Vec<&SymbolInfo> = self
                .symbols
                .values()
                .filter(|i| !seen.contains(i.symbol.as_str()))
                .filter(|i| i.name.to_ascii_lowercase().contains(&last))
                .collect();
            partial.sort_by(|a, b| {
                a.name
                    .len()
                    .cmp(&b.name.len())
                    .then(a.symbol.cmp(&b.symbol))
            });
            hits.extend(partial.into_iter().map(|i| (2, i)));
        }
        hits.sort_by(|(ra, a), (rb, b)| {
            ra.cmp(rb)
                .then(a.definition.is_none().cmp(&b.definition.is_none()))
                .then(a.symbol.cmp(&b.symbol))
        });
        hits.into_iter().take(limit).map(|(_, i)| i).collect()
    }

    pub fn references(&self, symbol: &str) -> &[Location] {
        self.references
            .get(symbol)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// The descriptor part of a SCIP symbol: `<scheme> <manager> <package> <version> <descriptors>`.
fn descriptor_of(symbol: &str) -> String {
    symbol.splitn(5, ' ').nth(4).unwrap_or(symbol).to_string()
}

/// Last descriptor name: `admission/parse_target_files().` -> `parse_target_files`.
fn name_of(descriptor: &str) -> String {
    let trimmed = descriptor.trim_end_matches(['.', '#', '/', ':', '(', ')', '!', ']']);
    let trimmed = trimmed.trim_end_matches(['.', '#', '/', ':', '(', ')']);
    trimmed
        .rsplit(['/', '#', '.', ':', '['])
        .next()
        .unwrap_or("")
        .trim_matches('`')
        .to_string()
}

fn fenced_code(doc: &str) -> Option<String> {
    let mut inside = false;
    let mut out = Vec::new();
    for line in doc.lines() {
        if line.trim_start().starts_with("```") {
            if inside {
                break;
            }
            inside = true;
            continue;
        }
        if inside {
            out.push(line);
        }
    }
    let text = out.join("\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn strip_fences(doc: &str) -> Vec<&str> {
    let mut inside = false;
    doc.lines()
        .filter(|l| {
            if l.trim_start().starts_with("```") {
                inside = !inside;
                return false;
            }
            !inside
        })
        .collect()
}

fn is_test_path(path: &str) -> bool {
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.ends_with("_test.rs")
        || path.ends_with("/tests.rs")
}

/// Files changed in `diff` (unified diff, `+++ b/` headers).
fn files_in_diff(diff: &str) -> HashSet<String> {
    diff.lines()
        .filter_map(|l| l.strip_prefix("+++ b/"))
        .map(|p| p.trim().to_string())
        .collect()
}

/// Per-file hunks, to tell which files changed again after indexing.
fn per_file_diff(diff: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in diff.lines() {
        if let Some(p) = line.strip_prefix("+++ b/") {
            current = Some(p.trim().to_string());
            continue;
        }
        if let Some(f) = &current {
            let e: &mut String = out.entry(f.clone()).or_default();
            e.push_str(line);
            e.push('\n');
        }
    }
    out
}

/// Files whose content may differ from what was indexed.
fn stale_files(indexed_diff: &str, current_diff: &str) -> HashSet<String> {
    let before = per_file_diff(indexed_diff);
    let after = per_file_diff(current_diff);
    files_in_diff(indexed_diff)
        .union(&files_in_diff(current_diff))
        .filter(|f| before.get(*f) != after.get(*f))
        .cloned()
        .collect()
}

const MAX_FIND_RESULTS: usize = 8;
const MAX_REFERENCES: usize = 40;
const MAX_OUTPUT_CHARS: usize = 6_000;

fn clip(mut s: String) -> String {
    if s.chars().count() > MAX_OUTPUT_CHARS {
        s = s.chars().take(MAX_OUTPUT_CHARS).collect();
        s.push_str("\n... (output truncated)");
    }
    s
}

/// Render `code:find_symbol` output.
pub fn render_find(index: &SymbolIndex, query: &str, stale: &HashSet<String>) -> String {
    let hits = index.find(query, MAX_FIND_RESULTS);
    if hits.is_empty() {
        return format!("No symbol named `{query}` in the index; try repo:search");
    }
    let mut out = String::new();
    for h in hits {
        let loc = match &h.definition {
            Some(l) => {
                let flag = if stale.contains(&l.path) {
                    " (file changed since indexing; re-read it)"
                } else {
                    ""
                };
                format!("{}:{}{flag}", l.path, l.line)
            }
            None => "defined outside the worktree".to_string(),
        };
        out.push_str(&format!(
            "{} — {}  [{} references]\n",
            h.name,
            loc,
            index.references(&h.symbol).len()
        ));
        if let Some(sig) = &h.signature {
            for l in sig.lines().take(4) {
                out.push_str("    ");
                out.push_str(l);
                out.push('\n');
            }
        }
        if let Some(doc) = &h.doc {
            out.push_str(&format!("    /// {doc}\n"));
        }
    }
    clip(out)
}

/// Render `code:find_references` output for the best match of `query`.
pub fn render_references(index: &SymbolIndex, query: &str, stale: &HashSet<String>) -> String {
    let Some(best) = index.find(query, 1).into_iter().next() else {
        return format!("No symbol named `{query}` in the index; try repo:search");
    };
    let refs = index.references(&best.symbol);
    let mut out = format!(
        "{} ({}): {} references\n",
        best.name,
        best.definition
            .as_ref()
            .map(|l| format!("defined at {}:{}", l.path, l.line))
            .unwrap_or_else(|| "defined outside the worktree".into()),
        refs.len()
    );
    let mut by_file: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
    for r in refs.iter().take(MAX_REFERENCES) {
        by_file.entry(&r.path).or_default().push(r.line);
    }
    for (file, lines) in by_file {
        let mut tags = Vec::new();
        if is_test_path(file) {
            tags.push("test");
        }
        if stale.contains(file) {
            tags.push("changed since indexing");
        }
        let tag = if tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", tags.join(", "))
        };
        let lines: Vec<String> = lines.iter().map(u32::to_string).collect();
        out.push_str(&format!("{file}{tag}: lines {}\n", lines.join(", ")));
    }
    if refs.len() > MAX_REFERENCES {
        out.push_str(&format!("... and {} more\n", refs.len() - MAX_REFERENCES));
    }
    clip(out)
}

type CachedIndex = Result<Arc<SymbolIndex>, String>;

/// Indexes built this process, by worktree root. Small: a job uses one worktree.
static CACHE: Mutex<Vec<(PathBuf, CachedIndex)>> = Mutex::new(Vec::new());
const CACHE_SLOTS: usize = 4;

/// Relative path (inside the worktree) the index is written to.
pub const INDEX_REL_PATH: &str = "target/sail-forge/index.scip";

/// Get or build the index for `worktree`. A failed build is remembered, so a missing
/// rust-analyzer costs one attempt per worktree, not one per tool call.
pub fn index_for(worktree: &IsolatedWorktree, timeout_secs: u64) -> CachedIndex {
    let root = worktree.root_dir.clone();
    if let Some((_, cached)) = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(r, _)| *r == root)
    {
        return cached.clone();
    }
    let built = build(worktree, timeout_secs).map(Arc::new);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= CACHE_SLOTS {
        drop(cache.remove(0));
    }
    cache.push((root, built.clone()));
    built
}

/// Run `rust-analyzer scip` in the sandbox and parse the result.
pub fn build(worktree: &IsolatedWorktree, timeout_secs: u64) -> Result<SymbolIndex, String> {
    if !crate_binary_available("rust-analyzer") {
        return Err("rust-analyzer is not installed".into());
    }
    let out_path = worktree.root_dir.join(INDEX_REL_PATH);
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(&out_path);
    let diff_at_build = worktree.get_diff().unwrap_or_default();
    let start = Instant::now();
    let out = worktree
        .run_command(
            "rust-analyzer",
            &["scip", ".", "--output", INDEX_REL_PATH],
            timeout_secs,
        )
        .map_err(|e| format!("rust-analyzer did not run: {e}"))?;
    if out.status != 0 {
        let tail: Vec<&str> = out.stderr.lines().rev().take(4).collect();
        return Err(format!(
            "rust-analyzer exited with {}: {}",
            out.status,
            tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
        ));
    }
    let bytes = std::fs::read(&out_path).map_err(|e| format!("no index written: {e}"))?;
    let mut index = SymbolIndex::from_scip_bytes(&bytes)?;
    index.indexed_diff = diff_at_build;
    index.build_secs = start.elapsed().as_secs_f64();
    Ok(index)
}

fn crate_binary_available(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
        .unwrap_or(false)
        || std::env::var_os("HOME")
            .map(|h| Path::new(&h).join(".cargo/bin").join(name).is_file())
            .unwrap_or(false)
}

/// Default time allowed to build the index (`SAIL_FORGE_SYMBOL_INDEX_TIMEOUT_SECS`).
pub fn build_timeout_secs() -> u64 {
    std::env::var("SAIL_FORGE_SYMBOL_INDEX_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| (30..=3600).contains(v))
        .unwrap_or(600)
}

/// Run a `code:*` tool. Returns `(ok, output)`.
pub fn run_tool(worktree: &IsolatedWorktree, references: bool, name: &str) -> (bool, String) {
    if name.trim().is_empty() {
        return (
            false,
            "Give a symbol name, for example {\"name\": \"parse_target_files\"}".into(),
        );
    }
    let index = match index_for(worktree, build_timeout_secs()) {
        Ok(i) => i,
        Err(e) => {
            return (
                false,
                format!("Symbol index unavailable ({e}); use repo:search or repo:map instead"),
            );
        }
    };
    let current = worktree.get_diff().unwrap_or_default();
    let stale = stale_files(&index.indexed_diff, &current);
    let text = if references {
        render_references(&index, name, &stale)
    } else {
        render_find(&index, name, &stale)
    };
    (true, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scip::types::{Document, Index, Occurrence, SymbolInformation, SymbolRole};

    fn sym(desc: &str) -> String {
        format!("rust-analyzer cargo demo 0.1.0 {desc}")
    }

    fn occ(symbol: &str, line: i32, def: bool) -> Occurrence {
        Occurrence {
            range: vec![line, 4, 10],
            symbol: symbol.into(),
            symbol_roles: if def {
                SymbolRole::Definition as i32
            } else {
                0
            },
            ..Default::default()
        }
    }

    fn sample() -> Vec<u8> {
        let add = sym("math/add().");
        let addr = sym("net/address().");
        let mut lib = Document {
            relative_path: "src/math.rs".into(),
            ..Default::default()
        };
        lib.symbols.push(SymbolInformation {
            symbol: add.clone(),
            documentation: vec![
                "```rust\npub fn add(a: i32, b: i32) -> i32\n```\n\nAdds two numbers.".into(),
            ],
            ..Default::default()
        });
        lib.symbols.push(SymbolInformation {
            symbol: addr.clone(),
            ..Default::default()
        });
        lib.occurrences.push(occ(&add, 2, true));
        lib.occurrences.push(occ(&addr, 9, true));
        lib.occurrences.push(occ("local 3", 4, false));
        let mut test = Document {
            relative_path: "tests/math_test.rs".into(),
            ..Default::default()
        };
        test.occurrences.push(occ(&add, 5, false));
        test.occurrences.push(occ(&add, 5, false));
        test.occurrences.push(occ(&add, 11, false));
        let index = Index {
            documents: vec![lib, test],
            ..Default::default()
        };
        index.write_to_bytes().unwrap()
    }

    #[test]
    fn parses_definitions_references_and_docs() {
        let idx = SymbolIndex::from_scip_bytes(&sample()).unwrap();
        assert_eq!(idx.len(), 2);
        let hit = idx.find("add", 5);
        assert_eq!(hit[0].name, "add");
        assert_eq!(
            hit[0].definition,
            Some(Location {
                path: "src/math.rs".into(),
                line: 3
            })
        );
        assert_eq!(
            hit[0].signature.as_deref(),
            Some("pub fn add(a: i32, b: i32) -> i32")
        );
        assert_eq!(hit[0].doc.as_deref(), Some("Adds two numbers."));
        assert_eq!(
            idx.references(&hit[0].symbol).len(),
            2,
            "duplicates collapse"
        );
    }

    #[test]
    fn exact_names_rank_before_substrings() {
        let idx = SymbolIndex::from_scip_bytes(&sample()).unwrap();
        let names: Vec<&str> = idx.find("add", 5).iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["add", "address"]);
        assert_eq!(idx.find("math::add()", 5)[0].name, "add");
        assert!(idx.find("zz", 5).is_empty());
    }

    #[test]
    fn rendering_marks_tests_and_stale_files() {
        let idx = SymbolIndex::from_scip_bytes(&sample()).unwrap();
        let stale: HashSet<String> = ["src/math.rs".to_string()].into();
        let find = render_find(&idx, "add", &stale);
        assert!(
            find.contains("add — src/math.rs:3 (file changed since indexing"),
            "{find}"
        );
        assert!(find.contains("[2 references]"));
        let refs = render_references(&idx, "add", &HashSet::new());
        assert!(
            refs.contains("tests/math_test.rs [test]: lines 6, 12"),
            "{refs}"
        );
        assert!(render_find(&idx, "nothing_here", &stale).contains("try repo:search"));
    }

    #[test]
    fn stale_files_are_those_changed_after_indexing() {
        let a = "+++ b/src/a.rs\n@@\n+x\n";
        let ab = "+++ b/src/a.rs\n@@\n+x\n+++ b/src/b.rs\n@@\n+y\n";
        let a2 = "+++ b/src/a.rs\n@@\n+z\n";
        assert!(stale_files(a, a).is_empty());
        assert_eq!(stale_files(a, ab), ["src/b.rs".to_string()].into());
        assert_eq!(stale_files(a, a2), ["src/a.rs".to_string()].into());
        assert_eq!(stale_files(a, ""), ["src/a.rs".to_string()].into());
    }

    #[test]
    fn names_come_from_descriptors() {
        assert_eq!(
            name_of("admission/parse_target_files()."),
            "parse_target_files"
        );
        assert_eq!(name_of("worktree/IsolatedWorktree#"), "IsolatedWorktree");
        assert_eq!(
            name_of("worktree/IsolatedWorktree#run_command()."),
            "run_command"
        );
        assert_eq!(name_of("gates/"), "gates");
    }

    #[test]
    fn rejects_garbage() {
        assert!(SymbolIndex::from_scip_bytes(b"\xff\xff\xff not protobuf").is_err());
    }
}
