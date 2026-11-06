//! Course packages (#386): authored, versioned, original lesson content.
//!
//! A package is JSON embedded in the binary. It holds answer keys, hints and solutions, so
//! only [`StepView`] (built by [`Step::view`]) ever leaves the server. The catalog lists
//! planned tracks without inventing lessons for them.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The most hints one step may carry (nudge → relationship → partial step → worked step).
pub const MAX_HINTS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Rust,
    Philosophy,
    Physics,
}

/// Plain text with a display role. Clients render `text` with `textContent` only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    P,
    H,
    Code,
    Note,
}

impl Block {
    pub fn p(text: impl Into<String>) -> Self {
        Self {
            kind: BlockKind::P,
            text: text.into(),
            lang: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceOption {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub code: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Explain,
    Choice,
    Predict,
    Order,
    Fill,
    RustEdit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunLimits {
    pub wall_ms: u32,
    pub mem_mb: u32,
}

/// What a correct answer is. Never serialized to a client.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AnswerKey {
    /// The exact set of option ids (order-insensitive).
    Choice(Vec<String>),
    /// The exact item order.
    Order(Vec<String>),
    /// Accepted texts after [`crate::courses::grade::normalize`].
    Text(Vec<String>),
    /// Must compile and run in the sandbox; stdout (trimmed) must equal `stdout`.
    Rust { stdout: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub kind: StepKind,
    /// Problem family for "unseen variant" checks. Explain steps have none.
    #[serde(default)]
    pub family: Option<String>,
    pub title: String,
    pub objective_ids: Vec<String>,
    #[serde(default)]
    pub confidence: bool,
    #[serde(default)]
    pub body: Vec<Block>,
    #[serde(default)]
    pub options: Vec<ChoiceOption>,
    #[serde(default)]
    pub multi: bool,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub items: Vec<ChoiceOption>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub starter: Option<String>,
    #[serde(default)]
    pub limits: Option<RunLimits>,
    #[serde(default)]
    pub key: Option<AnswerKey>,
    /// Feedback for a wrong option, keyed by option id.
    #[serde(default)]
    pub misconceptions: BTreeMap<String, String>,
    #[serde(default)]
    pub hints: Vec<String>,
    #[serde(default)]
    pub correct: Vec<Block>,
    #[serde(default)]
    pub solution: Vec<Block>,
}

/// A step as the learner may see it: no key, hints, misconceptions or solution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepView {
    pub id: String,
    pub kind: StepKind,
    pub title: String,
    pub body: Vec<Block>,
    pub objective_ids: Vec<String>,
    pub hint_count: u8,
    pub confidence: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ChoiceOption>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub multi: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ChoiceOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<RunLimits>,
}

impl Step {
    /// `seed` keys the deterministic shuffle of `order` items, so a reload shows the same order.
    pub fn view(&self, seed: &str, with_hints: bool) -> StepView {
        let mut items = self.items.clone();
        if self.kind == StepKind::Order {
            seeded_shuffle(&mut items, &format!("{seed}/{}", self.id));
        }
        StepView {
            id: self.id.clone(),
            kind: self.kind,
            title: self.title.clone(),
            body: self.body.clone(),
            objective_ids: self.objective_ids.clone(),
            hint_count: if with_hints {
                self.hints.len() as u8
            } else {
                0
            },
            confidence: self.confidence,
            options: self.options.clone(),
            multi: self.multi,
            code: self.code.clone(),
            items,
            template: self.template.clone(),
            placeholder: self.placeholder.clone(),
            starter: self.starter.clone(),
            limits: self.limits,
        }
    }

    pub fn scored(&self) -> bool {
        self.kind != StepKind::Explain
    }
}

/// FNV-1a: stable across builds and platforms, unlike `DefaultHasher`.
pub fn stable_hash(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Fisher–Yates driven by a seeded xorshift. It never returns the authored order for two
/// or more items, because the authored order is the answer.
fn seeded_shuffle<T: Clone + PartialEq>(items: &mut [T], seed: &str) {
    if items.len() < 2 {
        return;
    }
    let original = items.to_vec();
    let mut state = stable_hash(seed) | 1;
    for _ in 0..8 {
        for i in (1..items.len()).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            items.swap(i, (state % (i as u64 + 1)) as usize);
        }
        if items != original.as_slice() {
            return;
        }
    }
    items.rotate_left(1);
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Objective {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub prerequisites: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chapter {
    pub id: String,
    pub title: String,
    pub lessons: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lesson {
    pub id: String,
    pub title: String,
    pub minutes: u32,
    pub objective_ids: Vec<String>,
    #[serde(default)]
    pub prerequisites: Vec<String>,
    pub steps: Vec<Step>,
}

impl Lesson {
    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub id: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoursePackage {
    pub id: String,
    pub version: String,
    pub title: String,
    pub domain: Domain,
    pub summary: String,
    pub objectives: Vec<Objective>,
    pub chapters: Vec<Chapter>,
    pub lessons: Vec<Lesson>,
    pub diagnostic: Option<Diagnostic>,
    /// Unseen review variants, each tagged with the objectives it retests.
    #[serde(default)]
    pub reviews: Vec<Step>,
}

impl CoursePackage {
    pub fn lesson(&self, id: &str) -> Option<&Lesson> {
        self.lessons.iter().find(|l| l.id == id)
    }

    pub fn objective(&self, id: &str) -> Option<&Objective> {
        self.objectives.iter().find(|o| o.id == id)
    }

    pub fn review(&self, id: &str) -> Option<&Step> {
        self.reviews.iter().find(|s| s.id == id)
    }

    pub fn minutes(&self) -> u32 {
        self.lessons.iter().map(|l| l.minutes).sum()
    }

    /// Rejects a package whose references, keys or limits are inconsistent. Run on every
    /// embedded package in tests, so a broken package never ships.
    pub fn validate(&self) -> Result<(), String> {
        let objectives: BTreeSet<&str> = self.objectives.iter().map(|o| o.id.as_str()).collect();
        if objectives.len() != self.objectives.len() {
            return Err(format!("{}: duplicate objective id", self.id));
        }
        for o in &self.objectives {
            for p in &o.prerequisites {
                if !objectives.contains(p.as_str()) {
                    return Err(format!("objective {} needs unknown {p}", o.id));
                }
            }
        }
        let lessons: BTreeSet<&str> = self.lessons.iter().map(|l| l.id.as_str()).collect();
        if lessons.len() != self.lessons.len() {
            return Err(format!("{}: duplicate lesson id", self.id));
        }
        for c in &self.chapters {
            for l in &c.lessons {
                if !lessons.contains(l.as_str()) {
                    return Err(format!("chapter {} lists unknown lesson {l}", c.id));
                }
            }
        }
        let mut step_sets: Vec<(&str, &[Step])> = self
            .lessons
            .iter()
            .map(|l| (l.id.as_str(), l.steps.as_slice()))
            .collect();
        if let Some(d) = &self.diagnostic {
            step_sets.push((d.id.as_str(), d.steps.as_slice()));
        }
        step_sets.push(("reviews", self.reviews.as_slice()));
        let mut families: BTreeMap<&str, &str> = BTreeMap::new();
        for (owner, steps) in step_sets {
            let mut ids = BTreeSet::new();
            for s in steps {
                if !ids.insert(s.id.as_str()) {
                    return Err(format!("{owner}: duplicate step {}", s.id));
                }
                validate_step(s, &objectives).map_err(|e| format!("{owner}/{}: {e}", s.id))?;
                if let Some(f) = &s.family
                    && let Some(prev) = families.insert(f, owner)
                {
                    return Err(format!(
                        "family {f} is used in both {prev} and {owner}; review variants must be unseen"
                    ));
                }
            }
        }
        for l in &self.lessons {
            for p in &l.prerequisites {
                if !lessons.contains(p.as_str()) {
                    return Err(format!("lesson {} needs unknown {p}", l.id));
                }
            }
            for o in &l.objective_ids {
                if !objectives.contains(o.as_str()) {
                    return Err(format!("lesson {} names unknown objective {o}", l.id));
                }
                let reviewable = self.reviews.iter().any(|r| r.objective_ids.contains(o));
                if !reviewable {
                    return Err(format!("objective {o} has no review variant"));
                }
            }
        }
        Ok(())
    }
}

fn validate_step(s: &Step, objectives: &BTreeSet<&str>) -> Result<(), String> {
    if s.objective_ids.is_empty() {
        return Err("needs at least one objective".into());
    }
    if let Some(o) = s
        .objective_ids
        .iter()
        .find(|o| !objectives.contains(o.as_str()))
    {
        return Err(format!("unknown objective {o}"));
    }
    if s.hints.len() > MAX_HINTS {
        return Err(format!("at most {MAX_HINTS} hints"));
    }
    let option_ids: BTreeSet<&str> = s.options.iter().map(|o| o.id.as_str()).collect();
    let item_ids: BTreeSet<&str> = s.items.iter().map(|o| o.id.as_str()).collect();
    match (s.kind, &s.key) {
        (StepKind::Explain, None) => return Ok(()),
        (StepKind::Explain, Some(_)) => return Err("explain steps have no key".into()),
        (_, None) => return Err("scored steps need a key".into()),
        (StepKind::Choice | StepKind::Predict, Some(AnswerKey::Choice(k))) => {
            if k.is_empty() || k.iter().any(|id| !option_ids.contains(id.as_str())) {
                return Err("choice key must name existing options".into());
            }
            if k.len() > 1 && !s.multi {
                return Err("a key with several options needs `multi`".into());
            }
        }
        (StepKind::Order, Some(AnswerKey::Order(k))) => {
            let keyed: BTreeSet<&str> = k.iter().map(String::as_str).collect();
            if keyed != item_ids || k.len() != s.items.len() || k.len() < 2 {
                return Err("order key must be a permutation of the items".into());
            }
        }
        (StepKind::Fill, Some(AnswerKey::Text(k))) => {
            if k.is_empty() || !s.template.as_deref().unwrap_or("").contains("____") {
                return Err("fill needs accepted texts and a ____ template".into());
            }
        }
        (StepKind::RustEdit, Some(AnswerKey::Rust { .. })) => {
            if s.starter.is_none() || s.limits.is_none() {
                return Err("rust_edit needs a starter and limits".into());
            }
        }
        _ => return Err("key kind doesn't match the step kind".into()),
    }
    if s.family.is_none() {
        return Err("scored steps need a family".into());
    }
    if s.misconceptions
        .keys()
        .any(|k| !option_ids.contains(k.as_str()))
    {
        return Err("misconception for an unknown option".into());
    }
    Ok(())
}

/// A track in the catalog. Planned tracks are listed honestly, with no lessons.
#[derive(Debug, Clone)]
pub struct Track {
    pub id: &'static str,
    pub title: &'static str,
    pub domain: Domain,
    pub summary: &'static str,
    pub package: Option<CoursePackage>,
}

const RUST_SYSTEMS: &str = include_str!("../../courses/rust-systems.json");

/// The first-party catalog, in display order.
pub fn catalog() -> Vec<Track> {
    let rust: CoursePackage =
        serde_json::from_str(RUST_SYSTEMS).expect("embedded rust-systems course parses");
    vec![
        Track {
            id: "rust-systems",
            title: "Rust & Systems Programming",
            domain: Domain::Rust,
            summary: "Ownership, borrowing and the type system, checked by the real compiler.",
            package: Some(rust),
        },
        Track {
            id: "philosophy-logic",
            title: "Philosophy & Logic",
            domain: Domain::Philosophy,
            summary: "Arguments, validity and counterexamples. Lessons are being written.",
            package: None,
        },
        Track {
            id: "physics-engineering",
            title: "Physics, Math & Engineering",
            domain: Domain::Physics,
            summary: "Units, motion, circuits and control, with validated simulations. Lessons are being written.",
            package: None,
        },
    ]
}
