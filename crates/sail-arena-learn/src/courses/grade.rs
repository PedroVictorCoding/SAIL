//! Deterministic graders (#386). No model, no IO: the step's own key and the observed tool
//! output decide the result. Rust results come from [`RustRun`], which the sandbox adapter
//! produces. A sandbox that couldn't run yields `unavailable`, never `incorrect`.
use super::content::{AnswerKey, Block, BlockKind, Step, StepKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A learner's answer. Externally tagged: `{"choice":[..]}`, `{"order":[..]}`,
/// `{"text":".."}` or `{"code":".."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Answer {
    Choice(Vec<String>),
    Order(Vec<String>),
    Text(String),
    Code(String),
}

/// Longest learner code accepted, in bytes. It also has to fit the runner's argv limit.
pub const MAX_CODE: usize = 16 * 1024;
pub const MAX_TEXT_ANSWER: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Correct,
    Incorrect,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidatorKind {
    Choice,
    Order,
    Text,
    Rustc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub line: u32,
    pub column: u32,
    pub level: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feedback {
    pub summary: String,
    pub explanation: Vec<Block>,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graded {
    pub verdict: Verdict,
    pub validator: ValidatorKind,
    pub feedback: Feedback,
}

/// What the sandbox observed for one submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustRun {
    Compiled {
        exit_code: Option<i32>,
        stdout: String,
        diagnostics: Vec<Diagnostic>,
    },
    CompileFailed {
        diagnostics: Vec<Diagnostic>,
    },
    /// The sandbox wasn't configured, refused the job, timed out or crashed.
    Unavailable(String),
}

/// Trims, drops all whitespace and trailing `;`, so `& mut` matches `&mut`. Case is kept:
/// in code it matters.
pub fn normalize(text: &str) -> String {
    let squeezed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    squeezed.trim_end_matches(';').to_string()
}

fn feedback(summary: impl Into<String>, explanation: Vec<Block>) -> Feedback {
    Feedback {
        summary: summary.into(),
        explanation,
        diagnostics: Vec::new(),
        stdout: None,
        unavailable_reason: None,
    }
}

/// Grades every kind except `rust_edit` (see [`grade_rust`]). `Err` means the answer's shape
/// doesn't fit the step; that is a client error, not a wrong answer.
pub fn grade(step: &Step, answer: &Answer) -> Result<Graded, String> {
    let key = step.key.as_ref().ok_or("this step takes no answer")?;
    let (correct, validator) = match (step.kind, key, answer) {
        (StepKind::Choice | StepKind::Predict, AnswerKey::Choice(k), Answer::Choice(given)) => {
            let ids: BTreeSet<&str> = step.options.iter().map(|o| o.id.as_str()).collect();
            if given.is_empty() || given.iter().any(|g| !ids.contains(g.as_str())) {
                return Err("choose one or more of the listed options".into());
            }
            if !step.multi && given.len() > 1 {
                return Err("choose exactly one option".into());
            }
            let want: BTreeSet<&str> = k.iter().map(String::as_str).collect();
            let got: BTreeSet<&str> = given.iter().map(String::as_str).collect();
            (want == got, super::grade::ValidatorKind::Choice)
        }
        (StepKind::Order, AnswerKey::Order(k), Answer::Order(given)) => {
            let want: BTreeSet<&str> = k.iter().map(String::as_str).collect();
            let got: BTreeSet<&str> = given.iter().map(String::as_str).collect();
            if given.len() != k.len() || want != got {
                return Err("submit every item exactly once".into());
            }
            (given == k, ValidatorKind::Order)
        }
        (StepKind::Fill, AnswerKey::Text(accepted), Answer::Text(given)) => {
            if given.len() > MAX_TEXT_ANSWER {
                return Err(format!("the answer is longer than {MAX_TEXT_ANSWER} bytes"));
            }
            let given = normalize(given);
            (
                !given.is_empty() && accepted.iter().any(|a| normalize(a) == given),
                ValidatorKind::Text,
            )
        }
        _ => return Err("this answer type doesn't match the step".into()),
    };
    if correct {
        return Ok(Graded {
            verdict: Verdict::Correct,
            validator,
            feedback: feedback("Correct.", step.correct.clone()),
        });
    }
    let mut explanation = Vec::new();
    if let Answer::Choice(given) = answer {
        for g in given {
            if let Some(text) = step.misconceptions.get(g) {
                explanation.push(Block::p(text.clone()));
            }
        }
    }
    Ok(Graded {
        verdict: Verdict::Incorrect,
        validator,
        feedback: feedback("Not this time.", explanation),
    })
}

/// Grades a sandbox run against the step's expected stdout.
pub fn grade_rust(step: &Step, run: &RustRun) -> Result<Graded, String> {
    let Some(AnswerKey::Rust { stdout: expected }) = &step.key else {
        return Err("this step isn't a Rust edit".into());
    };
    let (verdict, mut fb) = match run {
        RustRun::Unavailable(reason) => {
            let mut fb = feedback(
                "Evaluation unavailable. This attempt isn't scored as right or wrong.",
                Vec::new(),
            );
            fb.unavailable_reason = Some(reason.clone());
            (Verdict::Unavailable, fb)
        }
        RustRun::CompileFailed { diagnostics } => {
            let mut fb = feedback(
                "It doesn't compile yet. Read the first error.",
                Vec::new(),
            );
            fb.diagnostics = diagnostics.clone();
            (Verdict::Incorrect, fb)
        }
        RustRun::Compiled {
            exit_code,
            stdout,
            diagnostics,
        } => {
            let ok = *exit_code == Some(0) && stdout.trim_end() == expected.trim_end();
            let mut fb = if ok {
                feedback("It compiles and prints the expected output.", step.correct.clone())
            } else if *exit_code != Some(0) {
                feedback(
                    "It compiles, but the program didn't exit successfully.",
                    Vec::new(),
                )
            } else {
                feedback(
                    "It compiles, but the output isn't what the task asks for.",
                    vec![Block {
                        kind: BlockKind::Code,
                        text: format!("expected: {expected}"),
                        lang: Some("text".into()),
                    }],
                )
            };
            fb.diagnostics = diagnostics.clone();
            fb.stdout = Some(stdout.clone());
            (
                if ok {
                    Verdict::Correct
                } else {
                    Verdict::Incorrect
                },
                fb,
            )
        }
    };
    fb.diagnostics.truncate(20);
    Ok(Graded {
        verdict,
        validator: ValidatorKind::Rustc,
        feedback: fb,
    })
}

/// Parses `rustc --error-format=json` lines into line-mapped diagnostics. It skips
/// span-less summaries such as "aborting due to 1 previous error" and non-JSON lines.
pub fn parse_rustc_json(stderr: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for line in stderr.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(spans) = v["spans"].as_array() else {
            continue;
        };
        let Some(span) = spans
            .iter()
            .find(|s| s["is_primary"].as_bool() == Some(true))
            .or(spans.first())
        else {
            continue;
        };
        let code = v["code"]["code"].as_str();
        let message = v["message"].as_str().unwrap_or_default();
        out.push(Diagnostic {
            line: span["line_start"].as_u64().unwrap_or(0) as u32,
            column: span["column_start"].as_u64().unwrap_or(0) as u32,
            level: v["level"].as_str().unwrap_or("error").to_string(),
            message: match code {
                Some(c) => format!("[{c}] {message}"),
                None => message.to_string(),
            },
        });
    }
    out
}
