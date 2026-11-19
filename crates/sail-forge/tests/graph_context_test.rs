//! Graphify code-graph context for the local solver (`graph.rs`).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sail_forge::graph::{Graphify, OUT_DIR};
use sail_forge::repo_map::{build_issue_context, build_issue_context_with};
use sail_forge::solver::{RepoToolCall, parse_model_action, solver_system_prompt};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sail-graph-ctx-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("crates/demo/src")).unwrap();
    std::fs::write(
        dir.join("crates/demo/Cargo.toml"),
        "[package]\nname = \"demo\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("crates/demo/src/cache.rs"),
        "pub struct Cache;\nimpl Cache {\n    pub fn evict_oldest(&mut self) {}\n}\n",
    )
    .unwrap();
    dir
}

/// Fake graphify that records its arguments and answers queries; `fail` makes queries exit 1.
fn fake(dir: &Path, fail: bool) -> Graphify {
    let bin = dir.join("fake-graphify");
    let query = if fail {
        "echo boom >&2; exit 1"
    } else {
        "echo \"NODE evict_oldest() [src=crates/demo/src/cache.rs loc=L3]\"; echo \"$*\" > args.txt"
    };
    std::fs::write(
        &bin,
        format!("#!/bin/sh\ncase \"$1\" in query) {query} ;; esac\n"),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(dir.join(OUT_DIR)).unwrap();
    std::fs::write(dir.join(OUT_DIR).join("graph.json"), "{}").unwrap();
    Graphify::with_binary(bin)
}

const BODY: &str = "Eviction is wrong.\n\n## Target files\n- crates/demo/src/cache.rs\n";

#[test]
fn graph_replaces_the_repo_map_and_seeds_from_target_files() {
    let dir = scratch("seeded");
    let graph = fake(&dir, false);
    let ctx =
        build_issue_context_with(&dir, "Fix cache eviction", BODY, 8_000, 4_000, Some(&graph));

    assert!(ctx.contains("## Target files"), "{ctx}");
    assert!(
        ctx.contains("## Code graph around crates/demo/src/cache.rs"),
        "{ctx}"
    );
    assert!(ctx.contains("NODE evict_oldest()"), "{ctx}");
    assert!(!ctx.contains("## Repository map"), "{ctx}");
    let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
    assert!(
        args.contains("query Fix cache eviction --budget 1142 --seed crates/demo/src/cache.rs"),
        "{args}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_files_seed_the_graph_when_the_issue_names_none() {
    let dir = scratch("ranked");
    let graph = fake(&dir, false);
    let ctx = build_issue_context_with(
        &dir,
        "evict_oldest panics",
        "Cache eviction.",
        8_000,
        4_000,
        Some(&graph),
    );
    assert!(
        ctx.contains("## Code graph around crates/demo/src/cache.rs"),
        "{ctx}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failing_graph_falls_back_to_the_repo_map() {
    let dir = scratch("fallback");
    let graph = fake(&dir, true);
    let with_failing_graph =
        build_issue_context_with(&dir, "Fix cache eviction", BODY, 8_000, 4_000, Some(&graph));
    assert!(
        with_failing_graph.contains("## Repository map"),
        "{with_failing_graph}"
    );
    assert!(
        !with_failing_graph.contains("graphify-out"),
        "graph files must not be mapped"
    );
    assert_eq!(
        with_failing_graph,
        build_issue_context(&dir, "Fix cache eviction", BODY, 8_000, 4_000)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn graph_tools_parse_and_appear_in_the_prompt_only_when_ready() {
    match parse_model_action(
        r#"{"tool": "graph:query", "question": "who evicts", "seed": "crates/demo/src/cache.rs"}"#,
    ) {
        Some(RepoToolCall::GraphQuery {
            question,
            seed,
            budget,
        }) => {
            assert_eq!(question, "who evicts");
            assert_eq!(seed, Some(serde_json::json!("crates/demo/src/cache.rs")));
            assert_eq!(budget, None);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(
        parse_model_action(r#"{"tool": "graph:explain", "name": "Cache"}"#),
        Some(RepoToolCall::GraphExplain { .. })
    ));
    assert!(matches!(
        parse_model_action(r#"{"tool": "graph:path", "from": "A", "to": "B"}"#),
        Some(RepoToolCall::GraphPath { .. })
    ));

    let base = "Tools (JSON):\n1. x\n\nEdits (preferred; ...)\nRules:";
    assert_eq!(solver_system_prompt(base, false), base);
    let with_graph = solver_system_prompt(base, true);
    let tools = with_graph.find("graph:query").unwrap();
    assert!(with_graph.find("1. x").unwrap() < tools);
    assert!(tools < with_graph.find("Edits (preferred").unwrap());
}
