//! Real solve-rate evaluation of the local solver.
//!
//! Unlike `BenchmarkHarness` (which validates fixtures by grading their reference
//! patches), this runs the actual `LocalSolverController` against each fixture in an
//! isolated worktree and grades the result with tests the solver never saw.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sail_guardian::sandbox::SandboxPolicy;

use crate::eval_cases::CASES;
use crate::fixtures::get_multi_file_smoke_fixture;
use crate::github::run_with_timeout;
use crate::solver::LocalSolverController;
use crate::worktree::IsolatedWorktree;

/// One evaluation task: a tiny crate, an issue, and an optional hidden test.
#[derive(Debug, Clone)]
pub struct EvalCase {
    pub id: u64,
    pub title: String,
    pub description: String,
    pub category: String,
    pub cargo_toml: String,
    pub files: Vec<(String, String)>,
    /// `(file, code)` appended to `file` after solving; `None` means the visible
    /// tests in `files` are the grader.
    pub hidden_test: Option<(String, String)>,
    /// Known-good solution, used only to validate the case itself.
    pub reference_files: Vec<(String, String)>,
    pub reference_patch: Option<String>,
}

impl EvalCase {
    /// Issue body in the `forge-ready` template shape the nightly runner feeds the solver.
    pub fn issue_body(&self) -> String {
        let targets: Vec<String> = self.files.iter().map(|(p, _)| format!("- `{p}`")).collect();
        format!(
            "## Problem\n{}\n\n## Target files\n{}\n\n## Acceptance\n- [ ] The described defect is fixed\n- [ ] `cargo test` passes\n",
            self.description,
            targets.join("\n")
        )
    }
}

fn owned(files: &[(&str, &str)]) -> Vec<(String, String)> {
    files
        .iter()
        .map(|(p, c)| (p.to_string(), c.to_string()))
        .collect()
}

/// All built-in cases: the validated single-crate tasks in `eval_cases` plus the
/// multi-file smoke fixture (graded by its own visible test).
pub fn builtin_cases() -> Vec<EvalCase> {
    let mut cases: Vec<EvalCase> = CASES
        .iter()
        .map(|c| {
            let lib_path = c.files[0].0;
            EvalCase {
                id: c.id,
                title: c.title.into(),
                description: c.description.into(),
                category: c.category.into(),
                cargo_toml: format!(
                    "[package]\nname = \"eval_{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"{lib_path}\"\n",
                    c.id
                ),
                files: owned(c.files),
                hidden_test: c.hidden_test.map(|(f, t)| (f.to_string(), t.to_string())),
                reference_files: owned(c.reference),
                reference_patch: None,
            }
        })
        .collect();
    let smoke = get_multi_file_smoke_fixture();
    cases.push(EvalCase {
        id: smoke.id,
        title: smoke.title.into(),
        description: smoke.description.into(),
        category: "multi_file".into(),
        cargo_toml: smoke.cargo_toml.into(),
        files: owned(&smoke.initial_files),
        hidden_test: None,
        reference_files: Vec::new(),
        reference_patch: Some(smoke.reference_patch.into()),
    });
    cases
}

/// Result for one case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseResult {
    pub id: u64,
    pub title: String,
    pub category: String,
    /// The model could not be reached; this case says nothing about solve ability.
    pub unavailable: bool,
    /// Hidden (or, for multi-file, visible) tests pass after solving.
    pub solved: bool,
    pub solver_claimed_success: bool,
    pub edit_failures: u32,
    pub parse_failures: u32,
    pub turns: u32,
    pub wall_ms: u64,
    pub diff_lines: usize,
    pub status: String,
    pub detail: String,
}

impl CaseResult {
    pub fn false_success(&self) -> bool {
        self.solver_claimed_success && !self.solved
    }
}

/// A full evaluation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRun {
    pub started_at: DateTime<Utc>,
    pub label: String,
    pub model: String,
    pub endpoint: String,
    pub max_iterations: u32,
    pub cases: Vec<CaseResult>,
}

impl EvalRun {
    fn measured(&self) -> impl Iterator<Item = &CaseResult> {
        self.cases.iter().filter(|c| !c.unavailable)
    }

    pub fn measured_count(&self) -> usize {
        self.measured().count()
    }

    pub fn solved_count(&self) -> usize {
        self.measured().filter(|c| c.solved).count()
    }

    pub fn false_success_count(&self) -> usize {
        self.measured().filter(|c| c.false_success()).count()
    }

    pub fn edit_failures(&self) -> u32 {
        self.measured().map(|c| c.edit_failures).sum()
    }

    pub fn parse_failures(&self) -> u32 {
        self.measured().map(|c| c.parse_failures).sum()
    }

    pub fn summary_line(&self) -> String {
        let n = self.measured_count();
        if n == 0 {
            return "no cases measured (model unavailable)".into();
        }
        format!(
            "solved {}/{} ({:.0}%), false success {}, edit failures {}, unparseable replies {}",
            self.solved_count(),
            n,
            100.0 * self.solved_count() as f64 / n as f64,
            self.false_success_count(),
            self.edit_failures(),
            self.parse_failures()
        )
    }

    pub fn to_markdown(&self) -> String {
        let mut md = format!(
            "# Forge solver eval `{}`\n\nModel `{}` at `{}`, max iterations {}, started {}\n\n**{}**\n\n",
            self.label,
            self.model,
            self.endpoint,
            self.max_iterations,
            self.started_at.format("%Y-%m-%d %H:%M UTC"),
            self.summary_line()
        );
        md.push_str("| id | category | solved | claimed | edit fails | parse fails | turns | time | status |\n|---|---|---|---|---|---|---|---|---|\n");
        for c in &self.cases {
            md.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {}s | {} |\n",
                c.id,
                c.category,
                if c.unavailable {
                    "n/a"
                } else if c.solved {
                    "yes"
                } else {
                    "no"
                },
                if c.solver_claimed_success {
                    "yes"
                } else {
                    "no"
                },
                c.edit_failures,
                c.parse_failures,
                c.turns,
                c.wall_ms / 1000,
                c.status
            ));
        }
        md
    }

    /// Per-case and aggregate comparison against an earlier run.
    pub fn compare(&self, baseline: &EvalRun) -> String {
        let mut md = format!(
            "## Comparison vs `{}`\n\n- baseline: {}\n- current: {}\n\n| id | baseline | current |\n|---|---|---|\n",
            baseline.label,
            baseline.summary_line(),
            self.summary_line()
        );
        let verdict = |c: Option<&CaseResult>| match c {
            None => "-".to_string(),
            Some(c) if c.unavailable => "n/a".into(),
            Some(c) => format!(
                "{} ({} edit fails, {} turns)",
                if c.solved { "solved" } else { "unsolved" },
                c.edit_failures,
                c.turns
            ),
        };
        for c in &self.cases {
            let b = baseline.cases.iter().find(|b| b.id == c.id);
            md.push_str(&format!(
                "| {} | {} | {} |\n",
                c.id,
                verdict(b),
                verdict(Some(c))
            ));
        }
        md
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    run_with_timeout(
        Command::new("git").arg("-C").arg(dir).args(args),
        Duration::from_secs(60),
    )
}

/// Materialize a case as a committed git repository under `root`.
pub fn build_case_repo(case: &EvalCase, root: &Path) -> Result<PathBuf, String> {
    let repo = root.join(format!("case_{}", case.id));
    std::fs::create_dir_all(&repo).map_err(|e| e.to_string())?;
    std::fs::write(repo.join("Cargo.toml"), &case.cargo_toml).map_err(|e| e.to_string())?;
    for (path, content) in &case.files {
        let full = repo.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(full, content).map_err(|e| e.to_string())?;
    }
    git(&repo, &["init", "-q", "-b", "main"])?;
    git(&repo, &["add", "."])?;
    git(
        &repo,
        &[
            "-c",
            "user.name=SAIL Eval",
            "-c",
            "user.email=eval@sail.local",
            "commit",
            "-q",
            "-m",
            "eval fixture",
        ],
    )?;
    Ok(repo)
}

/// Grade a solved worktree: inject the hidden test (if any) and run `cargo test`.
pub fn grade_worktree(case: &EvalCase, worktree: &IsolatedWorktree) -> (bool, String) {
    if let Some((file, code)) = &case.hidden_test {
        let current = match worktree.read_file(Path::new(file)) {
            Ok(c) => c,
            Err(e) => return (false, format!("target file unreadable after solve: {e}")),
        };
        if let Err(e) = worktree.write_file(Path::new(file), &format!("{current}\n{code}\n")) {
            return (false, format!("could not inject hidden test: {e}"));
        }
    }
    match worktree.run_command("cargo", &["test", "--offline"], 300) {
        Ok(out) if out.status == 0 => (true, "tests pass".into()),
        Ok(out) => {
            let tail: Vec<&str> = out.stdout.lines().chain(out.stderr.lines()).collect();
            let start = tail.len().saturating_sub(12);
            (false, tail[start..].join("\n"))
        }
        Err(e) => (false, format!("cargo test could not run: {e}")),
    }
}

/// Run one case end to end with the real solver.
pub async fn run_case(
    case: &EvalCase,
    controller: &LocalSolverController,
    scratch: &Path,
) -> CaseResult {
    let started = Instant::now();
    let mut result = CaseResult {
        id: case.id,
        title: case.title.clone(),
        category: case.category.clone(),
        unavailable: false,
        solved: false,
        solver_claimed_success: false,
        edit_failures: 0,
        parse_failures: 0,
        turns: 0,
        wall_ms: 0,
        diff_lines: 0,
        status: String::new(),
        detail: String::new(),
    };

    let repo = match build_case_repo(case, scratch) {
        Ok(r) => r,
        Err(e) => {
            result.status = "setup_failed".into();
            result.detail = e;
            return result;
        }
    };
    let job_id = format!(
        "eval-{}-{}",
        case.id,
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let worktree =
        match IsolatedWorktree::create(&job_id, &repo, "HEAD", Arc::new(SandboxPolicy::new())) {
            Ok(w) => w,
            Err(e) => {
                result.status = "setup_failed".into();
                result.detail = e.to_string();
                return result;
            }
        };

    let outcome = controller
        .solve_issue(
            &worktree,
            &case.title,
            &case.issue_body(),
            &scratch.join(format!("logs_{}", case.id)),
            || false,
            |_| {},
        )
        .await;

    result.turns = outcome.turns;
    result.edit_failures = outcome.edit_failures;
    result.parse_failures = outcome.parse_failures;
    result.solver_claimed_success = outcome.success;
    result.diff_lines = outcome.final_diff.lines().count();
    result.status = outcome.verification_status.clone();
    if outcome.verification_status == "model_unavailable" {
        result.unavailable = true;
        result.detail = outcome.failure_reason.unwrap_or_default();
    } else {
        let (solved, detail) = grade_worktree(case, &worktree);
        result.solved = solved;
        result.detail = detail;
    }
    result.wall_ms = started.elapsed().as_millis() as u64;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_cases_cover_all_fixtures() {
        let cases = builtin_cases();
        assert_eq!(cases.len(), CASES.len() + 1);
        assert_eq!(
            cases.iter().filter(|c| c.hidden_test.is_some()).count(),
            CASES.len()
        );
        for c in &cases {
            assert!(c.issue_body().contains("## Target files"));
            assert!(c.issue_body().contains("## Acceptance"));
        }
    }

    fn result(id: u64, solved: bool, claimed: bool, unavailable: bool) -> CaseResult {
        CaseResult {
            id,
            title: String::new(),
            category: String::new(),
            unavailable,
            solved,
            solver_claimed_success: claimed,
            edit_failures: 1,
            parse_failures: 0,
            turns: 3,
            wall_ms: 1000,
            diff_lines: 4,
            status: String::new(),
            detail: String::new(),
        }
    }

    #[test]
    fn rates_exclude_unavailable_cases_and_count_false_success() {
        let run = EvalRun {
            started_at: Utc::now(),
            label: "t".into(),
            model: "m".into(),
            endpoint: "e".into(),
            max_iterations: 5,
            cases: vec![
                result(1, true, true, false),
                result(2, false, true, false),
                result(3, false, false, true),
            ],
        };
        assert_eq!(run.measured_count(), 2);
        assert_eq!(run.solved_count(), 1);
        assert_eq!(run.false_success_count(), 1);
        assert!(run.summary_line().starts_with("solved 1/2 (50%)"));
        assert!(run.to_markdown().contains("| 3 |  | n/a |"));
    }

    #[test]
    fn all_unavailable_never_reports_a_rate() {
        let run = EvalRun {
            started_at: Utc::now(),
            label: "t".into(),
            model: "m".into(),
            endpoint: "e".into(),
            max_iterations: 5,
            cases: vec![result(1, false, false, true)],
        };
        assert!(run.summary_line().contains("model unavailable"));
    }

    fn case_worktree(case: &EvalCase, root: &Path, tag: &str) -> IsolatedWorktree {
        let repo = build_case_repo(case, &root.join(tag)).unwrap();
        IsolatedWorktree::create(
            &format!("eval-{tag}-{}-{}", case.id, uuid::Uuid::new_v4()),
            &repo,
            "HEAD",
            Arc::new(SandboxPolicy::new()),
        )
        .unwrap()
    }

    /// Every case must fail before solving and pass with its reference solution;
    /// otherwise it measures nothing.
    #[test]
    fn cases_fail_unmodified_and_pass_with_reference() {
        let root = std::env::temp_dir().join(format!("eval_valid_{}", uuid::Uuid::new_v4()));
        let mut problems = Vec::new();
        for case in builtin_cases() {
            let wt = case_worktree(&case, &root, "unmodified");
            if grade_worktree(&case, &wt).0 {
                problems.push(format!("{} passes without a fix", case.id));
            }

            let wt = case_worktree(&case, &root, "reference");
            for (path, content) in &case.reference_files {
                wt.write_file(Path::new(path), content).unwrap();
            }
            if let Some(patch) = &case.reference_patch {
                wt.apply_patch(patch).unwrap();
            }
            let (solved, detail) = grade_worktree(&case, &wt);
            if !solved {
                problems.push(format!("{} reference fails: {detail}", case.id));
            }
        }
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            problems.is_empty(),
            "invalid eval cases:\n{}",
            problems.join("\n")
        );
    }
}
