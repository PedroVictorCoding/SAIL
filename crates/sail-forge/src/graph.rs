//! Graphify knowledge-graph retrieval for the solver.
//!
//! [graphify](https://github.com/Graphify-Labs/graphify) parses the worktree with tree-sitter
//! into `graphify-out/graph.json`: files, symbols and their `calls`/`contains`/`uses` edges.
//! The build is AST-only (`GRAPHIFY_BACKEND=none`), so no model runs and nothing leaves the
//! host. Instead of grepping and listing the whole repository, the solver asks the graph for
//! the scoped neighbourhood it needs:
//!
//! - `graph:query {"question": "...", "seed": ["crates/x/src/y.rs"]}`: a token-capped BFS
//!   subgraph, started at the seeds (file paths or symbol names) or at the question's words.
//! - `graph:explain {"name": "parse_target_files"}`: one node with its callers and callees.
//! - `graph:path {"from": "A", "to": "B"}`: the shortest relationship chain between two nodes.
//!
//! The graph is built once per job worktree, before the first model turn. A copy of the last
//! built `graphify-out/` of the same repository (the seed, see [`seed_dir`]) turns the cold build of about
//! four minutes into an incremental update of about fifteen seconds. Edits made during the
//! job are not re-indexed; the prompt tells the model to re-read files it changed.
//!
//! When graphify is not installed, is disabled (`SAIL_GRAPHIFY=off`) or fails, [`prepare`]
//! reports why and the solver keeps its repo map and text search; nothing else changes.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Output directory graphify writes inside the scanned root.
pub const OUT_DIR: &str = "graphify-out";
/// Directory of seed copies of `graphify-out/`, one per repository (keyed by its first
/// commit), shared by job and agent worktrees. On the HDD, not the system SSD.
pub const DEFAULT_SEED_ROOT: &str = "/srv/sail-data/graphify/seeds";
/// Token budget for a `graph:query` call when the model does not give one.
pub const DEFAULT_QUERY_BUDGET: u32 = 1_200;
/// Upper bound on a model-chosen budget, so one call cannot flood the 16k context.
pub const MAX_QUERY_BUDGET: u32 = 3_000;
const QUERY_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_BUILD_TIMEOUT_SECS: u64 = 900;
/// Files of `graphify-out/` that are not worth copying into the seed.
const SEED_SKIP: &[&str] = &["graph.html"];
/// Hint appended when graph tools cannot answer.
const FALLBACK_HINT: &str = "use repo:map or repo:search instead";

/// Whether graph retrieval is enabled (`SAIL_GRAPHIFY` unset or not `0`/`off`/`false`/`no`).
pub fn enabled() -> bool {
    !std::env::var("SAIL_GRAPHIFY")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no"
            )
        })
        .unwrap_or(false)
}

/// Path of the graph inside `root`.
pub fn graph_path(root: &Path) -> PathBuf {
    root.join(OUT_DIR).join("graph.json")
}

/// `SAIL_GRAPHIFY_SEED_ROOT` (default [`DEFAULT_SEED_ROOT`]) joined with the repository key,
/// so different repositories never share or overwrite one seed.
pub fn seed_dir(root: &Path) -> PathBuf {
    let base = std::env::var_os("SAIL_GRAPHIFY_SEED_ROOT")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SEED_ROOT));
    base.join(repo_key(root))
}

/// First 12 hex digits of the repository's root commit (shared by all of its worktrees and
/// clones), or `unknown` outside Git.
fn repo_key(root: &Path) -> String {
    Command::new("git")
        .args(["rev-list", "--max-parents=0", "HEAD"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let text = String::from_utf8_lossy(&o.stdout);
            // Several roots (merged histories): the sorted first is stable.
            let mut roots: Vec<&str> = text.lines().map(str::trim).collect();
            roots.sort_unstable();
            roots
                .first()
                .filter(|r| r.len() >= 12 && r.chars().all(|c| c.is_ascii_hexdigit()))
                .map(|r| r[..12].to_ascii_lowercase())
        })
        .unwrap_or_else(|| "unknown".into())
}

fn build_timeout() -> Duration {
    let secs = std::env::var("SAIL_GRAPHIFY_BUILD_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_BUILD_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Result of one graphify CLI run.
#[derive(Debug, Clone)]
pub struct RunOutput {
    /// Process exit code; `None` when killed (timeout) or terminated by a signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// The graphify CLI, located once per call site.
#[derive(Debug, Clone)]
pub struct Graphify {
    bin: PathBuf,
}

impl Graphify {
    /// Use an explicit binary (tests, or a non-PATH install).
    pub fn with_binary(bin: impl Into<PathBuf>) -> Self {
        Self { bin: bin.into() }
    }

    /// `SAIL_GRAPHIFY_BIN`, else `graphify` on `PATH`; `None` when disabled or not installed.
    pub fn from_env() -> Option<Self> {
        if !enabled() {
            return None;
        }
        if let Some(bin) = std::env::var_os("SAIL_GRAPHIFY_BIN").filter(|v| !v.is_empty()) {
            let bin = PathBuf::from(bin);
            return bin.is_file().then_some(Self { bin });
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join("graphify"))
            .find(|candidate| candidate.is_file())
            .map(|bin| Self { bin })
    }

    /// Run `graphify <args>` in `root` with LLM backends and the query log disabled.
    pub fn run(&self, root: &Path, args: &[&str], timeout: Duration) -> Result<RunOutput, String> {
        let mut child = Command::new(&self.bin)
            .args(args)
            .current_dir(root)
            .env("GRAPHIFY_BACKEND", "none")
            .env("GRAPHIFY_QUERY_LOG_DISABLE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Own process group, so a timeout also stops graphify's extraction workers,
            // which would otherwise keep the output pipes open.
            .process_group(0)
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", self.bin.display()))?;

        // Drain both pipes on threads so a large graph answer cannot block the child.
        let mut out_pipe = child.stdout.take();
        let mut err_pipe = child.stderr.take();
        let out_reader = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(p) = out_pipe.as_mut() {
                let _ = p.read_to_string(&mut s);
            }
            s
        });
        let err_reader = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(p) = err_pipe.as_mut() {
                let _ = p.read_to_string(&mut s);
            }
            s
        });

        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if started.elapsed() >= timeout => {
                    // SAFETY: plain syscall on the group this child leads (pgid == pid).
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(e) => return Err(format!("waiting for graphify failed: {e}")),
            }
        };
        let stdout = out_reader.join().unwrap_or_default();
        let stderr = err_reader.join().unwrap_or_default();
        match status {
            None => Err(format!(
                "graphify {} timed out after {}s",
                args.first().copied().unwrap_or_default(),
                timeout.as_secs()
            )),
            Some(status) => Ok(RunOutput {
                code: status.code(),
                stdout,
                stderr,
            }),
        }
    }

    /// Build or refresh the graph of `root`, seeding a missing one from `seed` first.
    pub fn prepare_with_seed(&self, root: &Path, seed: &Path) -> Result<Prepared, String> {
        let started = Instant::now();
        let out = root.join(OUT_DIR);
        let seeded = !graph_path(root).is_file() && seed.join("graph.json").is_file();
        if seeded {
            copy_dir(seed, &out, &[])
                .map_err(|e| format!("copying graph seed {}: {e}", seed.display()))?;
        }
        // Plain update first: it re-extracts only changed files (seconds). `--force` rebuilds
        // everything (minutes), so it is only the retry when graphify refuses to shrink a
        // graph seeded from a commit with more code.
        let mut run = self.run(root, &["update", "."], build_timeout())?;
        if run.code != Some(0) {
            tracing::info!(stderr = %tail(&run.stderr, 300), "graphify update refused; retrying with --force");
            run = self.run(root, &["update", ".", "--force"], build_timeout())?;
        }
        if run.code != Some(0) || !graph_path(root).is_file() {
            return Err(format!(
                "graphify update failed (exit {:?}): {}",
                run.code,
                tail(&format!("{}\n{}", run.stdout, run.stderr), 600)
            ));
        }
        if let Err(e) = refresh_seed(&out, seed) {
            tracing::warn!(seed = %seed.display(), error = %e, "could not refresh the graphify seed");
        }
        Ok(Prepared {
            seeded,
            elapsed: started.elapsed(),
        })
    }

    /// `graph:query`: scoped subgraph for `question`, started at `seeds` when given.
    pub fn query(
        &self,
        root: &Path,
        question: &str,
        seeds: &[String],
        budget: u32,
    ) -> (bool, String) {
        let budget = budget.clamp(200, MAX_QUERY_BUDGET).to_string();
        let mut args: Vec<&str> = vec!["query", question, "--budget", &budget];
        for seed in seeds {
            args.push("--seed");
            args.push(seed);
        }
        self.answer(root, &args)
    }

    /// `graph:explain`: one node, its file and line, and its strongest connections.
    pub fn explain(&self, root: &Path, name: &str) -> (bool, String) {
        self.answer(root, &["explain", name])
    }

    /// `graph:path`: shortest chain of edges from `from` to `to`.
    pub fn path(&self, root: &Path, from: &str, to: &str) -> (bool, String) {
        self.answer(root, &["path", from, to])
    }

    /// Run a read-only graph command and turn its exit code into a tool result.
    fn answer(&self, root: &Path, args: &[&str]) -> (bool, String) {
        if !graph_path(root).is_file() {
            return (
                false,
                format!("No code graph in this worktree; {FALLBACK_HINT}"),
            );
        }
        match self.run(root, args, QUERY_TIMEOUT) {
            Err(e) => (
                false,
                format!("Code graph unavailable ({e}); {FALLBACK_HINT}"),
            ),
            Ok(run) => interpret(&run),
        }
    }
}

/// Map graphify's documented exit codes (0 answer, 3 ambiguous name, 4 no match) to output.
fn interpret(run: &RunOutput) -> (bool, String) {
    let text = if run.stdout.trim().is_empty() {
        run.stderr.trim()
    } else {
        run.stdout.trim()
    };
    match run.code {
        Some(0) => (true, text.to_string()),
        Some(3) => (
            true,
            format!("{text}\n(Ambiguous name: repeat the call with one of the candidates above.)"),
        ),
        Some(4) => (
            false,
            format!("{text}\nNo matching node in the code graph; {FALLBACK_HINT}"),
        ),
        code => (
            false,
            format!(
                "graphify failed (exit {code:?}): {}; {FALLBACK_HINT}",
                tail(text, 400)
            ),
        ),
    }
}

/// Outcome of [`prepare`].
#[derive(Debug, Clone)]
pub struct Prepared {
    /// The graph started from the shared seed instead of a cold build.
    pub seeded: bool,
    pub elapsed: Duration,
}

/// Build or refresh the code graph of `root` with the installed graphify, seeding from
/// [`seed_dir`]. `Err` explains why the graph is missing.
pub fn prepare(root: &Path) -> Result<Prepared, String> {
    if !enabled() {
        return Err("disabled by SAIL_GRAPHIFY".into());
    }
    let graphify = Graphify::from_env().ok_or("graphify is not installed")?;
    graphify.prepare_with_seed(root, &seed_dir(root))
}

/// Run a graph tool for the solver; the closure gets the located CLI.
pub fn run_tool(f: impl FnOnce(&Graphify) -> (bool, String)) -> (bool, String) {
    match Graphify::from_env() {
        Some(g) => f(&g),
        None => (
            false,
            format!("Code graph unavailable (graphify missing or disabled); {FALLBACK_HINT}"),
        ),
    }
}

/// Seeds for `graph:query` given as one string or a list of strings.
pub fn normalize_seeds(value: Option<&serde_json::Value>) -> Vec<String> {
    let mut seeds: Vec<String> = match value {
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    seeds.retain(|s| !s.trim().is_empty());
    seeds.iter_mut().for_each(|s| *s = s.trim().to_string());
    seeds.dedup();
    seeds.truncate(8);
    seeds
}

/// Last `max` characters of `s`, on a char boundary.
fn tail(s: &str, max: usize) -> String {
    let s = s.trim();
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        format!("…{}", s.chars().skip(count - max).collect::<String>())
    }
}

/// Recursively copy `from` into `to`, skipping top-level names in `skip`.
fn copy_dir(from: &Path, to: &Path, skip: &[&str]) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if skip.iter().any(|s| name == *s) {
            continue;
        }
        let ty = entry.file_type()?;
        let dest = to.join(&name);
        if ty.is_dir() {
            copy_dir(&entry.path(), &dest, &[])?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

/// Replace the seed with `out`, via a temporary sibling so readers never see half a copy.
fn refresh_seed(out: &Path, seed: &Path) -> std::io::Result<()> {
    let parent = seed.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        seed.file_name().and_then(|n| n.to_str()).unwrap_or("seed"),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    copy_dir(out, &tmp, SEED_SKIP)?;
    let old = parent.join(format!(
        ".{}.old-{}",
        seed.file_name().and_then(|n| n.to_str()).unwrap_or("seed"),
        std::process::id()
    ));
    if seed.exists() {
        std::fs::rename(seed, &old)?;
    }
    let result = std::fs::rename(&tmp, seed);
    if result.is_err() {
        // Another job installed its seed first; keep theirs.
        let _ = std::fs::remove_dir_all(&tmp);
    }
    let _ = std::fs::remove_dir_all(&old);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sail-graph-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in graphify: `update` writes a graph, read commands echo their args, and a
    /// name starting with `ambiguous`/`missing` exits 3/4 like the real CLI.
    fn fake_graphify(dir: &Path) -> Graphify {
        let bin = dir.join("graphify");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
[ "$GRAPHIFY_BACKEND" = none ] || { echo "backend not disabled" >&2; exit 9; }
case "$1" in
  update) if [ -f refuse ] && [ "$3" != --force ]; then echo "fewer nodes" >&2; exit 1; fi
          mkdir -p graphify-out && echo '{"nodes":[]}' > graphify-out/graph.json && echo "rebuilt $*" ;;
  explain) case "$2" in ambiguous*) echo "candidates: a b"; exit 3;; missing*) echo "no node"; exit 4;; esac; echo "Node: $2" ;;
  *) echo "ARGS: $*" ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Graphify::with_binary(bin)
    }

    #[test]
    fn prepare_seeds_builds_and_refreshes_the_seed() {
        let dir = scratch("prepare");
        let g = fake_graphify(&dir);
        let root = dir.join("wt");
        let seed = dir.join("seed");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(seed.join("cache")).unwrap();
        std::fs::write(seed.join("graph.json"), "{}").unwrap();
        std::fs::write(seed.join("cache").join("a.json"), "cached").unwrap();

        let prepared = g.prepare_with_seed(&root, &seed).unwrap();
        assert!(prepared.seeded);
        // The seed's cache came along, and the update rewrote the graph.
        assert_eq!(
            std::fs::read_to_string(root.join(OUT_DIR).join("cache/a.json")).unwrap(),
            "cached"
        );
        assert!(
            std::fs::read_to_string(graph_path(&root))
                .unwrap()
                .contains("nodes")
        );
        // The seed now holds the fresh graph.
        assert!(
            std::fs::read_to_string(seed.join("graph.json"))
                .unwrap()
                .contains("nodes")
        );

        // A second prepare finds the graph and does not reseed; a refused plain update is
        // retried with --force.
        std::fs::write(root.join("refuse"), "").unwrap();
        std::fs::write(graph_path(&root), "stale").unwrap();
        assert!(!g.prepare_with_seed(&root, &seed).unwrap().seeded);
        assert!(
            std::fs::read_to_string(graph_path(&root))
                .unwrap()
                .contains("nodes")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tools_pass_budget_and_seeds_and_map_exit_codes() {
        let dir = scratch("tools");
        let g = fake_graphify(&dir);
        let root = dir.join("wt");
        std::fs::create_dir_all(&root).unwrap();

        let (ok, text) = g.explain(&root, "x");
        assert!(!ok && text.contains("No code graph"), "{text}");

        g.prepare_with_seed(&root, &dir.join("no-seed")).unwrap();
        let (ok, text) = g.query(&root, "who builds context", &["src/a.rs".into()], 99_999);
        assert!(ok);
        assert_eq!(
            text,
            format!("ARGS: query who builds context --budget {MAX_QUERY_BUDGET} --seed src/a.rs")
        );
        assert_eq!(g.explain(&root, "solve").1, "Node: solve");
        let (ok, text) = g.explain(&root, "ambiguous_name");
        assert!(ok && text.contains("candidates") && text.contains("Ambiguous"));
        let (ok, text) = g.explain(&root, "missing_name");
        assert!(!ok && text.contains("repo:search"));
        assert_eq!(g.path(&root, "A", "B").1, "ARGS: path A B");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_times_out_and_kills_the_child() {
        let dir = scratch("timeout");
        let bin = dir.join("graphify");
        std::fs::write(&bin, "#!/bin/sh\nsleep 5\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        let err = Graphify::with_binary(&bin)
            .run(&dir, &["query", "x"], Duration::from_millis(200))
            .unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(4));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repo_key_is_the_root_commit_or_unknown() {
        let dir = scratch("key");
        assert_eq!(repo_key(&dir), "unknown");
        let ok = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&dir)
                    .output()
                    .unwrap()
                    .status
                    .success()
            )
        };
        ok(&["init", "-q"]);
        ok(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "root",
        ]);
        let key = repo_key(&dir);
        assert_eq!(key.len(), 12);
        assert!(seed_dir(&dir).ends_with(&key));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seeds_accept_a_string_or_a_list() {
        assert_eq!(
            normalize_seeds(Some(&serde_json::json!(" src/a.rs "))),
            vec!["src/a.rs"]
        );
        assert_eq!(
            normalize_seeds(Some(&serde_json::json!(["a", "", "a", 3, "b"]))),
            vec!["a", "b"]
        );
        assert!(normalize_seeds(None).is_empty());
    }
}
