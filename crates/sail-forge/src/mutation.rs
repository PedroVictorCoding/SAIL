//! Mutation gate: do the tests actually catch changes to the patched code?
//!
//! A passing test suite does not show that a patch is tested (vision §8.3: "compile success
//! alone is insufficient"). After Forge's tests and diff gates pass, this gate runs
//! `cargo mutants --in-diff` inside the sandboxed worktree. cargo-mutants breaks each
//! function the patch touched (for example, replaces its body with `Default::default()`)
//! and runs the tests again. A *missed* mutant means no test failed with that function
//! broken, so the patch's behavior is not really tested.
//!
//! Modes (`SAIL_FORGE_MUTATION_GATE`):
//! - `off`: never run.
//! - `advisory` (default): run and record the result in the job's activity; never block.
//! - `required`: block the patch when a mutant is missed, cargo-mutants is missing, or the
//!   run cannot complete. The missed mutants become the retry feedback for the model.

use std::fs;
use std::path::Path;

use crate::test_report::{FORGE_SCRATCH_DIR, TestTool, binary_on_path};
use crate::worktree::IsolatedWorktree;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationGateMode {
    Off,
    Advisory,
    Required,
}

impl MutationGateMode {
    pub fn from_env() -> Self {
        Self::parse(std::env::var("SAIL_FORGE_MUTATION_GATE").ok().as_deref())
    }

    fn parse(v: Option<&str>) -> Self {
        match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("off") | Some("0") | Some("false") => MutationGateMode::Off,
            Some("required") | Some("require") | Some("block") => MutationGateMode::Required,
            _ => MutationGateMode::Advisory,
        }
    }
}

/// What the gate observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationOutcome {
    /// The gate did not run cargo-mutants, with the reason.
    NotRun(String),
    /// cargo-mutants finished.
    Completed(MutationCounts),
    /// cargo-mutants started but its result cannot be trusted (crash, timeout, or it left
    /// the worktree modified).
    Inconclusive(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MutationCounts {
    pub caught: usize,
    pub missed: Vec<String>,
    pub timeout: usize,
    pub unviable: usize,
}

impl MutationCounts {
    pub fn tested(&self) -> usize {
        self.caught + self.missed.len() + self.timeout
    }
}

/// Gate decision: whether the patch may continue, and a one-line summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationVerdict {
    pub blocks: bool,
    pub summary: String,
    pub details: Vec<String>,
}

/// Decide what an outcome means under `mode`.
pub fn judge(mode: MutationGateMode, outcome: &MutationOutcome) -> MutationVerdict {
    let required = mode == MutationGateMode::Required;
    match outcome {
        MutationOutcome::NotRun(reason) => MutationVerdict {
            blocks: required,
            summary: format!("Mutation gate not run: {reason}"),
            details: vec![],
        },
        MutationOutcome::Inconclusive(reason) => MutationVerdict {
            blocks: required,
            summary: format!("Mutation gate inconclusive: {reason}"),
            details: vec![],
        },
        MutationOutcome::Completed(c) if c.missed.is_empty() => MutationVerdict {
            blocks: false,
            summary: if c.tested() == 0 {
                "Mutation gate: no mutable code in the patch".to_string()
            } else {
                format!(
                    "Mutation gate passed: tests caught {} of {} mutants ({} timed out, {} unviable)",
                    c.caught,
                    c.tested(),
                    c.timeout,
                    c.unviable
                )
            },
            details: vec![],
        },
        MutationOutcome::Completed(c) => MutationVerdict {
            blocks: required,
            summary: format!(
                "Mutation gate: {} of {} mutants were not caught by any test; add tests that fail when this code is broken",
                c.missed.len(),
                c.tested()
            ),
            details: c.missed.iter().take(20).cloned().collect(),
        },
    }
}

/// Run cargo-mutants on the lines `diff` changed, inside the worktree's sandbox.
///
/// Mutates the worktree in place (it is a disposable private copy) so the existing build
/// is reused. Afterwards the worktree's diff must be byte-identical to `diff`; otherwise
/// the result is inconclusive and the caller must not trust the worktree.
pub fn run(worktree: &IsolatedWorktree, diff: &str, timeout_secs: u64) -> MutationOutcome {
    if !binary_on_path("cargo-mutants") {
        return MutationOutcome::NotRun("cargo-mutants is not installed".into());
    }
    if !diff
        .lines()
        .any(|l| l.starts_with("+++ b/") && l.ends_with(".rs"))
    {
        return MutationOutcome::NotRun("the patch changes no Rust files".into());
    }

    let scratch = Path::new(FORGE_SCRATCH_DIR);
    let diff_rel = scratch.join("mutants.diff");
    if let Err(e) = worktree.write_file(&diff_rel, diff) {
        return MutationOutcome::NotRun(format!("could not stage the diff: {e}"));
    }
    let out_dir = scratch.join("mutants.out");
    if let Err(e) = worktree.remove_output(&out_dir) {
        return MutationOutcome::NotRun(format!("could not clear old results: {e}"));
    }

    let diff_arg = diff_rel.to_string_lossy().into_owned();
    let out_arg = scratch.to_string_lossy().into_owned();
    let tool = TestTool::from_env().mutants_name();
    let args = [
        "mutants",
        "--in-place",
        "--in-diff",
        diff_arg.as_str(),
        "--output",
        out_arg.as_str(),
        "--test-tool",
        tool,
        // Forge's own test step just passed on this tree.
        "--baseline",
        "skip",
        "--no-shuffle",
        "--timeout-multiplier",
        "3",
        "--cargo-arg=--offline",
        "--colors",
        "never",
    ];
    let result = worktree.run_command("cargo", &args, timeout_secs);

    // In-place mutation restores each file after testing it; prove that it did.
    match worktree.get_diff() {
        Ok(after) if after == diff => {}
        Ok(_) => {
            return MutationOutcome::Inconclusive(
                "the worktree differs from the patch after mutation testing".into(),
            );
        }
        Err(e) => {
            return MutationOutcome::Inconclusive(format!("could not re-read the diff: {e}"));
        }
    }

    let output = match result {
        Ok(o) => o,
        Err(e) => return MutationOutcome::Inconclusive(format!("cargo-mutants did not run: {e}")),
    };
    // cargo-mutants: 0 all caught, 2 some missed, 3 some timed out. Anything else
    // (usage error, failed baseline, interrupted run) means the counts are not a result.
    if ![0, 2, 3].contains(&output.status) {
        let tail: Vec<&str> = output.stderr.lines().rev().take(6).collect();
        return MutationOutcome::Inconclusive(format!(
            "cargo-mutants exited with {}: {}",
            output.status,
            tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
        ));
    }
    // The sandbox wrote these files; read them without following symlinks out of it.
    MutationOutcome::Completed(counts_from(|name| {
        worktree
            .read_output(&out_dir.join(name))
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }))
}

/// Read cargo-mutants' per-outcome list files from its output directory.
pub fn read_counts(out_dir: &Path) -> MutationCounts {
    counts_from(|name| fs::read_to_string(out_dir.join(name)).ok())
}

fn counts_from(read: impl Fn(&str) -> Option<String>) -> MutationCounts {
    let lines = |name: &str| -> Vec<String> {
        read(name)
            .map(|s| {
                s.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    };
    MutationCounts {
        caught: lines("caught.txt").len(),
        missed: lines("missed.txt"),
        timeout: lines("timeout.txt").len(),
        unviable: lines("unviable.txt").len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_defaults_to_advisory() {
        assert_eq!(MutationGateMode::parse(None), MutationGateMode::Advisory);
        assert_eq!(
            MutationGateMode::parse(Some("bogus")),
            MutationGateMode::Advisory
        );
        assert_eq!(MutationGateMode::parse(Some("OFF")), MutationGateMode::Off);
        assert_eq!(
            MutationGateMode::parse(Some("required")),
            MutationGateMode::Required
        );
    }

    fn missed() -> MutationOutcome {
        MutationOutcome::Completed(MutationCounts {
            caught: 3,
            missed: vec!["src/lib.rs:4:5: replace add -> i32 with 0".into()],
            timeout: 0,
            unviable: 1,
        })
    }

    #[test]
    fn missed_mutants_block_only_when_required() {
        let advisory = judge(MutationGateMode::Advisory, &missed());
        assert!(!advisory.blocks);
        assert!(advisory.summary.contains("1 of 4 mutants were not caught"));
        assert_eq!(
            advisory.details,
            vec!["src/lib.rs:4:5: replace add -> i32 with 0"]
        );

        assert!(judge(MutationGateMode::Required, &missed()).blocks);
    }

    #[test]
    fn required_mode_fails_closed_when_it_cannot_run() {
        let not_run = MutationOutcome::NotRun("cargo-mutants is not installed".into());
        assert!(judge(MutationGateMode::Required, &not_run).blocks);
        assert!(!judge(MutationGateMode::Advisory, &not_run).blocks);

        let broken = MutationOutcome::Inconclusive("exited with 4".into());
        assert!(judge(MutationGateMode::Required, &broken).blocks);
    }

    #[test]
    fn all_caught_passes() {
        let ok = MutationOutcome::Completed(MutationCounts {
            caught: 5,
            ..Default::default()
        });
        let v = judge(MutationGateMode::Required, &ok);
        assert!(!v.blocks);
        assert!(v.summary.contains("caught 5 of 5"));

        let none = MutationOutcome::Completed(MutationCounts::default());
        assert!(
            judge(MutationGateMode::Required, &none)
                .summary
                .contains("no mutable code")
        );
    }

    #[test]
    fn reads_outcome_files() {
        let dir = std::env::temp_dir().join(format!("sail-mutants-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("caught.txt"), "a\nb\n\n").unwrap();
        fs::write(dir.join("missed.txt"), "src/x.rs:1:1: replace f with ()\n").unwrap();
        let c = read_counts(&dir);
        assert_eq!(c.caught, 2);
        assert_eq!(c.missed.len(), 1);
        assert_eq!(c.timeout, 0);
        fs::remove_dir_all(&dir).unwrap();
    }
}
