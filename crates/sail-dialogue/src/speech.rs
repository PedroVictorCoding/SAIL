//! Commit speech at prosodic boundaries, never at arbitrary model-token boundaries.

const CONTROL_MARKERS: [&str; 4] = ["<|tool_call", "<tool_call|>", "<|tool_response", "<|\"|>"];
#[derive(Default)]
pub struct SpeechTextGate {
    pending: String,
    blocked: bool,
}
impl SpeechTextGate {
    pub fn push(&mut self, delta: &str) -> String {
        if self.blocked {
            return String::new();
        }
        self.pending.push_str(delta);
        if let Some(index) = CONTROL_MARKERS
            .iter()
            .filter_map(|marker| self.pending.find(marker))
            .min()
        {
            self.blocked = true;
            let safe = self.pending[..index].to_owned();
            self.pending.clear();
            return safe;
        }
        // Retain only a possible split control-token prefix. Ordinary prose streams immediately.
        let keep = self
            .pending
            .char_indices()
            .filter_map(|(index, _)| {
                let suffix = &self.pending[index..];
                CONTROL_MARKERS
                    .iter()
                    .any(|marker| marker.starts_with(suffix))
                    .then_some(index)
            })
            .min()
            .unwrap_or(self.pending.len());
        let safe = self.pending[..keep].to_owned();
        self.pending.drain(..keep);
        safe
    }
    pub fn finish(&mut self) -> String {
        if self.pending.starts_with("<|") || self.pending.starts_with("<tool_") {
            self.blocked = true;
            self.pending.clear();
        }
        std::mem::take(&mut self.pending)
    }
    pub fn blocked(&self) -> bool {
        self.blocked
    }
}

/// Words required before the first spoken segment may be committed. Kept small so audio
/// starts after the first short sentence instead of waiting for a long run of text.
pub const DEFAULT_FIRST_SEGMENT_MIN_WORDS: usize = 4;
/// Steady-state word minimum per segment once playback is under way. Later segments are
/// synthesized while earlier ones play, so they can be longer (fewer render seams, better
/// prosody) without adding latency.
pub const DEFAULT_MIN_WORDS_PER_SEGMENT: usize = 20;

/// Splits streamed model text into speakable segments at prosodic boundaries.
///
/// The per-segment word minimum ramps up: the first segment needs only
/// `first_min_words`, and each following segment doubles that until `min_words`.
/// For example, the defaults give 4, 8, 16, 20, 20, ... words.
/// That keeps time-to-first-audio low and still avoids stop/start renders between
/// many short sentences.
#[derive(Debug, Clone)]
pub struct SpeechChunker {
    pending: String,
    first_min_words: usize,
    min_words: usize,
    emitted: usize,
}
fn env_words(name: &str) -> Option<usize> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}
impl Default for SpeechChunker {
    fn default() -> Self {
        let min_words = env_words("SAIL_VOICE_MIN_WORDS").unwrap_or(DEFAULT_MIN_WORDS_PER_SEGMENT);
        let first_min_words =
            env_words("SAIL_VOICE_FIRST_MIN_WORDS").unwrap_or(DEFAULT_FIRST_SEGMENT_MIN_WORDS);
        Self::with_ramp(first_min_words, min_words)
    }
}
impl SpeechChunker {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }
    /// A flat minimum for every segment (0 disables the minimum).
    #[allow(dead_code)]
    pub fn with_min_words(min_words: usize) -> Self {
        Self::with_ramp(min_words, min_words)
    }
    /// Start at `first_min_words` and double per segment up to `min_words`.
    pub fn with_ramp(first_min_words: usize, min_words: usize) -> Self {
        Self {
            pending: String::new(),
            first_min_words: first_min_words.min(min_words),
            min_words,
            emitted: 0,
        }
    }
    /// The steady-state (maximum) word minimum.
    #[allow(dead_code)]
    pub fn min_words(&self) -> usize {
        self.min_words
    }
    #[allow(dead_code)]
    pub fn set_min_words(&mut self, min_words: usize) {
        self.min_words = min_words;
        self.first_min_words = self.first_min_words.min(min_words);
    }
    /// Word minimum that applies to the next committed segment.
    pub fn current_min_words(&self) -> usize {
        let shift = self.emitted.min(usize::BITS as usize - 1) as u32;
        self.first_min_words
            .checked_shl(shift)
            .filter(|n| n >> shift == self.first_min_words)
            .unwrap_or(usize::MAX)
            .min(self.min_words)
    }
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.pending.push_str(text);
        self.drain(false)
    }
    pub fn finish(&mut self) -> Vec<String> {
        self.drain(true)
    }
    pub fn discard_pending(&mut self) {
        self.pending.clear();
        // Speech after an interruption starts from silence again, so ramp from the start.
        self.emitted = 0;
    }
    fn drain(&mut self, finished: bool) -> Vec<String> {
        let mut result = Vec::new();
        while let Some(end) = boundary(&self.pending, finished, self.current_min_words()) {
            let chunk = self.pending.drain(..end).collect::<String>();
            let chunk = chunk.trim().replace(['`', '*', '#'], "");
            if !chunk.trim().is_empty() {
                result.push(chunk);
                self.emitted += 1;
            }
        }
        if finished {
            let tail = std::mem::take(&mut self.pending)
                .trim()
                .replace(['`', '*', '#'], "");
            if !tail.trim().is_empty() {
                result.push(tail);
            }
        }
        result
    }
}
fn boundary(text: &str, finished: bool, min_words: usize) -> Option<usize> {
    for (index, ch) in text.char_indices() {
        let sentence = matches!(ch, '.' | '?' | '!' | '。' | '？' | '！');
        // Never split normal sentences at mid-sentence commas/clauses: neural TTS resets
        // pitch contour (F0) on each segment, so starting synthesis after a comma sounds
        // like a new sentence starting mid-thought. Only use a clause break as a safety
        // valve when an unpunctuated sentence exceeds 240 characters.
        let clause = matches!(ch, ',' | ';' | ':' | '—') && text[..index].chars().count() >= 240;
        let newline = ch == '\n' && text[..index].trim().len() >= 20;
        if !sentence && !clause && !newline {
            continue;
        }
        let mut end = index + ch.len_utf8();
        // Keep quotation marks and closing brackets with their sentence.
        for next in text[end..].chars() {
            if matches!(next, '"' | '\'' | '”' | '’' | ')' | ']') {
                end += next.len_utf8();
            } else {
                break;
            }
        }
        let next = text[end..].chars().next();
        // Unambiguous sentence endings (question, exclamation, CJK terminators, or newlines)
        // do not need to wait for a subsequent whitespace token when streaming.
        let unambiguous = matches!(ch, '?' | '!' | '。' | '？' | '！' | '\n');
        if !unambiguous {
            if next.is_none() && !finished {
                // For a period at the end of a multi-word phrase (not a single isolated token),
                // check that the phrase has at least 3 words and the final token is plain
                // alphabetic prose rather than a path, domain, version, or abbreviation.
                let words: Vec<&str> = text[..index].split_whitespace().collect();
                let plain_prose_word = words.last().is_some_and(|w| {
                    let trimmed = w.trim_matches(['"', '\'', '(', '[']);
                    trimmed.chars().count() >= 2 && trimmed.chars().all(char::is_alphabetic)
                });
                if words.len() < 3 || !plain_prose_word {
                    continue;
                }
            }
            if next.is_some_and(|c| !c.is_whitespace()) {
                continue;
            }
        }
        if ch == '.' {
            let token = text[..index]
                .split_whitespace()
                .next_back()
                .unwrap_or_default();
            let lower = token.trim_matches(['"', '\'', '(', '[']).to_lowercase();
            if matches!(
                lower.as_str(),
                "mr" | "mrs"
                    | "ms"
                    | "dr"
                    | "prof"
                    | "sr"
                    | "sra"
                    | "srta"
                    | "st"
                    | "jr"
                    | "inc"
                    | "ltd"
                    | "co"
                    | "fig"
                    | "no"
                    | "approx"
                    | "est"
                    | "dept"
                    | "e.g"
                    | "i.e"
                    | "etc"
                    | "vs"
            ) {
                continue;
            }
            if lower.chars().count() == 1 && lower.chars().all(char::is_alphabetic) {
                continue;
            }
            // Initialisms such as U.S. must not become independent spoken fragments.
            if lower.contains('.')
                && lower
                    .split('.')
                    .all(|p| p.chars().count() == 1 && p.chars().all(char::is_alphabetic))
            {
                continue;
            }
            // Preserve ordered-list prefixes: "1. First item" or digits like "3.14".
            if lower.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
        }

        // A segment needs at least `min_words` words so audio does not start and stop
        // between very short sentences. On finish, a shorter remainder is flushed whole
        // as the tail rather than split per sentence.
        if min_words > 0 {
            let word_count = text[..end].split_whitespace().count();
            if word_count < min_words {
                continue;
            }
        }

        return Some(end);
    }
    // A very long unpunctuated sentence is bounded at a complete word, not a token.
    if text.chars().count() > 360 {
        return text
            .char_indices()
            .filter(|(index, c)| (140..=300).contains(index) && c.is_whitespace())
            .map(|(index, _)| index)
            .next_back();
    }
    None
}
pub fn pause_after_ms(text: &str) -> usize {
    match text
        .trim_end_matches(|c: char| {
            c.is_whitespace() || matches!(c, '"' | '\'' | '”' | '’' | ')' | ']')
        })
        .chars()
        .next_back()
    {
        Some('.' | '?' | '!' | '。' | '？' | '！') => 140,
        Some(';' | ':') => 90,
        Some(',' | '—') => 60,
        _ => 0,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_sentences_inside_tokens_and_keeps_punctuation() {
        let mut c = SpeechChunker::with_min_words(0);
        assert_eq!(c.push("Yes. This sentence is still"), vec!["Yes."]);
        assert!(c.push(" arriving").is_empty());
        assert_eq!(c.push("! And "), vec!["This sentence is still arriving!"]);
        assert_eq!(c.finish(), vec!["And"]);
    }
    #[test]
    fn abbreviations_decimals_unicode_and_quotes_are_preserved() {
        let mut c = SpeechChunker::with_min_words(0);
        assert_eq!(
            c.push("Dr. Silva measured 3.14 volts. \"Está correto?\" Sim "),
            vec!["Dr. Silva measured 3.14 volts.", "\"Está correto?\""]
        );
        assert_eq!(c.finish(), vec!["Sim"]);
        let mut c = SpeechChunker::with_min_words(0);
        assert!(c.push("Hello.").is_empty());
        assert_eq!(c.push(" "), vec!["Hello."]);
    }
    #[test]
    fn control_tokens_split_across_deltas_are_not_displayed_or_spoken() {
        let text = "A safe sentence. <|tool_call>call:fs_query{}";
        for split in 0..=text.len() {
            if !text.is_char_boundary(split) {
                continue;
            }
            let mut gate = SpeechTextGate::default();
            let mut safe = gate.push(&text[..split]);
            safe.push_str(&gate.push(&text[split..]));
            safe.push_str(&gate.finish());
            assert_eq!(safe, "A safe sentence. ");
            assert!(gate.blocked());
        }
        let mut gate = SpeechTextGate::default();
        assert_eq!(gate.push("Normal text"), "Normal text");
        assert!(!gate.blocked());
    }
    #[test]
    fn long_clauses_and_cadence_use_natural_boundaries() {
        // Normal sentences with commas (even > 70 chars) stay intact so neural TTS
        // never starts synthesizing in the middle of a sentence after a comma.
        let normal_sentence_with_comma = "When the voice pipeline synthesizes a multi-clause sentence with commas, it waits for the full sentence terminator.";
        let mut c = SpeechChunker::with_min_words(0);
        assert_eq!(
            c.push(normal_sentence_with_comma),
            vec![normal_sentence_with_comma]
        );

        // Only an oversized unpunctuated run (>= 240 chars) uses a clause break as a safety valve.
        let clause = format!(
            "{}, and the next clause continues",
            "A complete phrase with useful context ".repeat(7).trim()
        );
        let mut c = SpeechChunker::with_min_words(0);
        let chunks = c.push(&clause);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].ends_with(','));
        assert_eq!(pause_after_ms(&chunks[0]), 60);
        assert_eq!(pause_after_ms("Really?\""), 140);
        assert_eq!(pause_after_ms("Hello."), 140);
    }
    #[test]
    fn streaming_paths_and_versions_do_not_split_mid_sentence() {
        let mut c = SpeechChunker::with_min_words(0);
        assert!(c.push("Check the file at src/server.").is_empty());
        assert_eq!(
            c.push("rs and update version v1.2 before deploying."),
            vec!["Check the file at src/server.rs and update version v1.2 before deploying."]
        );
    }
    #[test]
    fn multi_word_sentences_and_questions_emit_promptly() {
        let mut c = SpeechChunker::with_min_words(0);
        assert_eq!(
            c.push("The system is running normally."),
            vec!["The system is running normally."]
        );
        assert_eq!(
            c.push("How can I assist you?"),
            vec!["How can I assist you?"]
        );
    }
    #[test]
    fn first_segment_starts_early_and_later_segments_grow() {
        let mut c = SpeechChunker::with_ramp(4, 20);
        assert_eq!(c.min_words(), 20);
        assert_eq!(c.current_min_words(), 4);
        // A one-word opener waits for enough words, then audio starts promptly.
        assert!(c.push("Sure. ").is_empty());
        assert_eq!(
            c.push("Let me check that. "),
            vec!["Sure. Let me check that."]
        );
        // The second segment needs 8 words: a 5-word sentence is held back.
        assert_eq!(c.current_min_words(), 8);
        assert!(c.push("The disk is mostly full. ").is_empty());
        assert_eq!(
            c.push("Cargo builds use most of it. "),
            vec!["The disk is mostly full. Cargo builds use most of it."]
        );
        assert_eq!(c.current_min_words(), 16);
        c.push("Two. ");
        c.push("Three. ");
        assert_eq!(c.current_min_words(), 16);
        // A short tail always flushes when the turn finishes.
        assert_eq!(c.finish(), vec!["Two. Three."]);
    }
    #[test]
    fn ramp_caps_at_the_steady_state_minimum_and_resets_on_interruption() {
        let mut c = SpeechChunker::with_ramp(4, 20);
        let sentence = "One two three four five six seven eight nine ten eleven twelve. ";
        for _ in 0..12 {
            c.push(sentence);
        }
        assert_eq!(c.current_min_words(), 20);
        c.discard_pending();
        assert_eq!(c.current_min_words(), 4);
        // A flat minimum keeps the old behaviour; 0 disables the minimum entirely.
        assert_eq!(SpeechChunker::with_min_words(20).current_min_words(), 20);
        assert_eq!(SpeechChunker::with_min_words(0).current_min_words(), 0);
        // An oversized first minimum is clamped to the steady-state minimum.
        assert_eq!(SpeechChunker::with_ramp(50, 10).current_min_words(), 10);
    }
}
