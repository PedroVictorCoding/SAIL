//! Free-recall judgment (#243). A free-recall question carries a short rubric written by the
//! tutor and never shown to the learner. Before the tutor replies, a separate judge call
//! compares the learner's answer with that rubric and returns a verdict plus the specific
//! missing relation. An `unclear` answer gets a follow-up question, not a verdict. Output
//! that does not parse is retried once and is then recorded as "could not judge". It is
//! never guessed.
use super::{Invalid, MAX_SHORT, MAX_TEXT, bounded};
use serde::{Deserialize, Serialize};

/// Bound on a rubric, in bytes. A rubric names one to three ideas; it is not a lesson.
pub const MAX_RUBRIC: usize = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Correct,
    Partial,
    Incorrect,
    /// Not enough to decide. Leads to a follow-up question, never to a verdict.
    Unclear,
    /// The judge produced no usable output after the retry. Nothing was guessed.
    CouldNotJudge,
}

/// The stored result of judging one free-recall answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Judgment {
    pub verdict: Verdict,
    /// The specific idea or relation the answer is missing or gets wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing: Option<String>,
    /// One or two sentences of reasoning about the answer, addressed to the learner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    /// For `unclear`: the follow-up question the learner is asked next.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<String>,
    /// Judge calls used (1 or 2).
    pub attempts: u8,
}

impl Judgment {
    pub fn could_not_judge(attempts: u8) -> Self {
        Self {
            verdict: Verdict::CouldNotJudge,
            missing: None,
            feedback: None,
            follow_up: None,
            attempts,
        }
    }
}

/// What the judge model must return, as tool arguments or as one JSON object in text.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeOutput {
    pub verdict: JudgeVerdict,
    #[serde(default)]
    pub missing: Option<String>,
    #[serde(default)]
    pub feedback: Option<String>,
    #[serde(default)]
    pub follow_up: Option<String>,
}

/// The verdicts a judge may give. `could_not_judge` is the engine's, not the model's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgeVerdict {
    Correct,
    Partial,
    Incorrect,
    Unclear,
}

fn clean(field: &str, value: Option<String>, max: usize) -> Result<Option<String>, Invalid> {
    match value {
        Some(text) if !text.trim().is_empty() => {
            bounded(field, &text, max)?;
            Ok(Some(text.trim().to_string()))
        }
        _ => Ok(None),
    }
}

impl JudgeOutput {
    /// Checks the output and turns it into a stored judgment.
    pub fn into_judgment(self, attempts: u8) -> Result<Judgment, Invalid> {
        let missing = clean("missing", self.missing, MAX_SHORT * 2)?;
        let feedback = clean("feedback", self.feedback, MAX_SHORT * 2)?;
        let follow_up = clean("follow_up", self.follow_up, MAX_TEXT)?;
        let verdict = match self.verdict {
            JudgeVerdict::Correct => Verdict::Correct,
            JudgeVerdict::Partial => Verdict::Partial,
            JudgeVerdict::Incorrect => Verdict::Incorrect,
            JudgeVerdict::Unclear => Verdict::Unclear,
        };
        match verdict {
            Verdict::Partial | Verdict::Incorrect if missing.is_none() => {
                return Err(Invalid::new(
                    "`missing` is required for partial and incorrect: name the specific missing or wrong idea",
                ));
            }
            Verdict::Unclear if follow_up.is_none() => {
                return Err(Invalid::new(
                    "`follow_up` is required for unclear: one short question that would decide it",
                ));
            }
            _ => {}
        }
        Ok(Judgment {
            verdict,
            missing,
            feedback,
            // A follow-up only makes sense when no verdict was given.
            follow_up: follow_up.filter(|_| verdict == Verdict::Unclear),
            attempts,
        })
    }
}

/// Finds one JSON object in free text: the whole text, a fenced block, or the outermost
/// `{…}`. Small models often answer in text even when a tool is offered.
pub fn json_object_in(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (end > start).then(|| &trimmed[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(verdict: JudgeVerdict) -> JudgeOutput {
        JudgeOutput {
            verdict,
            missing: None,
            feedback: None,
            follow_up: None,
        }
    }

    #[test]
    fn partial_and_incorrect_must_name_the_missing_relation() {
        for verdict in [JudgeVerdict::Partial, JudgeVerdict::Incorrect] {
            let error = output(verdict).into_judgment(1).unwrap_err();
            assert!(error.0.contains("`missing` is required"));
            let mut blank = output(verdict);
            blank.missing = Some("   ".into());
            assert!(blank.into_judgment(1).is_err());
            let mut named = output(verdict);
            named.missing = Some(" loss → smaller window ".into());
            let judgment = named.into_judgment(2).unwrap();
            assert_eq!(judgment.missing.as_deref(), Some("loss → smaller window"));
            assert_eq!(judgment.attempts, 2);
        }
        output(JudgeVerdict::Correct).into_judgment(1).unwrap();
    }

    #[test]
    fn unclear_needs_a_follow_up_and_only_unclear_keeps_one() {
        assert!(
            output(JudgeVerdict::Unclear)
                .into_judgment(1)
                .unwrap_err()
                .0
                .contains("follow_up")
        );
        let mut unclear = output(JudgeVerdict::Unclear);
        unclear.follow_up = Some("What happens to the window after a loss?".into());
        assert!(unclear.into_judgment(1).unwrap().follow_up.is_some());
        let mut correct = output(JudgeVerdict::Correct);
        correct.follow_up = Some("stray".into());
        assert!(correct.into_judgment(1).unwrap().follow_up.is_none());
    }

    #[test]
    fn oversized_fields_are_rejected() {
        let mut long = output(JudgeVerdict::Incorrect);
        long.missing = Some("x".repeat(MAX_SHORT * 2 + 1));
        assert!(long.into_judgment(1).unwrap_err().0.contains("too long"));
    }

    #[test]
    fn engine_only_verdict_cannot_come_from_the_model() {
        let parsed: Result<JudgeOutput, _> =
            serde_json::from_str(r#"{"verdict":"could_not_judge"}"#);
        assert!(parsed.is_err());
        let parsed: Result<JudgeOutput, _> =
            serde_json::from_str(r#"{"verdict":"correct","score":10}"#);
        assert!(parsed.is_err(), "unknown fields are refused");
    }

    #[test]
    fn finds_a_json_object_in_text() {
        assert_eq!(json_object_in(" {\"a\":1} "), Some("{\"a\":1}"));
        assert_eq!(
            json_object_in("Verdict:\n```json\n{\"a\":{\"b\":2}}\n```"),
            Some("{\"a\":{\"b\":2}}")
        );
        assert_eq!(json_object_in("no json here"), None);
        assert_eq!(json_object_in("} backwards {"), None);
    }
}
