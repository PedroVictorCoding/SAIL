//! Lexical retrieval over stored source fragments.
//!
//! The author's context builder never reads a whole book: it asks for the handful of
//! fragments that actually mention an objective. This module is the deterministic, model-free
//! half of that: a BM25 index over fragment text and chapter titles, plus a small cache keyed
//! by `(source_id, revision)` so one index is not rebuilt for every objective.
//!
//! Deliberate limits:
//! - No embeddings and no model calls. The same fragments and the same query always return
//!   the same ranking, on every machine.
//! - Definition, Example and Theorem fragments and `Clean` fragments are preferred, because
//!   they are the kinds a lesson can ground a claim in.
//! - Scanned or otherwise unvalidated text is scored down, but not hidden: a caller can still
//!   see why a source has no clean grounding.

use crate::factory::contracts::{
    FragmentId, FragmentKind, QualityStatus, SourceFragment, SourceId,
};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Largest number of fragments one index will hold. Extraction caps a source well below this;
/// the cap here bounds memory if hostile input ever reaches the index.
pub const MAX_INDEXED_FRAGMENTS: usize = 20_000;

/// Longest token kept while indexing. Longer alphanumeric runs (a base64 blob, for example)
/// are truncated rather than allowed to grow the vocabulary without bound.
pub const MAX_TOKEN_CHARS: usize = 64;

/// Tokens per indexed fragment. A single fragment is a paragraph or a few pages at most.
pub const MAX_TOKENS_PER_FRAGMENT: usize = 4_096;

/// Tokens accepted from one query. Objective statements are much shorter.
pub const MAX_QUERY_TOKENS: usize = 256;

/// How many fragments a context builder normally wants per objective.
pub const DEFAULT_TOP_K: usize = 6;

/// Hard upper bound on `top_k`, so a caller cannot ask for the whole source.
pub const MAX_TOP_K: usize = 20;

/// BM25 term-frequency saturation.
const BM25_K1: f64 = 1.2;

/// BM25 document-length normalisation.
const BM25_B: f64 = 0.75;

/// Multiplier for fragment kinds that usually carry a definition or a worked example.
const KIND_BOOST: f64 = 1.15;

/// Multiplier for text the extractor marked `Clean`.
const CLEAN_BOOST: f64 = 1.10;

/// Multiplier for text that must not ground a published claim yet.
const NEEDS_VALIDATION_PENALTY: f64 = 0.60;

/// One document in the index: the fragment plus its pre-computed term frequencies.
struct IndexedDoc {
    fragment: SourceFragment,
    token_count: usize,
    term_frequency: BTreeMap<String, u32>,
}

/// A fragment returned by a search, with the score that ranked it.
#[derive(Debug, Clone, PartialEq)]
pub struct RetrievedFragment {
    pub fragment: SourceFragment,
    pub score: f64,
}

/// A read-only BM25 index over the fragments of exactly one source revision.
///
/// Construction sorts by `fragment_id` before applying [`MAX_INDEXED_FRAGMENTS`], so the
/// indexed set does not depend on the order in which extraction happened to hand them over.
pub struct FragmentIndex {
    source_id: SourceId,
    revision: u64,
    docs: Vec<IndexedDoc>,
    document_frequency: BTreeMap<String, u32>,
    average_length: f64,
}

impl FragmentIndex {
    /// Builds an index from a fragment slice, keeping only `source_id` at `revision`.
    ///
    /// The caller may pass fragments from several sources; the extra ones are ignored. That
    /// keeps the cache lookup honest (`(source_id, revision)` names exactly one index).
    pub fn build(source_id: &str, revision: u64, fragments: &[SourceFragment]) -> Self {
        let mut selected: Vec<&SourceFragment> = fragments
            .iter()
            .filter(|f| f.source_id == source_id && f.source_revision == revision)
            .collect();
        selected.sort_by(|a, b| a.fragment_id.cmp(&b.fragment_id));
        selected.truncate(MAX_INDEXED_FRAGMENTS);

        let mut document_frequency: BTreeMap<String, u32> = BTreeMap::new();
        let mut docs = Vec::with_capacity(selected.len());
        for fragment in selected {
            let mut text = String::with_capacity(fragment.text.len() + 64);
            text.push_str(&fragment.text);
            text.push(' ');
            text.push_str(&fragment.chapter_path.join(" "));
            let tokens = tokenize(&text, MAX_TOKENS_PER_FRAGMENT);
            let token_count = tokens.len();
            let mut term_frequency: BTreeMap<String, u32> = BTreeMap::new();
            for token in tokens {
                *term_frequency.entry(token).or_insert(0) += 1;
            }
            for term in term_frequency.keys() {
                *document_frequency.entry(term.clone()).or_insert(0) += 1;
            }
            docs.push(IndexedDoc {
                fragment: fragment.clone(),
                token_count,
                term_frequency,
            });
        }
        let total: usize = docs.iter().map(|d| d.token_count).sum();
        let average_length = if docs.is_empty() {
            1.0
        } else {
            total as f64 / docs.len() as f64
        };
        Self {
            source_id: source_id.to_owned(),
            revision,
            docs,
            document_frequency,
            average_length,
        }
    }

    /// The source this index belongs to.
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    /// The source revision this index belongs to.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Number of indexed fragments.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// Whether the index has no fragments (an empty or fully filtered source).
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Ranks fragments against `query` and returns at most `k` of them.
    ///
    /// `k` is clamped to [`MAX_TOP_K`]. Ties are broken by `fragment_id`, so a ranking is
    /// stable even when two fragments have identical scores.
    pub fn search(&self, query: &str, k: usize) -> Vec<RetrievedFragment> {
        let k = k.min(MAX_TOP_K);
        if k == 0 || self.docs.is_empty() {
            return Vec::new();
        }
        let mut query_terms: BTreeMap<String, u32> = BTreeMap::new();
        for token in tokenize(query, MAX_QUERY_TOKENS) {
            *query_terms.entry(token).or_insert(0) += 1;
        }
        if query_terms.is_empty() {
            return Vec::new();
        }
        let document_count = self.docs.len() as f64;
        let mut hits: Vec<RetrievedFragment> = self
            .docs
            .iter()
            .filter_map(|doc| {
                let mut score = 0.0;
                for (term, query_frequency) in &query_terms {
                    let Some(&frequency) = doc.term_frequency.get(term) else {
                        continue;
                    };
                    let document_frequency =
                        self.document_frequency.get(term).copied().unwrap_or(0) as f64;
                    let idf = (1.0
                        + (document_count - document_frequency + 0.5) / (document_frequency + 0.5))
                        .ln();
                    let frequency = frequency as f64;
                    let length_ratio = doc.token_count as f64 / self.average_length;
                    let denominator = frequency + BM25_K1 * (1.0 - BM25_B + BM25_B * length_ratio);
                    score += idf
                        * (frequency * (BM25_K1 + 1.0) / denominator)
                        * f64::from(*query_frequency);
                }
                if score <= 0.0 {
                    return None;
                }
                Some(RetrievedFragment {
                    score: score * preference_boost(&doc.fragment),
                    fragment: doc.fragment.clone(),
                })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.fragment.fragment_id.cmp(&b.fragment.fragment_id))
        });
        hits.truncate(k);
        hits
    }
}

/// A cache of indexes, keyed by source revision.
///
/// The authoring pipeline builds a context for one objective at a time; without the cache it
/// would re-tokenise the same source for every objective. Entries are immutable, so sharing
/// them across tasks is safe.
#[derive(Default)]
pub struct RetrievalCache {
    entries: Mutex<BTreeMap<(SourceId, u64), Arc<FragmentIndex>>>,
}

impl RetrievalCache {
    /// An empty cache. Each factory process owns one.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached index for `(source_id, revision)`, building it from `fragments`
    /// the first time.
    ///
    /// If a different `source_id`/`revision` appears in `fragments`, it is filtered out by
    /// [`FragmentIndex::build`]; the cache key still names exactly the index that is stored.
    pub fn index(
        &self,
        source_id: &str,
        revision: u64,
        fragments: &[SourceFragment],
    ) -> Arc<FragmentIndex> {
        let key = (source_id.to_owned(), revision);
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(index) = entries.get(&key) {
            return Arc::clone(index);
        }
        let index = Arc::new(FragmentIndex::build(source_id, revision, fragments));
        entries.insert(key, Arc::clone(&index));
        index
    }

    /// Drops every cached revision of one source, after the owner replaces or removes it.
    pub fn invalidate_source(&self, source_id: &str) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.retain(|(cached, _), _| cached != source_id);
    }

    /// Drops every cached index.
    pub fn clear(&self) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.clear();
    }

    /// Number of cached revisions, for tests and diagnostics.
    pub fn cached_revisions(&self) -> usize {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.len()
    }
}

/// The small deterministic preference described in the module docs.
fn preference_boost(fragment: &SourceFragment) -> f64 {
    let mut boost = 1.0;
    if matches!(
        fragment.kind,
        FragmentKind::Definition | FragmentKind::Example | FragmentKind::Theorem
    ) {
        boost *= KIND_BOOST;
    }
    match fragment.quality {
        QualityStatus::Clean => boost *= CLEAN_BOOST,
        QualityStatus::NeedsValidation { .. } => boost *= NEEDS_VALIDATION_PENALTY,
        QualityStatus::LowConfidence { .. } => {}
    }
    boost
}

/// Lowercase alphanumeric tokens. Punctuation, markup and whitespace are separators, and the
/// token count is capped so hostile input cannot make one document expensive.
fn tokenize(text: &str, limit: usize) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            if current.len() < MAX_TOKEN_CHARS {
                current.extend(character.to_lowercase());
            }
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
            if tokens.len() >= limit {
                return tokens;
            }
        }
    }
    if !current.is_empty() && tokens.len() < limit {
        tokens.push(current);
    }
    tokens
}

/// A fragment id is not a score; this helper keeps test code readable.
pub type ScoredFragmentId = (FragmentId, f64);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factory::contracts::SourceLocation;

    fn fragment(
        id: &str,
        text: &str,
        kind: FragmentKind,
        quality: QualityStatus,
    ) -> SourceFragment {
        SourceFragment {
            fragment_id: id.to_owned(),
            source_id: "src-rust".to_owned(),
            source_revision: 1,
            kind,
            chapter_path: vec!["Ownership".to_owned()],
            location: SourceLocation::Lines { first: 1, last: 5 },
            text: text.to_owned(),
            content_hash: format!("hash-{id}"),
            quality,
        }
    }

    fn doc(id: &str, text: &str) -> SourceFragment {
        fragment(id, text, FragmentKind::Prose, QualityStatus::Clean)
    }

    #[test]
    fn bm25_ranks_the_matching_fragment_first() {
        let fragments = vec![
            doc(
                "f-borrow",
                "Borrowing lets a function read a value without taking ownership.",
            ),
            doc(
                "f-move",
                "A move transfers ownership and the old binding can no longer be used.",
            ),
            doc("f-copy", "Copy types are duplicated instead of moved."),
        ];
        let index = FragmentIndex::build("src-rust", 1, &fragments);
        let hits = index.search("ownership move binding", 2);
        assert_eq!(hits[0].fragment.fragment_id, "f-move");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn ranking_is_independent_of_input_order() {
        let mut fragments = vec![
            doc("f-a", "ownership and borrowing"),
            doc("f-b", "ownership"),
            doc("f-c", "borrowing"),
        ];
        let forward = FragmentIndex::build("src-rust", 1, &fragments);
        fragments.reverse();
        let backward = FragmentIndex::build("src-rust", 1, &fragments);
        let first: Vec<ScoredFragmentId> = forward
            .search("ownership", 3)
            .into_iter()
            .map(|h| (h.fragment.fragment_id, h.score))
            .collect();
        let second: Vec<ScoredFragmentId> = backward
            .search("ownership", 3)
            .into_iter()
            .map(|h| (h.fragment.fragment_id, h.score))
            .collect();
        assert_eq!(first, second);
    }

    #[test]
    fn definitions_and_clean_text_are_preferred_at_equal_relevance() {
        let fragments = vec![
            fragment(
                "f-prose",
                "A lifetime is a scope in which a reference is valid.",
                FragmentKind::Prose,
                QualityStatus::Clean,
            ),
            fragment(
                "f-definition",
                "A lifetime is a scope in which a reference is valid.",
                FragmentKind::Definition,
                QualityStatus::Clean,
            ),
        ];
        let index = FragmentIndex::build("src-rust", 1, &fragments);
        let hits = index.search("lifetime scope reference", 2);
        assert_eq!(hits[0].fragment.fragment_id, "f-definition");
    }

    #[test]
    fn unvalidated_text_is_ranked_below_clean_text() {
        let fragments = vec![
            fragment(
                "f-scan",
                "The formula for kinetic energy is one half m v squared.",
                FragmentKind::Formula,
                QualityStatus::NeedsValidation {
                    reason: "scanned".to_owned(),
                },
            ),
            fragment(
                "f-clean",
                "The formula for kinetic energy is one half m v squared.",
                FragmentKind::Formula,
                QualityStatus::Clean,
            ),
        ];
        let index = FragmentIndex::build("src-rust", 1, &fragments);
        let hits = index.search("kinetic energy formula", 2);
        assert_eq!(hits[0].fragment.fragment_id, "f-clean");
    }

    #[test]
    fn cache_returns_the_same_index_for_the_same_revision() {
        let cache = RetrievalCache::new();
        let fragments = vec![doc("f-a", "ownership")];
        let first = cache.index("src-rust", 1, &fragments);
        let second = cache.index("src-rust", 1, &fragments);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(cache.cached_revisions(), 1);

        let next = cache.index("src-rust", 2, &[]);
        assert!(!Arc::ptr_eq(&first, &next));
        assert_eq!(cache.cached_revisions(), 2);

        cache.invalidate_source("src-rust");
        assert_eq!(cache.cached_revisions(), 0);
    }

    #[test]
    fn search_is_clamped_to_the_top_k_cap() {
        let fragments: Vec<SourceFragment> = (0..40)
            .map(|n| doc(&format!("f-{n:02}"), "ownership"))
            .collect();
        let index = FragmentIndex::build("src-rust", 1, &fragments);
        assert_eq!(index.search("ownership", 1_000).len(), MAX_TOP_K);
        assert!(index.search("ownership", 0).is_empty());
        assert!(index.search("", 5).is_empty());
    }

    #[test]
    fn index_ignores_other_revisions_and_sources() {
        let mut other = doc("f-other", "ownership");
        other.source_revision = 2;
        let mut foreign = doc("f-foreign", "ownership");
        foreign.source_id = "src-other".to_owned();
        let index =
            FragmentIndex::build("src-rust", 1, &[doc("f-own", "ownership"), other, foreign]);
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn empty_index_is_safe_and_identified() {
        let index = FragmentIndex::build("src-rust", 1, &[]);
        assert!(index.is_empty());
        assert_eq!(index.source_id(), "src-rust");
        assert_eq!(index.revision(), 1);
    }
}
