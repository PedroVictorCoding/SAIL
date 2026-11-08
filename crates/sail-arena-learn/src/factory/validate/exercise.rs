//! Exercise-tests validator (`CheckKind::ExerciseTests`): compiles and runs each
//! [`RustFixture`]'s `reference_solution` and `known_bad` against `hidden_tests` in the
//! isolated [`RustEvaluator`] sandbox.
//!
//! Because `sail-learn-rust-check` compiles with `rustc --edition 2021` and executes `main`,
//! the validator appends a generated `fn main()` that invokes every `fn hidden_*()` function
//! in `hidden_tests` and then prints [`HIDDEN_TESTS_PASSED`].
//! - `reference_solution` must compile, exit 0, and print [`HIDDEN_TESTS_PASSED`].
//! - `known_bad` must **not** print [`HIDDEN_TESTS_PASSED`] (proving the tests bite).
//! - [`RustRun::Unavailable`] always maps to [`Verdict::Unavailable`], never [`Verdict::Pass`].

use super::make_receipt;
use crate::courses::content::{RunLimits, Step, StepKind};
use crate::courses::engine::RustEvaluator;
use crate::courses::grade::{MAX_CODE, RustRun};
use crate::factory::contracts::{
    CheckKind, DraftValidator, HIDDEN_TESTS_PASSED, LessonDraft, RustFixture, ValidationContext,
    ValidationReceipt, Verdict,
};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Version tag stamped on receipts produced by [`ExerciseTestsValidator`].
pub const VALIDATOR_VERSION: &str = "exercise_tests/1";

/// Maximum number of `fn hidden_*` test functions permitted in one [`RustFixture`].
pub const MAX_HIDDEN_TESTS: usize = 64;

/// Default sandbox limits when a step does not specify its own [`RunLimits`].
pub const DEFAULT_LIMITS: RunLimits = RunLimits {
    wall_ms: 20_000,
    mem_mb: 512,
};

/// Validates [`RustFixture`]s by running `reference_solution` and `known_bad` against
/// `hidden_tests` inside the injected [`RustEvaluator`].
pub struct ExerciseTestsValidator {
    evaluator: Arc<dyn RustEvaluator>,
}

impl ExerciseTestsValidator {
    /// Creates a new [`ExerciseTestsValidator`] backed by `evaluator`.
    pub fn new(evaluator: Arc<dyn RustEvaluator>) -> Self {
        Self { evaluator }
    }
}

#[async_trait::async_trait]
impl DraftValidator for ExerciseTestsValidator {
    fn check(&self) -> CheckKind {
        CheckKind::ExerciseTests
    }

    async fn validate(
        &self,
        draft: &LessonDraft,
        ctx: &ValidationContext<'_>,
    ) -> Vec<ValidationReceipt> {
        let all_steps: Vec<&Step> = draft
            .output
            .lesson
            .steps
            .iter()
            .chain(draft.output.review_variants.iter())
            .collect();

        let mut receipts = Vec::new();
        let mut covered_steps = BTreeSet::new();

        for fixture in &draft.output.rust {
            covered_steps.insert(fixture.step_id.as_str());
            let step = all_steps.iter().copied().find(|s| s.id == fixture.step_id);
            let verdict = self.validate_one_fixture(fixture, step).await;
            receipts.push(make_receipt(
                draft,
                CheckKind::ExerciseTests,
                Some(fixture.step_id.clone()),
                verdict,
                VALIDATOR_VERSION,
                ctx.now_ms,
            ));
        }

        // Any RustEdit step without a corresponding RustFixture fails validation.
        for step in all_steps {
            if step.kind == StepKind::RustEdit && !covered_steps.contains(step.id.as_str()) {
                receipts.push(make_receipt(
                    draft,
                    CheckKind::ExerciseTests,
                    Some(step.id.clone()),
                    Verdict::Fail {
                        reasons: vec![format!(
                            "rust_edit step `{}` has no RustFixture in draft.output.rust",
                            step.id
                        )],
                    },
                    VALIDATOR_VERSION,
                    ctx.now_ms,
                ));
            }
        }

        if receipts.is_empty() {
            receipts.push(make_receipt(
                draft,
                CheckKind::ExerciseTests,
                None,
                Verdict::Pass,
                VALIDATOR_VERSION,
                ctx.now_ms,
            ));
        }

        receipts
    }
}

impl ExerciseTestsValidator {
    async fn validate_one_fixture(&self, fixture: &RustFixture, step: Option<&Step>) -> Verdict {
        let mut static_errors = validate_rust_fixture_static(fixture, step);
        if !static_errors.is_empty() {
            return Verdict::Fail {
                reasons: static_errors,
            };
        }

        let ref_program =
            match compose_rust_test_program(&fixture.reference_solution, &fixture.hidden_tests) {
                Ok(src) => src,
                Err(err) => {
                    static_errors.push(format!("step `{}`: {err}", fixture.step_id));
                    return Verdict::Fail {
                        reasons: static_errors,
                    };
                }
            };

        let known_bad_src = fixture.known_bad.as_deref().unwrap_or("");
        let bad_program = match compose_rust_test_program(known_bad_src, &fixture.hidden_tests) {
            Ok(src) => src,
            Err(err) => {
                return Verdict::Fail {
                    reasons: vec![format!("step `{}` known_bad: {err}", fixture.step_id)],
                };
            }
        };

        let limits = step.and_then(|s| s.limits).unwrap_or(DEFAULT_LIMITS);

        // 1. Run `reference_solution + hidden_tests + main`: must compile and print sentinel.
        let ref_run = self.evaluator.run(&ref_program, limits).await;
        match ref_run {
            RustRun::Unavailable(reason) => {
                return Verdict::Unavailable { reason };
            }
            RustRun::CompileFailed { diagnostics } => {
                let diag_summary: Vec<String> = diagnostics
                    .iter()
                    .take(5)
                    .map(|d| format!("line {}:{}: {}", d.line, d.column, d.message))
                    .collect();
                return Verdict::Fail {
                    reasons: vec![format!(
                        "step `{}`: reference_solution failed to compile: {}",
                        fixture.step_id,
                        if diag_summary.is_empty() {
                            "compiler rejected source".to_string()
                        } else {
                            diag_summary.join("; ")
                        }
                    )],
                };
            }
            RustRun::Compiled {
                exit_code, stdout, ..
            } => {
                if exit_code != Some(0) || !stdout_contains_sentinel(&stdout) {
                    return Verdict::Fail {
                        reasons: vec![format!(
                            "step `{}`: reference_solution did not pass hidden tests (exit_code={exit_code:?}, sentinel_present={})",
                            fixture.step_id,
                            stdout_contains_sentinel(&stdout)
                        )],
                    };
                }
            }
        }

        // 2. Run `known_bad + hidden_tests + main`: must NOT print sentinel.
        let bad_run = self.evaluator.run(&bad_program, limits).await;
        match bad_run {
            RustRun::Unavailable(reason) => {
                return Verdict::Unavailable { reason };
            }
            RustRun::CompileFailed { .. } => {
                // A `known_bad` that doesn't compile proves nothing about the hidden tests: a
                // typo would "pass" this check. It must compile and then fail a test.
                return Verdict::Fail {
                    reasons: vec![format!(
                        "step `{}`: known_bad doesn't compile, so it can't show that the hidden tests catch a wrong answer",
                        fixture.step_id
                    )],
                };
            }
            RustRun::Compiled { stdout, .. } => {
                if stdout_contains_sentinel(&stdout) {
                    return Verdict::Fail {
                        reasons: vec![format!(
                            "step `{}`: known_bad passed all hidden tests and printed `{HIDDEN_TESTS_PASSED}`; hidden tests do not bite",
                            fixture.step_id
                        )],
                    };
                }
            }
        }

        Verdict::Pass
    }
}

/// Checks static defence-in-depth rules on a [`RustFixture`] before sandbox execution.
pub fn validate_rust_fixture_static(fixture: &RustFixture, step: Option<&Step>) -> Vec<String> {
    let mut errors = Vec::new();
    let ctx = format!("RustFixture `{}`", fixture.step_id);

    if let Some(s) = step {
        if s.kind != StepKind::RustEdit {
            errors.push(format!(
                "{ctx}: target step has kind `{:?}`, expected `RustEdit`",
                s.kind
            ));
        }
        if let Some(starter) = &s.starter {
            check_forbidden_rust_constructs(starter, &format!("{ctx}.starter"), false, &mut errors);
        }
    }

    if fixture.edition != "2021" {
        errors.push(format!(
            "{ctx}: edition must be `\"2021\"`, found `{}`",
            fixture.edition
        ));
    }

    if fixture.reference_solution.trim().is_empty() {
        errors.push(format!("{ctx}: reference_solution cannot be empty"));
    } else if fixture.reference_solution.len() > MAX_CODE {
        errors.push(format!(
            "{ctx}: reference_solution length {} exceeds {MAX_CODE} bytes",
            fixture.reference_solution.len()
        ));
    }

    if fixture.hidden_tests.trim().is_empty() {
        errors.push(format!("{ctx}: hidden_tests cannot be empty"));
    } else if fixture.hidden_tests.len() > MAX_CODE {
        errors.push(format!(
            "{ctx}: hidden_tests length {} exceeds {MAX_CODE} bytes",
            fixture.hidden_tests.len()
        ));
    }

    match &fixture.known_bad {
        None => errors.push(format!(
            "{ctx}: known_bad is required to prove hidden tests bite"
        )),
        Some(bad) if bad.trim().is_empty() => {
            errors.push(format!("{ctx}: known_bad cannot be empty"));
        }
        Some(bad) if bad.len() > MAX_CODE => {
            errors.push(format!(
                "{ctx}: known_bad length {} exceeds {MAX_CODE} bytes",
                bad.len()
            ));
        }
        Some(bad) => {
            check_forbidden_rust_constructs(bad, &format!("{ctx}.known_bad"), true, &mut errors);
        }
    }

    check_forbidden_rust_constructs(
        &fixture.reference_solution,
        &format!("{ctx}.reference_solution"),
        true,
        &mut errors,
    );
    check_forbidden_rust_constructs(
        &fixture.hidden_tests,
        &format!("{ctx}.hidden_tests"),
        true,
        &mut errors,
    );

    let test_fns = extract_hidden_test_names(&fixture.hidden_tests);
    if test_fns.is_empty() {
        errors.push(format!(
            "{ctx}: hidden_tests must define at least one `fn hidden_*()` function"
        ));
    } else if test_fns.len() > MAX_HIDDEN_TESTS {
        errors.push(format!(
            "{ctx}: hidden_tests defines {} test functions, exceeding cap of {MAX_HIDDEN_TESTS}",
            test_fns.len()
        ));
    }

    errors
}

/// Composes `program_under_test`, `hidden_tests`, and a generated `fn main()` that calls
/// every `fn hidden_*()` in source order before printing [`HIDDEN_TESTS_PASSED`].
pub fn compose_rust_test_program(
    program_under_test: &str,
    hidden_tests: &str,
) -> Result<String, String> {
    if contains_fn_main(program_under_test) {
        return Err("program under test must not define `fn main`".to_string());
    }
    if contains_fn_main(hidden_tests) {
        return Err("hidden_tests must not define `fn main`".to_string());
    }
    let test_names = extract_hidden_test_names(hidden_tests);
    if test_names.is_empty() {
        return Err("hidden_tests contains no `fn hidden_*()` functions".to_string());
    }
    if test_names.len() > MAX_HIDDEN_TESTS {
        return Err(format!(
            "hidden_tests exceeds maximum of {MAX_HIDDEN_TESTS} test functions"
        ));
    }

    let mut out = String::with_capacity(
        program_under_test.len() + hidden_tests.len() + test_names.len() * 32 + 96,
    );
    out.push_str(program_under_test);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(hidden_tests);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\nfn main() {\n");
    for name in &test_names {
        out.push_str("    ");
        out.push_str(name);
        out.push_str("();\n");
    }
    out.push_str("    println!(\"");
    out.push_str(HIDDEN_TESTS_PASSED);
    out.push_str("\");\n}\n");

    Ok(out)
}

/// Extracts all `fn hidden_*` function identifiers defined in `hidden_tests`, in source order.
pub fn extract_hidden_test_names(hidden_tests: &str) -> Vec<String> {
    let stripped = strip_comments_and_strings(hidden_tests);
    let tokens = rust_ident_tokens(&stripped);
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();

    for window in tokens.windows(2) {
        if window[0] == "fn"
            && window[1].starts_with("hidden_")
            && window[1].len() > "hidden_".len()
            && seen.insert(window[1].clone())
        {
            names.push(window[1].clone());
        }
    }
    names
}

fn check_forbidden_rust_constructs(
    source: &str,
    field: &str,
    forbid_fn_main: bool,
    errors: &mut Vec<String>,
) {
    if source.contains(HIDDEN_TESTS_PASSED) {
        errors.push(format!(
            "{field}: must not contain sentinel `{HIDDEN_TESTS_PASSED}`"
        ));
    }

    let stripped = strip_comments_and_strings(source);
    let tokens = rust_ident_tokens(&stripped);

    if forbid_fn_main && contains_fn_main_tokens(&tokens) {
        errors.push(format!("{field}: must not define `fn main`"));
    }

    if tokens.iter().any(|t| t == "unsafe") {
        errors.push(format!("{field}: `unsafe` is forbidden in fixtures"));
    }

    for forbidden in [
        "std::process",
        "process::Command",
        "std::net",
        "std::fs",
        "include!",
        "include_str!",
        "include_bytes!",
        "env!",
        "option_env!",
        "std::env",
    ] {
        // Normalize whitespace around `::` and `!` so `std :: fs` or `include !` is also caught.
        let compact: String = stripped.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains(forbidden) {
            errors.push(format!("{field}: forbidden construct `{forbidden}`"));
        }
    }
}

fn contains_fn_main(source: &str) -> bool {
    let stripped = strip_comments_and_strings(source);
    let tokens = rust_ident_tokens(&stripped);
    contains_fn_main_tokens(&tokens)
}

fn contains_fn_main_tokens(tokens: &[String]) -> bool {
    tokens.windows(2).any(|w| w[0] == "fn" && w[1] == "main")
}

/// Replaces comments (`// ...`, `/* ... */`) and string literals (`"..."`) with spaces so
/// token scans do not trigger on comments or string text, while keeping raw source checks
/// for sentinel spoofing and macro invocations.
fn strip_comments_and_strings(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '/' {
            if chars.peek() == Some(&'/') {
                chars.next();
                for nc in chars.by_ref() {
                    if nc == '\n' {
                        out.push('\n');
                        break;
                    }
                }
                continue;
            } else if chars.peek() == Some(&'*') {
                chars.next();
                let mut depth = 1usize;
                while depth > 0 {
                    match chars.next() {
                        Some('/') if chars.peek() == Some(&'*') => {
                            chars.next();
                            depth += 1;
                        }
                        Some('*') if chars.peek() == Some(&'/') => {
                            chars.next();
                            depth -= 1;
                        }
                        Some('\n') => out.push('\n'),
                        Some(_) => {}
                        None => break,
                    }
                }
                out.push(' ');
                continue;
            }
        } else if c == '"' {
            out.push(' ');
            while let Some(sc) = chars.next() {
                if sc == '\\' {
                    chars.next();
                } else if sc == '"' {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn rust_ident_tokens(src: &str) -> Vec<String> {
    src.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn stdout_contains_sentinel(stdout: &str) -> bool {
    stdout
        .lines()
        .any(|line| line.trim() == HIDDEN_TESTS_PASSED)
}
