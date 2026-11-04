//! Learner questions (#236). A question is a model tool call whose real result arrives
//! later, when the learner answers. Select kinds are graded here by option id; free recall
//! goes back to the model for judgment.
use super::{Invalid, MAX_SHORT, MAX_TEXT, bounded};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    /// Exactly one correct option.
    Single,
    /// One or more correct options; the learner must pick exactly that set.
    Multiple,
    /// Free-text answer judged by the model.
    FreeRecall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionOption {
    pub id: String,
    pub text: String,
}

/// What the model asks. `correct` holds option ids and is never sent to the learner's view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionSpec {
    pub kind: QuestionKind,
    pub prompt: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub correct: Vec<String>,
    /// Plan node the question checks; required for `quiz`, absent for probes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

impl QuestionSpec {
    pub fn validate(&self) -> Result<(), Invalid> {
        bounded("prompt", &self.prompt, MAX_TEXT)?;
        match self.kind {
            QuestionKind::FreeRecall => {
                if !self.options.is_empty() || !self.correct.is_empty() {
                    return Err(Invalid::new(
                        "a free_recall question takes no `options` or `correct`",
                    ));
                }
            }
            QuestionKind::Single | QuestionKind::Multiple => {
                if !(2..=6).contains(&self.options.len()) {
                    return Err(Invalid::new(format!(
                        "a {} question needs 2 to 6 `options`, got {}",
                        if self.kind == QuestionKind::Single {
                            "single"
                        } else {
                            "multiple"
                        },
                        self.options.len()
                    )));
                }
                let mut ids = BTreeSet::new();
                for option in &self.options {
                    bounded("options[].id", &option.id, 40)?;
                    bounded("options[].text", &option.text, MAX_SHORT * 2)?;
                    if !ids.insert(option.id.as_str()) {
                        return Err(Invalid::new(format!(
                            "option id `{}` is used twice",
                            option.id
                        )));
                    }
                }
                let correct: BTreeSet<&str> = self.correct.iter().map(String::as_str).collect();
                if let Some(unknown) = correct.iter().find(|id| !ids.contains(*id)) {
                    return Err(Invalid::new(format!(
                        "`correct` lists `{unknown}`, which is not an option id"
                    )));
                }
                let expected = if self.kind == QuestionKind::Single {
                    correct.len() == 1
                } else {
                    !correct.is_empty()
                };
                if !expected || correct.len() != self.correct.len() {
                    return Err(Invalid::new(if self.kind == QuestionKind::Single {
                        "a single question needs exactly one distinct id in `correct`"
                    } else {
                        "a multiple question needs at least one distinct id in `correct`"
                    }));
                }
            }
        }
        Ok(())
    }

    pub fn grade(&self, answer: &Answer) -> Result<Grade, Invalid> {
        match self.kind {
            QuestionKind::FreeRecall => {
                let text = answer.text.as_deref().unwrap_or_default();
                bounded("text", text, MAX_TEXT)?;
                Ok(Grade::NeedsJudgment)
            }
            QuestionKind::Single | QuestionKind::Multiple => {
                let chosen: BTreeSet<&str> = answer.choices.iter().map(String::as_str).collect();
                if chosen.is_empty() || chosen.len() != answer.choices.len() {
                    return Err(Invalid::new("choose at least one option, each once"));
                }
                if self.kind == QuestionKind::Single && chosen.len() != 1 {
                    return Err(Invalid::new("choose exactly one option"));
                }
                if chosen
                    .iter()
                    .any(|c| !self.options.iter().any(|o| o.id == *c))
                {
                    return Err(Invalid::new("an answer refers to an unknown option"));
                }
                let correct: BTreeSet<&str> = self.correct.iter().map(String::as_str).collect();
                Ok(if chosen == correct {
                    Grade::Correct
                } else {
                    Grade::Incorrect
                })
            }
        }
    }
}

/// The learner's answer. `confidence` (1–5) and `thinking` are signals, not proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

impl Answer {
    pub fn validate(&self) -> Result<(), Invalid> {
        if let Some(c) = self.confidence
            && !(1..=5).contains(&c)
        {
            return Err(Invalid::new("`confidence` must be 1 to 5"));
        }
        if let Some(t) = &self.thinking
            && t.len() > MAX_TEXT
        {
            return Err(Invalid::new("`thinking` is too long"));
        }
        if self.choices.len() > 6 {
            return Err(Invalid::new("too many choices"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grade {
    Correct,
    Incorrect,
    /// Free recall: the model judges it on the next turn.
    NeedsJudgment,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn select(kind: QuestionKind, correct: &[&str]) -> QuestionSpec {
        QuestionSpec {
            kind,
            prompt: "Which?".into(),
            options: ["a", "b", "c"]
                .iter()
                .map(|id| QuestionOption {
                    id: id.to_string(),
                    text: format!("Option {id}"),
                })
                .collect(),
            correct: correct.iter().map(|s| s.to_string()).collect(),
            node_id: None,
        }
    }

    fn choose(ids: &[&str]) -> Answer {
        Answer {
            choices: ids.iter().map(|s| s.to_string()).collect(),
            ..Answer::default()
        }
    }

    #[test]
    fn validation_rejects_malformed_questions() {
        select(QuestionKind::Single, &["a"]).validate().unwrap();
        select(QuestionKind::Multiple, &["a", "c"])
            .validate()
            .unwrap();
        for (spec, expected) in [
            (select(QuestionKind::Single, &["a", "b"]), "exactly one"),
            (select(QuestionKind::Single, &[]), "exactly one"),
            (select(QuestionKind::Multiple, &["a", "a"]), "distinct"),
            (select(QuestionKind::Single, &["z"]), "not an option id"),
        ] {
            assert!(spec.validate().unwrap_err().0.contains(expected));
        }
        let mut one = select(QuestionKind::Single, &["a"]);
        one.options.truncate(1);
        assert!(one.validate().unwrap_err().0.contains("2 to 6"));
        let mut dup = select(QuestionKind::Single, &["a"]);
        dup.options[1].id = "a".into();
        assert!(dup.validate().unwrap_err().0.contains("used twice"));
        let mut free = select(QuestionKind::FreeRecall, &[]);
        assert!(free.validate().is_err());
        free.options.clear();
        free.validate().unwrap();
    }

    #[test]
    fn select_kinds_are_graded_by_id() {
        let single = select(QuestionKind::Single, &["b"]);
        assert_eq!(single.grade(&choose(&["b"])).unwrap(), Grade::Correct);
        assert_eq!(single.grade(&choose(&["a"])).unwrap(), Grade::Incorrect);
        assert!(single.grade(&choose(&["a", "b"])).is_err());
        assert!(single.grade(&choose(&["z"])).is_err());
        let multiple = select(QuestionKind::Multiple, &["a", "c"]);
        assert_eq!(
            multiple.grade(&choose(&["c", "a"])).unwrap(),
            Grade::Correct
        );
        assert_eq!(multiple.grade(&choose(&["a"])).unwrap(), Grade::Incorrect);
        assert!(multiple.grade(&choose(&["a", "a"])).is_err());
    }

    #[test]
    fn free_recall_needs_judgment_and_text() {
        let mut free = select(QuestionKind::FreeRecall, &[]);
        free.options.clear();
        let answer = Answer {
            text: Some("Because the window halves".into()),
            ..Answer::default()
        };
        assert_eq!(free.grade(&answer).unwrap(), Grade::NeedsJudgment);
        assert!(free.grade(&Answer::default()).is_err());
    }

    #[test]
    fn answer_signals_are_bounded() {
        let mut answer = choose(&["a"]);
        answer.confidence = Some(6);
        assert!(answer.validate().is_err());
        answer.confidence = Some(3);
        answer.validate().unwrap();
    }
}
