//! Structured test results for Forge's solver loop.
//!
//! When `cargo-nextest` is available, Forge runs tests with it instead of `cargo test` and
//! reads the JUnit XML report it writes. The model then sees exactly which tests failed and
//! the first lines of each failure, instead of the last 80 lines of mixed compiler and test
//! output. Without nextest, Forge keeps using `cargo test` and the text tail.

use std::path::{Path, PathBuf};

/// Which test runner Forge uses inside the sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestTool {
    /// `cargo nextest run` with a JUnit report.
    Nextest,
    /// Plain `cargo test`.
    CargoTest,
}

impl TestTool {
    /// Pick the runner from `SAIL_FORGE_TEST_TOOL` (`auto`, `nextest` or `cargo`).
    ///
    /// `auto` (the default) uses nextest when `cargo-nextest` is on `PATH`. `nextest`
    /// forces it: if the binary is missing, the test run fails rather than silently
    /// switching runners.
    pub fn from_env() -> Self {
        Self::select(
            std::env::var("SAIL_FORGE_TEST_TOOL").ok().as_deref(),
            nextest_on_path(),
        )
    }

    fn select(setting: Option<&str>, nextest_available: bool) -> Self {
        match setting.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("cargo") | Some("cargo-test") => TestTool::CargoTest,
            Some("nextest") => TestTool::Nextest,
            _ if nextest_available => TestTool::Nextest,
            _ => TestTool::CargoTest,
        }
    }

    /// The `--test-tool` value cargo-mutants expects.
    pub fn mutants_name(self) -> &'static str {
        match self {
            TestTool::Nextest => "nextest",
            TestTool::CargoTest => "cargo",
        }
    }
}

fn nextest_on_path() -> bool {
    binary_on_path("cargo-nextest")
}

/// True when `name` is an executable file in one of the `PATH` directories.
pub fn binary_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// Directory inside the worktree (under the ignored `target/`) for Forge's own files.
/// Nothing in it ever appears in the diff.
pub const FORGE_SCRATCH_DIR: &str = "target/sail-forge";

/// Nextest configuration Forge passes with `--config-file`. It replaces the repository's
/// own nextest config for this run, so Forge's report settings do not depend on the repo.
pub const FORGE_NEXTEST_CONFIG: &str = r#"[profile.forge]
fail-fast = false
failure-output = "final"
success-output = "never"
status-level = "fail"
final-status-level = "fail"
slow-timeout = { period = "60s", terminate-after = 3 }

[profile.forge.junit]
path = "junit.xml"
store-success-output = false
store-failure-output = true
"#;

/// Path of the nextest config file, relative to the worktree root.
pub fn nextest_config_rel() -> PathBuf {
    Path::new(FORGE_SCRATCH_DIR).join("nextest.toml")
}

/// Where nextest writes the JUnit report for the `forge` profile, relative to the worktree.
pub fn junit_report_rel() -> PathBuf {
    Path::new("target/nextest/forge/junit.xml").to_path_buf()
}

/// Arguments for `cargo` that run nextest with Forge's profile.
pub fn nextest_args(filter: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "nextest".to_string(),
        "run".into(),
        "--offline".into(),
        "--config-file".into(),
        nextest_config_rel().to_string_lossy().into_owned(),
        "--profile".into(),
        "forge".into(),
    ];
    if let Some(f) = filter.map(str::trim).filter(|f| !f.is_empty()) {
        args.push(f.to_string());
    }
    args
}

/// One failed test from a JUnit report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedTest {
    /// Test binary or suite, for example `sail-forge::gates_test`.
    pub suite: String,
    /// Test path, for example `gates::tests::rejects_ignore`.
    pub name: String,
    /// Failure message or the most relevant lines of the test's output.
    pub detail: String,
}

/// Counts and failures parsed from a JUnit report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JunitSummary {
    pub tests: usize,
    pub failures: usize,
    pub skipped: usize,
    pub failed: Vec<FailedTest>,
}

impl JunitSummary {
    pub fn passed(&self) -> usize {
        self.tests.saturating_sub(self.failures + self.skipped)
    }
}

/// Maximum characters kept per failure detail.
const DETAIL_CHARS: usize = 600;

/// Parse a JUnit XML report as written by nextest.
pub fn parse_junit(xml: &str) -> Result<JunitSummary, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("invalid JUnit XML: {e}"))?;
    let mut summary = JunitSummary::default();
    for case in doc.descendants().filter(|n| n.has_tag_name("testcase")) {
        summary.tests += 1;
        let suite = case
            .parent()
            .filter(|p| p.has_tag_name("testsuite"))
            .and_then(|p| p.attribute("name"))
            .or_else(|| case.attribute("classname"))
            .unwrap_or("")
            .to_string();
        let name = case.attribute("name").unwrap_or("").to_string();

        if case.children().any(|c| c.has_tag_name("skipped")) {
            summary.skipped += 1;
            continue;
        }
        // nextest records a failed attempt as <failure> or <error>; a passing retry is
        // recorded as <flakyFailure>, which still counts as a pass here.
        let failure = case
            .children()
            .find(|c| c.has_tag_name("failure") || c.has_tag_name("error"));
        let Some(failure) = failure else { continue };
        summary.failures += 1;

        let output = case
            .children()
            .find(|c| c.has_tag_name("system-err") || c.has_tag_name("system-out"))
            .and_then(|c| c.text())
            .unwrap_or("");
        let message = failure
            .attribute("message")
            .or_else(|| failure.text())
            .unwrap_or("");
        summary.failed.push(FailedTest {
            suite,
            name,
            detail: failure_detail(message, output),
        });
    }
    Ok(summary)
}

/// Prefer the panic location and message from the output; fall back to the JUnit message.
fn failure_detail(message: &str, output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let detail = match lines
        .iter()
        .position(|l| l.contains("panicked at") || l.trim_start().starts_with("error"))
    {
        Some(start) => lines[start..lines.len().min(start + 8)].join("\n"),
        None if !message.trim().is_empty() => message.trim().to_string(),
        None => {
            let start = lines.len().saturating_sub(8);
            lines[start..].join("\n")
        }
    };
    truncate_chars(detail.trim(), DETAIL_CHARS)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str(" …");
    out
}

/// Render a summary for the model: counts, then up to `max_failures` failures.
pub fn format_for_model(summary: &JunitSummary, max_failures: usize) -> String {
    let mut out = format!(
        "tests: {} run, {} passed, {} failed, {} skipped\n",
        summary.tests,
        summary.passed(),
        summary.failures,
        summary.skipped
    );
    for f in summary.failed.iter().take(max_failures) {
        out.push_str(&format!("FAILED {} :: {}\n{}\n", f.suite, f.name, f.detail));
    }
    if summary.failed.len() > max_failures {
        out.push_str(&format!(
            "... and {} more failed tests (see the full log)\n",
            summary.failed.len() - max_failures
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="4" failures="1" errors="0">
  <testsuite name="demo" tests="4" disabled="1" errors="0" failures="1">
    <testcase name="tests::adds" classname="demo" timestamp="t" time="0.01"/>
    <testcase name="tests::rejects_empty" classname="demo" timestamp="t" time="0.02">
      <failure type="test failure with exit code 101">thread 'tests::rejects_empty' panicked</failure>
      <system-err>running 1 test
thread 'tests::rejects_empty' panicked at src/lib.rs:42:9:
assertion `left == right` failed
  left: 1
 right: 0
note: run with `RUST_BACKTRACE=1`</system-err>
    </testcase>
    <testcase name="tests::flaky" classname="demo" timestamp="t" time="0.02">
      <flakyFailure type="test failure">first attempt failed</flakyFailure>
    </testcase>
    <testcase name="tests::later" classname="demo" timestamp="t" time="0">
      <skipped/>
    </testcase>
  </testsuite>
</testsuites>"#;

    #[test]
    fn parses_counts_and_failures() {
        let s = parse_junit(REPORT).unwrap();
        assert_eq!(s.tests, 4);
        assert_eq!(s.failures, 1);
        assert_eq!(s.skipped, 1);
        assert_eq!(s.passed(), 2);
        assert_eq!(s.failed.len(), 1);
        assert_eq!(s.failed[0].suite, "demo");
        assert_eq!(s.failed[0].name, "tests::rejects_empty");
        assert!(
            s.failed[0]
                .detail
                .starts_with("thread 'tests::rejects_empty' panicked at src/lib.rs:42:9")
        );
        assert!(s.failed[0].detail.contains("right: 0"));
    }

    #[test]
    fn error_element_counts_as_failure() {
        let xml = r#"<testsuites><testsuite name="s"><testcase name="t"><error message="timed out"/></testcase></testsuite></testsuites>"#;
        let s = parse_junit(xml).unwrap();
        assert_eq!(s.failures, 1);
        assert_eq!(s.failed[0].detail, "timed out");
    }

    #[test]
    fn rejects_invalid_xml() {
        assert!(parse_junit("<testsuites>").is_err());
    }

    #[test]
    fn model_summary_lists_failures_and_caps_them() {
        let mut s = parse_junit(REPORT).unwrap();
        let text = format_for_model(&s, 5);
        assert!(text.starts_with("tests: 4 run, 2 passed, 1 failed, 1 skipped"));
        assert!(text.contains("FAILED demo :: tests::rejects_empty"));

        s.failed.push(s.failed[0].clone());
        s.failed.push(s.failed[0].clone());
        let capped = format_for_model(&s, 1);
        assert_eq!(capped.matches("FAILED ").count(), 1);
        assert!(capped.contains("and 2 more failed tests"));
    }

    #[test]
    fn long_details_are_truncated_on_char_boundaries() {
        let long = "é".repeat(DETAIL_CHARS + 50);
        let d = failure_detail(&long, "");
        assert_eq!(d.chars().count(), DETAIL_CHARS + 2);
        assert!(d.ends_with(" …"));
    }

    #[test]
    fn tool_selection_honors_setting_and_availability() {
        assert_eq!(TestTool::select(None, true), TestTool::Nextest);
        assert_eq!(TestTool::select(None, false), TestTool::CargoTest);
        assert_eq!(TestTool::select(Some("auto"), false), TestTool::CargoTest);
        assert_eq!(TestTool::select(Some("cargo"), true), TestTool::CargoTest);
        // Forcing nextest does not fall back when it is missing; the run fails instead.
        assert_eq!(TestTool::select(Some("NEXTEST"), false), TestTool::Nextest);
    }

    #[test]
    fn nextest_args_include_profile_and_filter() {
        let args = nextest_args(Some("  gates "));
        assert_eq!(&args[..3], &["nextest", "run", "--offline"]);
        assert!(args.windows(2).any(|w| w == ["--profile", "forge"]));
        assert_eq!(args.last().unwrap(), "gates");
        assert!(!nextest_args(Some("  ")).contains(&"".to_string()));
    }
}
