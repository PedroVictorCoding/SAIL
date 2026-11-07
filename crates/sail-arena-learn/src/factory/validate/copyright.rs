//! Copyright validator (`CheckKind::Copyright`): enforces [`SourceRights::max_quote_words`]
//! by computing the longest common normalized word run between all learner-visible draft text
//! and each cited source fragment.
//!
//! Uses a linear-time Suffix Automaton over word token IDs (`O(N + M)` time, where `N` is the
//! draft word count and `M` is the total word count across all cited fragments), staying well
//! under a few milliseconds on 50 fragments of 4 KB each.

use super::make_receipt;
use crate::courses::content::Step;
use crate::factory::contracts::{
    CheckKind, DraftValidator, FragmentLookup, LessonDraft, SourceRights, ValidationContext,
    ValidationReceipt, Verdict,
};
use std::collections::{BTreeMap, BTreeSet};

/// Version tag stamped on receipts produced by [`CopyrightValidator`].
pub const VALIDATOR_VERSION: &str = "copyright/1";

/// Maximum number of normalized words indexed from a single lesson draft.
pub const MAX_DRAFT_WORDS: usize = 32_768;

/// Maximum number of normalized words scanned per source fragment.
pub const MAX_FRAGMENT_WORDS: usize = 16_384;

/// Maximum number of cited fragments checked per draft.
pub const MAX_CITED_FRAGMENTS: usize = 256;

/// Maximum number of words from an offending verbatim run included in a failure message.
pub const MAX_REPORTED_QUOTE_WORDS: usize = 8;

/// Checks that no cited fragment shares a contiguous run of normalized words longer than
/// its source's [`SourceRights::max_quote_words`].
#[derive(Debug, Default, Clone, Copy)]
pub struct CopyrightValidator;

#[async_trait::async_trait]
impl DraftValidator for CopyrightValidator {
    fn check(&self) -> CheckKind {
        CheckKind::Copyright
    }

    async fn validate(
        &self,
        draft: &LessonDraft,
        ctx: &ValidationContext<'_>,
    ) -> Vec<ValidationReceipt> {
        let reasons = validate_copyright(draft, ctx.fragments);
        let verdict = if reasons.is_empty() {
            Verdict::Pass
        } else {
            Verdict::Fail { reasons }
        };
        vec![make_receipt(
            draft,
            CheckKind::Copyright,
            None,
            verdict,
            VALIDATOR_VERSION,
            ctx.now_ms,
        )]
    }
}

/// Checks learner-visible text in `draft` against every cited fragment in `fragments`.
///
/// Returns failure messages (each with the offending quote truncated to
/// [`MAX_REPORTED_QUOTE_WORDS`] words) for any fragment whose longest shared word run
/// exceeds `rights.max_quote_words()`.
pub fn validate_copyright(draft: &LessonDraft, fragments: &dyn FragmentLookup) -> Vec<String> {
    let mut errors = Vec::new();
    let visible_words = collect_learner_visible_words(draft);
    if visible_words.is_empty() {
        return errors;
    }
    if visible_words.len() > MAX_DRAFT_WORDS {
        errors.push(format!(
            "draft learner-visible word count {} exceeds cap of {MAX_DRAFT_WORDS}",
            visible_words.len()
        ));
        return errors;
    }

    let sam = WordSuffixAutomaton::build(&visible_words);

    let mut cited_ids = Vec::new();
    let mut seen = BTreeSet::new();
    for claim in &draft.output.claims {
        for fid in &claim.fragment_ids {
            if seen.insert(fid.as_str()) {
                cited_ids.push(fid.as_str());
            }
        }
    }

    if cited_ids.len() > MAX_CITED_FRAGMENTS {
        errors.push(format!(
            "draft cites {} distinct fragments, exceeding cap of {MAX_CITED_FRAGMENTS}",
            cited_ids.len()
        ));
    }

    for frag_id in cited_ids.into_iter().take(MAX_CITED_FRAGMENTS) {
        let Some(fragment) = fragments.fragment(frag_id) else {
            continue;
        };
        let rights = fragments
            .source(&fragment.source_id)
            .map(|s| s.rights)
            .unwrap_or(SourceRights::Unknown);
        let max_words = rights.max_quote_words();

        let frag_words = normalize_words(&fragment.text);
        if frag_words.len() > MAX_FRAGMENT_WORDS {
            errors.push(format!(
                "fragment `{frag_id}` has {} words, exceeding cap of {MAX_FRAGMENT_WORDS}",
                frag_words.len()
            ));
            continue;
        }

        let run = sam.longest_common_run(&frag_words);
        if run.len() > max_words {
            let truncated: Vec<&str> = run
                .iter()
                .take(MAX_REPORTED_QUOTE_WORDS)
                .map(String::as_str)
                .collect();
            let snippet = if run.len() > MAX_REPORTED_QUOTE_WORDS {
                format!("{} ...", truncated.join(" "))
            } else {
                truncated.join(" ")
            };
            errors.push(format!(
                "fragment `{frag_id}` (source `{}`): verbatim run of {} words exceeds limit of {max_words} words: \"{snippet}\"",
                fragment.source_id,
                run.len()
            ));
        }
    }

    errors
}

/// Normalizes text into lowercase alphanumeric word tokens, stripping punctuation and
/// collapsing whitespace.
pub fn normalize_words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                current.push(lower);
            }
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Computes the longest contiguous run of normalized words shared between `a` and `b`.
pub fn longest_common_word_run(a: &str, b: &str) -> Vec<String> {
    let a_words = normalize_words(a);
    let b_words = normalize_words(b);
    if a_words.is_empty() || b_words.is_empty() {
        return Vec::new();
    }
    let sam = WordSuffixAutomaton::build(&a_words);
    sam.longest_common_run(&b_words)
}

fn collect_learner_visible_words(draft: &LessonDraft) -> Vec<String> {
    let mut out = Vec::new();
    let lesson = &draft.output.lesson;
    out.extend(normalize_words(&lesson.title));

    for step in lesson
        .steps
        .iter()
        .chain(draft.output.review_variants.iter())
    {
        append_step_words(step, &mut out);
    }
    out
}

fn append_step_words(step: &Step, out: &mut Vec<String>) {
    out.extend(normalize_words(&step.title));
    for block in &step.body {
        out.extend(normalize_words(&block.text));
    }
    if let Some(code) = &step.code {
        out.extend(normalize_words(code));
    }
    if let Some(tmpl) = &step.template {
        out.extend(normalize_words(tmpl));
    }
    if let Some(ph) = &step.placeholder {
        out.extend(normalize_words(ph));
    }
    if let Some(starter) = &step.starter {
        out.extend(normalize_words(starter));
    }
    for opt in &step.options {
        out.extend(normalize_words(&opt.text));
    }
    for item in &step.items {
        out.extend(normalize_words(&item.text));
    }
    for hint in &step.hints {
        out.extend(normalize_words(hint));
    }
    for misc in step.misconceptions.values() {
        out.extend(normalize_words(misc));
    }
    for block in &step.correct {
        out.extend(normalize_words(&block.text));
    }
    for block in &step.solution {
        out.extend(normalize_words(&block.text));
    }
}

#[derive(Debug, Clone)]
struct SamState {
    len: usize,
    link: Option<usize>,
    next: BTreeMap<u32, usize>,
}

/// Word-level Suffix Automaton for linear-time longest common contiguous word-run queries.
#[derive(Debug, Clone)]
struct WordSuffixAutomaton {
    vocab: BTreeMap<String, u32>,
    states: Vec<SamState>,
    last: usize,
}

impl WordSuffixAutomaton {
    fn build(words: &[String]) -> Self {
        let mut sam = Self {
            vocab: BTreeMap::new(),
            states: Vec::with_capacity(words.len().saturating_mul(2).max(1)),
            last: 0,
        };
        sam.states.push(SamState {
            len: 0,
            link: None,
            next: BTreeMap::new(),
        });

        let mut next_id = 1u32;
        for word in words.iter().take(MAX_DRAFT_WORDS) {
            let id = match sam.vocab.get(word.as_str()) {
                Some(&existing) => existing,
                None => {
                    let assigned = next_id;
                    next_id = next_id.saturating_add(1);
                    sam.vocab.insert(word.clone(), assigned);
                    assigned
                }
            };
            sam.extend(id);
        }
        sam
    }

    fn extend(&mut self, token: u32) {
        let cur = self.states.len();
        let cur_len = self.states[self.last].len + 1;
        self.states.push(SamState {
            len: cur_len,
            link: None,
            next: BTreeMap::new(),
        });

        let mut p = Some(self.last);
        while let Some(u) = p {
            if self.states[u].next.contains_key(&token) {
                break;
            }
            self.states[u].next.insert(token, cur);
            p = self.states[u].link;
        }

        let Some(p_idx) = p else {
            self.states[cur].link = Some(0);
            self.last = cur;
            return;
        };

        let q_idx = self.states[p_idx].next[&token];
        if self.states[p_idx].len + 1 == self.states[q_idx].len {
            self.states[cur].link = Some(q_idx);
        } else {
            let clone = self.states.len();
            let cloned_state = SamState {
                len: self.states[p_idx].len + 1,
                link: self.states[q_idx].link,
                next: self.states[q_idx].next.clone(),
            };
            self.states.push(cloned_state);

            let mut walk = Some(p_idx);
            while let Some(u) = walk {
                if self.states[u].next.get(&token) == Some(&q_idx) {
                    self.states[u].next.insert(token, clone);
                    walk = self.states[u].link;
                } else {
                    break;
                }
            }
            self.states[q_idx].link = Some(clone);
            self.states[cur].link = Some(clone);
        }

        self.last = cur;
    }

    fn longest_common_run(&self, query_words: &[String]) -> Vec<String> {
        let mut state = 0usize;
        let mut cur_len = 0usize;
        let mut best_len = 0usize;
        let mut best_end = 0usize;

        for (idx, word) in query_words.iter().take(MAX_FRAGMENT_WORDS).enumerate() {
            let Some(&token) = self.vocab.get(word.as_str()) else {
                state = 0;
                cur_len = 0;
                continue;
            };
            while state != 0 && !self.states[state].next.contains_key(&token) {
                state = self.states[state].link.unwrap_or(0);
                cur_len = self.states[state].len;
            }
            if let Some(&next_state) = self.states[state].next.get(&token) {
                state = next_state;
                cur_len += 1;
            } else {
                state = 0;
                cur_len = 0;
            }
            if cur_len > best_len {
                best_len = cur_len;
                best_end = idx;
            }
        }

        if best_len == 0 {
            return Vec::new();
        }
        let start = best_end + 1 - best_len;
        query_words[start..=best_end].to_vec()
    }
}
